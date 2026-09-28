//! Embedded source modules are parsed once and resolved in their own namespaces.
use super::*;
use std::collections::HashMap;

mod spans;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct DefinitionScope {
    pub imports: Arc<ImportScope>,
    pub nominals: Arc<HashMap<String, Type>>,
    pub members: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImplicitImports {
    None,
    Types,
    All,
}

pub(super) struct SourceDefinition {
    pub path: &'static str,
    pub source: &'static str,
    pub reexport_imports: bool,
    pub implicit: ImplicitImports,
}

pub(super) const SOURCES: &[SourceDefinition] = &[
    SourceDefinition {
        path: "joky/option",
        source: include_str!("../../stdlib/option.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        path: "joky/result",
        source: include_str!("../../stdlib/result.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        path: "joky/integer",
        source: include_str!("../../stdlib/integer.jk"),
        reexport_imports: true,
        implicit: ImplicitImports::Types,
    },
    SourceDefinition {
        path: PRELUDE,
        source: include_str!("../../stdlib/prelude.jk"),
        reexport_imports: true,
        implicit: ImplicitImports::All,
    },
];

#[derive(Debug)]
pub(super) struct SourceModule {
    module: Module,
    pub exports: ImportScope,
    dependencies: Vec<String>,
    pub implicit: ImplicitImports,
}

#[derive(Debug)]
pub(crate) struct StandardLibrary {
    pub(super) modules: BTreeMap<String, SourceModule>,
}

impl StandardLibrary {
    pub(super) fn new() -> Self {
        Self::from_sources(SOURCES).expect("valid embedded source modules")
    }

    fn from_sources(sources: &[SourceDefinition]) -> Result<Self, Error> {
        let mut library = Self {
            modules: BTreeMap::new(),
        };
        let registry = TypeRegistry::new().with_prelude(false);
        for source in sources {
            library.load(source.path, sources, &registry, &mut Vec::new())?;
        }
        Ok(library)
    }

    fn load(
        &mut self,
        path: &str,
        sources: &[SourceDefinition],
        registry: &TypeRegistry,
        stack: &mut Vec<String>,
    ) -> Result<(), Error> {
        if self.modules.contains_key(path) {
            return Ok(());
        }
        if stack.iter().any(|parent| parent == path) {
            let mut cycle = stack.clone();
            cycle.push(path.into());
            return Err(error(
                Span::empty(0),
                ErrorCode::InvalidImport,
                format!("Cyclic source module dependency: {}", cycle.join(" -> ")),
            ));
        }
        if stack.len() >= 128 {
            return Err(error(
                Span::empty(0),
                ErrorCode::ResourceLimit,
                "Source module dependency depth exceeds 128 levels",
            ));
        }
        let source = sources
            .iter()
            .find(|source| source.path == path)
            .ok_or_else(|| {
                error(
                    Span::empty(0),
                    ErrorCode::InvalidImport,
                    format!("Unknown embedded source module '{path}'"),
                )
            })?;
        stack.push(path.into());
        let mut module = crate::parser::parse_library(source.source)?;
        // Source modules never inherit the consumer's implicit prelude or hosts.
        for import in &module.imports {
            self.load(&import.path, sources, registry, stack)?;
        }
        let (mut scope, required) = bind(&module, registry, &self.modules)?;
        let mut exports = ImportScope::default();
        if source.reexport_imports {
            for member in module.imports.iter().flat_map(|import| &import.members) {
                let name = member.alias.as_ref().unwrap_or(&member.name);
                if let Some(binding) = scope
                    .types
                    .get(name)
                    .filter(|_| member.kind == ImportMemberKind::Type)
                {
                    exports.types.insert(name.clone(), binding.clone());
                } else if let Some(binding) = scope.constructors.get(name) {
                    exports.constructors.insert(name.clone(), binding.clone());
                } else if let Some(binding) = scope.functions.get(name) {
                    exports.functions.insert(name.clone(), binding.clone());
                }
            }
        }
        for declaration in &mut module.enums {
            let name = declaration.name.clone();
            declaration.name = format!("{path}.{name}");
            let binding = TypeBinding {
                name: declaration.name.clone(),
                declaration: Some(declaration.id),
            };
            scope.types.insert(name.clone(), binding.clone());
            exports.types.insert(name, binding.clone());
            for variant in &declaration.variants {
                let constructor = ConstructorBinding {
                    enumeration: binding.clone(),
                    variant: variant.name.clone(),
                };
                if exports
                    .constructors
                    .insert(variant.name.clone(), constructor.clone())
                    .is_some()
                {
                    return Err(error(
                        variant.span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate exported constructor '{}'", variant.name),
                    ));
                }
                scope.constructors.insert(variant.name.clone(), constructor);
            }
        }
        for declaration in &mut module.records {
            let name = declaration.name.clone();
            declaration.name = format!("{path}.{name}");
            let binding = TypeBinding {
                name: declaration.name.clone(),
                declaration: Some(declaration.id),
            };
            scope.types.insert(name.clone(), binding.clone());
            exports.types.insert(name, binding);
        }
        for item in &mut module.items {
            let Item::FnDef(function) = item else {
                return Err(error(
                    Span::empty(0),
                    ErrorCode::InvalidImport,
                    "Embedded source modules cannot declare host functions",
                ));
            };
            let name = function.name.clone();
            function.name = format!("{path}.{name}");
            if exports.constructors.contains_key(&name)
                || exports
                    .functions
                    .insert(name.clone(), function.name.clone())
                    .is_some()
            {
                return Err(error(
                    function.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate exported value '{name}'"),
                ));
            }
            scope.functions.insert(name, function.name.clone());
            scope
                .functions
                .insert(function.name.clone(), function.name.clone());
        }
        for implementation in &mut module.implementations {
            if let Some(binding) = scope.types.get(&implementation.target_name) {
                implementation.target_name.clone_from(&binding.name);
            }
        }
        let own = (
            module.enums.len(),
            module.records.len(),
            module.items.len(),
            module.implementations.len(),
        );
        module.scope = Some(Arc::new(scope));
        self.append(&mut module, &required);
        // A bound scope suppresses expansion, including while this cache is built.
        let mut module = registry.resolve_aliases(&module)?;
        let definition_scope = Arc::new(DefinitionScope {
            imports: module.scope.clone().unwrap(),
            nominals: Arc::new(crate::core::types::nominal_types(registry, &module)?),
            members: module
                .implementations
                .iter()
                .flat_map(|implementation| {
                    implementation
                        .methods
                        .iter()
                        .map(|method| &method.name)
                        .chain(
                            implementation
                                .constants
                                .iter()
                                .map(|constant| &constant.name),
                        )
                        .map(|name| format!("{}.{name}", implementation.target))
                })
                .collect(),
        });
        module.enums.truncate(own.0);
        module.records.truncate(own.1);
        module.items.truncate(own.2);
        module.implementations.truncate(own.3);
        module.imports.clear();
        for item in &mut module.items {
            if let Item::FnDef(function) = item {
                function.scope = Some(definition_scope.clone());
            }
        }
        for implementation in &mut module.implementations {
            for function in &mut implementation.methods {
                function.scope = Some(definition_scope.clone());
            }
            for constant in &mut implementation.constants {
                constant.scope = Some(definition_scope.clone());
            }
        }
        self.modules.insert(
            path.into(),
            SourceModule {
                module,
                exports,
                dependencies: required.into_keys().collect(),
                implicit: source.implicit,
            },
        );
        stack.pop();
        Ok(())
    }

    pub(super) fn append(&self, module: &mut Module, required: &BTreeMap<String, Span>) {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<_> = required
            .iter()
            .rev()
            .map(|(path, span)| (path.as_str(), *span))
            .collect();
        while let Some((path, inherited_span)) = pending.pop() {
            if !seen.insert(path) {
                continue;
            }
            let source = &self.modules[path];
            let span = required.get(path).copied().unwrap_or(inherited_span);
            let mut imported = source.module.clone();
            spans::relocate(&mut imported, span);
            module.enums.extend(imported.enums);
            module.records.extend(imported.records);
            module.items.extend(imported.items);
            module.implementations.extend(imported.implementations);
            pending.extend(
                source
                    .dependencies
                    .iter()
                    .rev()
                    .map(|path| (path.as_str(), span)),
            );
        }
    }
}
