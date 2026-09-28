//! Embedded source modules are parsed once and resolved in their own namespaces.
use super::*;
use std::collections::HashMap;

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

#[derive(Clone, Debug)]
pub(super) struct SourceModule {
    module: Module,
    pub exports: ImportScope,
    dependencies: Vec<String>,
    pub implicit: ImplicitImports,
}

#[derive(Clone, Debug)]
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
        check_dependency_depth(Span::empty(0), path, stack)?;
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
        // Embedded modules parse against a real source snapshot so their
        // diagnostics point into the stdlib text instead of the import site.
        let file = crate::SourceFile::new(format!("{path}.jk"), source.source);
        let mut module =
            crate::parser::parse_with_source(source.source, file.id()).map_err(|error| {
                let mut sources = crate::SourceMap::default();
                sources.insert(file.clone());
                Error::from(error).with_sources(&sources)
            })?;
        Arc::make_mut(&mut module.sources).insert(file);
        // Source modules never inherit the consumer's implicit prelude or hosts.
        for import in &module.imports {
            self.load(&import.path, sources, registry, stack)?;
        }
        self.prepare(
            path,
            module,
            registry,
            source.reexport_imports,
            source.implicit,
            true,
        )?;
        stack.pop();
        Ok(())
    }

    pub(super) fn prepare(
        &mut self,
        path: &str,
        mut module: Module,
        registry: &TypeRegistry,
        reexport_imports: bool,
        implicit: ImplicitImports,
        embedded: bool,
    ) -> Result<(), Error> {
        let (mut scope, required) = bind(&module, registry, &self.modules)?;
        let mut exports = ImportScope::default();
        if reexport_imports {
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
                resolved: None,
            };
            scope.types.insert(name.clone(), binding.clone());
            scope.types.insert(binding.name.clone(), binding.clone());
            if embedded || declaration.public {
                exports.types.insert(name, binding.clone());
            }
            for variant in &declaration.variants {
                let constructor = ConstructorBinding {
                    enumeration: binding.clone(),
                    variant: variant.name.clone(),
                };
                if (embedded || declaration.public)
                    && exports
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
                resolved: None,
            };
            scope.types.insert(name.clone(), binding.clone());
            scope.types.insert(binding.name.clone(), binding.clone());
            if embedded || declaration.public {
                exports.types.insert(name, binding);
            }
        }
        for item in &mut module.items {
            let Item::FnDef(function) = item else {
                if let Item::ExternFn(function) = item {
                    scope
                        .functions
                        .insert(function.name.clone(), function.name.clone());
                }
                continue;
            };
            let name = function.name.clone();
            function.name = format!("{path}.{name}");
            if scope.constructors.contains_key(&name)
                || scope
                    .functions
                    .get(&name)
                    .is_some_and(|value| value == &function.name)
            {
                return Err(error(
                    function.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate exported value '{name}'"),
                ));
            }
            if embedded || function.public {
                exports
                    .functions
                    .insert(name.clone(), function.name.clone());
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
        module
            .imports
            .retain(|import| !self.modules.contains_key(&import.path));
        // Resolve hosts in the defining module, before dependencies are appended.
        let mut module = crate::core::resolver::resolve(&module, &registry.hosts)?;
        let own_items = module.items.len();
        self.append(&mut module, &required);
        let aliases = crate::core::types::type_definitions(registry, &module)?;
        for alias in &module.type_aliases {
            let binding = TypeBinding {
                name: format!("{path}.{}", alias.name),
                declaration: None,
                resolved: Some(aliases[&alias.name].clone()),
            };
            Arc::make_mut(module.scope.as_mut().unwrap())
                .types
                .insert(alias.name.clone(), binding.clone());
            if embedded || alias.public {
                exports.types.insert(alias.name.clone(), binding);
            }
        }
        // A bound scope suppresses expansion, including while this cache is built.
        let mut module = registry.resolve_aliases(&module)?;
        let definition_scope = Arc::new(DefinitionScope {
            imports: module.scope.clone().unwrap(),
            nominals: Arc::new(crate::core::types::nominal_types(registry, &module)?),
            members: module
                .implementations
                .iter()
                .take(own.3)
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
                        .map(|name| format!("{}.{name}", implementation.target_name))
                })
                .chain(module.scope.as_ref().unwrap().members.iter().cloned())
                .collect(),
        });
        module.enums.truncate(own.0);
        module.records.truncate(own.1);
        module.items.truncate(own_items);
        module.implementations.truncate(own.3);
        module.imports.clear();
        for implementation in &module.implementations {
            for method in &implementation.methods {
                if embedded || method.public {
                    exports
                        .members
                        .insert(format!("{}.{}", implementation.target_name, method.name));
                }
            }
            for constant in &implementation.constants {
                if embedded || constant.public {
                    exports
                        .members
                        .insert(format!("{}.{}", implementation.target_name, constant.name));
                }
            }
        }
        for item in &mut module.items {
            if let Item::FnDef(function) = item {
                function.public |= embedded;
                function.scope = Some(definition_scope.clone());
            }
        }
        for implementation in &mut module.implementations {
            for function in &mut implementation.methods {
                function.public |= embedded;
                function.scope = Some(definition_scope.clone());
            }
            for constant in &mut implementation.constants {
                constant.public |= embedded;
                constant.scope = Some(definition_scope.clone());
            }
        }
        self.modules.insert(
            path.into(),
            SourceModule {
                module,
                exports,
                dependencies: required.into_iter().collect(),
                implicit,
            },
        );
        Ok(())
    }

    pub(super) fn append(&self, module: &mut Module, required: &BTreeSet<String>) {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<_> = required.iter().rev().map(String::as_str).collect();
        while let Some(path) = pending.pop() {
            if !seen.insert(path) {
                continue;
            }
            let source = &self.modules[path];
            let imported = source.module.clone();
            Arc::make_mut(&mut module.sources).extend(&imported.sources);
            if let Some(scope) = &mut module.scope {
                Arc::make_mut(scope)
                    .members
                    .extend(source.exports.members.iter().cloned());
            }
            module.enums.extend(imported.enums);
            module.records.extend(imported.records);
            for item in imported.items {
                if let Item::ExternFn(function) = &item {
                    Arc::make_mut(module.scope.as_mut().unwrap())
                        .imported_functions
                        .insert(function.name.clone());
                    if module.items.iter().any(|item| matches!(item, Item::ExternFn(existing) if existing.name == function.name && existing.suspends == function.suspends && existing.ret_ty == function.ret_ty && existing.params.iter().map(|p| &p.ty).eq(function.params.iter().map(|p| &p.ty)))) {
                        continue;
                    }
                }
                module.items.push(item);
            }
            module.implementations.extend(imported.implementations);
            pending.extend(source.dependencies.iter().rev().map(String::as_str));
        }
    }
}

/// Canonical dependency names are implementation details, never implicit imports.
pub(super) fn record_imported_functions(module: &mut Module) {
    let scope = Arc::make_mut(module.scope.as_mut().unwrap());
    for item in &module.items {
        if let Item::FnDef(function) = item {
            if function.scope.is_some() {
                scope.imported_functions.insert(function.name.clone());
            }
        }
    }
}
