//! Embedded source modules are parsed once and resolved in their own namespaces.
use super::*;
use std::collections::HashMap;

mod intrinsics;
#[cfg(test)]
mod qualification_tests;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct DefinitionScope {
    pub lazy_functions: bool,
    pub imports: Arc<ImportScope>,
    pub nominals: Arc<HashMap<String, Type>>,
    pub members: BTreeSet<String>,
    pub records: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImplicitImports {
    None,
    Types,
    All,
}

/// Privileges belong to these compiler-owned source definitions, never paths.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceKind {
    User,
    Embedded,
    BufferStorage,
    BufferLibrary,
    DropTrait,
    Check,
    TypeCheck,
    Integer,
    Float,
    Text,
    Json,
    Mem,
    Domain,
    Vm,
}

pub(super) struct SourceDefinition {
    pub kind: SourceKind,
    pub path: &'static str,
    pub source: &'static str,
    pub reexport_imports: bool,
    pub implicit: ImplicitImports,
}

pub(super) const SOURCES: &[SourceDefinition] = &[
    SourceDefinition {
        kind: SourceKind::Vm,
        path: "joky/vm",
        source: include_str!("../../stdlib/vm.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/outbox",
        source: include_str!("../../stdlib/outbox.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/admission",
        source: include_str!("../../stdlib/admission.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Domain,
        path: "joky/domain",
        source: include_str!("../../stdlib/domain.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/json/typed",
        source: include_str!("../../stdlib/json_typed.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Json,
        path: "joky/json",
        source: include_str!("../../stdlib/json.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/dns",
        source: include_str!("../../stdlib/dns.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/tcp",
        source: include_str!("../../stdlib/tcp.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Mem,
        path: "joky/mem",
        source: include_str!("../../stdlib/mem.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/io",
        source: include_str!("../../stdlib/io.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Embedded,
        path: "joky/show",
        source: include_str!("../../stdlib/show.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Text,
        path: "joky/string",
        source: include_str!("../../stdlib/string.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::TypeCheck,
        path: "joky/internal/types",
        source: include_str!("../../stdlib/internal/types.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Check,
        path: "joky/check",
        source: include_str!("../../stdlib/check.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::DropTrait,
        path: "joky/drop",
        source: include_str!("../../stdlib/drop.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferStorage,
        path: "joky/internal/buffer",
        source: include_str!("../../stdlib/internal/buffer.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/array",
        source: include_str!("../../stdlib/array.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/element",
        source: include_str!("../../stdlib/element.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/slice",
        source: include_str!("../../stdlib/slice.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Embedded,
        path: "joky/option",
        source: include_str!("../../stdlib/option.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Embedded,
        path: "joky/result",
        source: include_str!("../../stdlib/result.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Integer,
        path: "joky/integer",
        source: include_str!("../../stdlib/integer.jk"),
        reexport_imports: true,
        implicit: ImplicitImports::Types,
    },
    SourceDefinition {
        kind: SourceKind::Float,
        path: "joky/float",
        source: include_str!("../../stdlib/float.jk"),
        reexport_imports: true,
        implicit: ImplicitImports::Types,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/eq",
        source: include_str!("../../stdlib/eq.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/hash",
        source: include_str!("../../stdlib/hash.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/map",
        source: include_str!("../../stdlib/map.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/list",
        source: include_str!("../../stdlib/list.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/set",
        source: include_str!("../../stdlib/set.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    },
    SourceDefinition {
        kind: SourceKind::Embedded,
        path: PRELUDE,
        source: include_str!("../../stdlib/prelude.jk"),
        reexport_imports: true,
        implicit: ImplicitImports::All,
    },
];

#[derive(Clone, Debug)]
pub(super) struct SourceModule {
    pub private: bool,
    pub lazy_hosts: BTreeSet<String>,
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
        if source.kind == SourceKind::BufferStorage {
            bind_buffer_definitions(&mut module)?;
        }
        intrinsics::bind_functions(&mut module, source.kind)?;
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
            source.kind,
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
        kind: SourceKind,
    ) -> Result<(), Error> {
        crate::core::json_derive::expand(&mut module)?;
        let embedded = !matches!(
            kind,
            SourceKind::User
                | SourceKind::BufferLibrary
                | SourceKind::Text
                | SourceKind::Json
                | SourceKind::Domain
                | SourceKind::Vm
        );
        let (mut scope, required) = bind(
            &module,
            registry,
            &self.modules,
            matches!(
                kind,
                SourceKind::BufferLibrary
                    | SourceKind::Text
                    | SourceKind::Json
                    | SourceKind::Domain
                    | SourceKind::Vm
            ),
        )?;
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
                } else if let Some(binding) = scope.traits.get(name) {
                    exports.traits.insert(name.clone(), binding.clone());
                } else if let Some(binding) = scope.constructors.get(name) {
                    exports.constructors.insert(name.clone(), binding.clone());
                } else if let Some(binding) = scope.functions.get(name) {
                    exports.functions.insert(name.clone(), binding.clone());
                } else if let Some(binding) = scope.constants.get(name) {
                    exports.constants.insert(name.clone(), binding.clone());
                }
            }
        }
        let mut constructors: BTreeMap<String, Vec<ConstructorBinding>> = BTreeMap::new();
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
                if embedded || declaration.public {
                    exports
                        .constructors
                        .entry(variant.name.clone())
                        .or_default()
                        .push(constructor.clone());
                }
                constructors
                    .entry(variant.name.clone())
                    .or_default()
                    .push(constructor);
            }
        }
        scope.constructors.extend(constructors);
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
        for declaration in &mut module.traits {
            declaration.is_drop = kind == SourceKind::DropTrait;
            let name = declaration.name.clone();
            declaration.name = format!("{path}.{name}");
            let binding = TraitBinding {
                name: declaration.name.clone(),
                id: declaration.self_parameter.id,
            };
            scope.traits.insert(name.clone(), binding.clone());
            if embedded || declaration.public {
                exports.traits.insert(name, binding);
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
            if (embedded || function.public)
                && !matches!(
                    function.compiler_intrinsic,
                    Some(crate::parser::ast::CompilerIntrinsic::RuneFromValidU32)
                )
            {
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
            implementation.origin = Some(path.into());
            if let Some(binding) = scope.types.get(&implementation.target_name) {
                implementation.target_name.clone_from(&binding.name);
            }
        }
        let own = (
            module.enums.len(),
            module.records.len(),
            module.items.len(),
            module.implementations.len(),
            module.traits.len(),
            module.constants.len(),
        );
        // Constants must be visible to the defining module's own bodies, so
        // they join the scope before it is frozen into `DefinitionScope`.
        for constant in &mut module.constants {
            constant.public |= embedded;
            constant.path = Some(path.into());
            let canonical = format!("{path}.{}", constant.name);
            scope
                .constants
                .insert(constant.name.clone(), canonical.clone());
            scope.constants.insert(canonical.clone(), canonical.clone());
            if constant.public {
                exports.constants.insert(constant.name.clone(), canonical);
            }
        }
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
        intrinsics::validate_signatures(&module.items[..own_items])?;
        let definition_scope = Arc::new(DefinitionScope {
            lazy_functions: kind != SourceKind::User,
            records: module.records.iter().take(own.1).map(|r| r.id).collect(),
            imports: module.scope.clone().unwrap(),
            nominals: Arc::new(crate::core::types::nominal_types(registry, &module)?),
            members: module
                .implementations
                .iter()
                .take(own.3)
                .filter(|implementation| implementation.trait_name.is_none())
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
        for record in &mut module.records {
            record.imported = true;
        }
        module.items.truncate(own_items);
        module.implementations.truncate(own.3);
        module.traits.truncate(own.4);
        module.constants.truncate(own.5);
        for declaration in &mut module.traits {
            declaration.scope = Some(definition_scope.clone());
        }
        module.imports.clear();
        for implementation in &module.implementations {
            if implementation.trait_name.is_some() {
                continue;
            }
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
            let mut scope = definition_scope.clone();
            if let Some(generated) = &implementation.scope {
                let imports = Arc::make_mut(&mut Arc::make_mut(&mut scope).imports);
                imports.traits.extend(generated.imports.traits.clone());
            }
            implementation.scope = Some(scope.clone());
            for function in &mut implementation.methods {
                function.public |= embedded && implementation.trait_name.is_none();
                function.scope = Some(scope.clone());
            }
            for constant in &mut implementation.constants {
                constant.public |= embedded;
                constant.scope = Some(definition_scope.clone());
            }
        }
        for constant in &mut module.constants {
            constant.scope = Some(definition_scope.clone());
        }
        self.modules.insert(
            path.into(),
            SourceModule {
                lazy_hosts: if kind != SourceKind::User {
                    module
                        .items
                        .iter()
                        .filter_map(|item| match item {
                            Item::ExternFn(f) => Some(f.name.clone()),
                            _ => None,
                        })
                        .collect()
                } else {
                    BTreeSet::new()
                },
                private: matches!(kind, SourceKind::BufferStorage | SourceKind::TypeCheck),
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
                for name in &source.lazy_hosts {
                    if !module
                        .items
                        .iter()
                        .any(|item| matches!(item, Item::ExternFn(f) if &f.name == name))
                        || scope.lazy_hosts.contains(name)
                    {
                        Arc::make_mut(scope).lazy_hosts.insert(name.clone());
                    }
                }
                Arc::make_mut(scope)
                    .members
                    .extend(source.exports.members.iter().cloned());
            }
            module.enums.extend(imported.enums);
            module.records.extend(imported.records);
            module.constants.extend(imported.constants);
            for item in imported.items {
                if let Item::ExternFn(function) = &item {
                    if !source.lazy_hosts.contains(&function.name) {
                        Arc::make_mut(module.scope.as_mut().unwrap())
                            .lazy_hosts
                            .remove(&function.name);
                    }
                    Arc::make_mut(module.scope.as_mut().unwrap())
                        .imported_functions
                        .insert(function.name.clone());
                    if module.items.iter().any(|item| matches!(item, Item::ExternFn(existing) if existing.name == function.name && existing.c_abi == function.c_abi && existing.suspends == function.suspends && existing.ret_ty == function.ret_ty && existing.params.iter().map(|p| &p.ty).eq(function.params.iter().map(|p| &p.ty)))) {
                        continue;
                    }
                }
                module.items.push(item);
            }
            module.implementations.extend(imported.implementations);
            module.traits.extend(imported.traits);
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

/// Install internal buffer type identities only on the trusted storage AST.
fn bind_buffer_definitions(module: &mut Module) -> Result<(), Error> {
    for name in ["Buffer", "Element", "Slice"] {
        let alias = module
            .type_aliases
            .iter_mut()
            .find(|alias| alias.name == name && alias.type_params.len() == 1)
            .ok_or_else(|| {
                error(
                    Span::empty(0),
                    ErrorCode::InvalidState,
                    "Missing compiler buffer type definition",
                )
            })?;
        let element = Arc::new(Type::Parameter(alias.type_params[0].clone()));
        alias.target = match name {
            "Buffer" => Type::Buffer(element),
            "Element" => Type::BufferElement(element),
            _ => Type::BufferSlice(element),
        };
    }
    Ok(())
}

// Test-only wrappers run against the one production algorithm and its fields.
#[cfg(test)]
pub(crate) fn map_test_library() -> StandardLibrary {
    use std::sync::OnceLock;
    static SOURCE: OnceLock<String> = OnceLock::new();
    thread_local! {
        static LIBRARY: StandardLibrary = {
            let source = SOURCE.get_or_init(|| format!("{}\n{}", include_str!("../../stdlib/map.jk"), include_str!("../../../tests/fixtures/map_invariants.jk")));
            let definitions: Vec<_> = SOURCES.iter().map(|s| SourceDefinition {
                kind: s.kind, path: s.path, source: if s.path == "joky/map" { source } else { s.source },
                reexport_imports: s.reexport_imports, implicit: s.implicit,
            }).collect();
            StandardLibrary::from_sources(&definitions).unwrap()
        };
    }
    LIBRARY.with(Clone::clone)
}

// Text boundary probes use the production private algorithm without exporting
// it in the ordinary standard library or synthesizing unsafe buffer metadata.
#[cfg(test)]
pub(crate) fn string_test_library() -> StandardLibrary {
    use std::sync::OnceLock;
    static SOURCE: OnceLock<String> = OnceLock::new();
    thread_local! {
        static LIBRARY: StandardLibrary = {
            let source = SOURCE.get_or_init(|| format!("{}\n{}", include_str!("../../stdlib/string.jk"), include_str!("../../../tests/fixtures/string_length_invariants.jk")));
            let definitions: Vec<_> = SOURCES.iter().map(|definition| SourceDefinition {
                kind: definition.kind, path: definition.path,
                source: if definition.path == "joky/string" { source } else { definition.source },
                reexport_imports: definition.reexport_imports, implicit: definition.implicit,
            }).collect();
            StandardLibrary::from_sources(&definitions).unwrap()
        };
    }
    LIBRARY.with(Clone::clone)
}

// List probes inspect the real private slots without adding a production API.
#[cfg(test)]
pub(crate) fn list_test_library() -> StandardLibrary {
    use std::sync::OnceLock;
    static SOURCE: OnceLock<String> = OnceLock::new();
    thread_local! {
        static LIBRARY: StandardLibrary = {
            let source = SOURCE.get_or_init(|| format!("{}\n{}", include_str!("../../stdlib/list.jk"), include_str!("../../../tests/fixtures/list_invariants.jk")));
            let definitions: Vec<_> = SOURCES.iter().map(|s| SourceDefinition {
                kind: s.kind, path: s.path, source: if s.path == "joky/list" { source } else { s.source },
                reexport_imports: s.reexport_imports, implicit: s.implicit,
            }).collect();
            StandardLibrary::from_sources(&definitions).unwrap()
        };
    }
    LIBRARY.with(Clone::clone)
}

// TCP probes exercise buffered terminal states with the production methods.
#[cfg(test)]
pub(crate) fn tcp_test_library() -> StandardLibrary {
    use std::sync::OnceLock;
    static SOURCE: OnceLock<String> = OnceLock::new();
    thread_local! {
        static LIBRARY: StandardLibrary = {
            let source = SOURCE.get_or_init(|| format!("{}\n{}", include_str!("../../stdlib/tcp.jk"), include_str!("../../../tests/fixtures/buffered_tcp_invariants.jk")));
            let definitions: Vec<_> = SOURCES.iter().map(|s| SourceDefinition {
                kind: s.kind, path: s.path, source: if s.path == "joky/tcp" { source } else { s.source },
                reexport_imports: s.reexport_imports, implicit: s.implicit,
            }).collect();
            StandardLibrary::from_sources(&definitions).unwrap()
        };
    }
    LIBRARY.with(Clone::clone)
}
