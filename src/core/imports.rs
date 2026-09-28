//! Import scopes bind public spellings to declarations, independently of locals.
use crate::{parser::ast::*, Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

mod files;
mod sources;
pub(crate) use files::{expand_with, resolve_file};
pub(crate) use sources::{DefinitionScope, StandardLibrary};

const PRELUDE: &str = "joky/prelude";

#[derive(Clone, Debug)]
pub(crate) struct TypeBinding {
    pub name: String,
    pub declaration: Option<u64>,
    pub resolved: Option<Type>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConstructorBinding {
    pub enumeration: TypeBinding,
    pub variant: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ImportScope {
    pub types: BTreeMap<String, TypeBinding>,
    pub constructors: BTreeMap<String, ConstructorBinding>,
    pub functions: BTreeMap<String, String>,
    pub imported_functions: BTreeSet<String>,
    pub explicit_values: BTreeSet<String>,
    pub modules: BTreeMap<String, String>,
    pub members: BTreeSet<String>,
}

pub(crate) fn is_source_library(path: &str) -> bool {
    sources::SOURCES.iter().any(|source| source.path == path)
}

fn error(span: Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Resolve, code, message).with_span(span)
}

/// Guard shared by every source-module loader: reject cyclic dependencies and
/// depth beyond 128 levels, with one wording for both embedded and file loads.
pub(crate) fn check_dependency_depth(
    span: Span,
    path: &str,
    stack: &[String],
) -> Result<(), Error> {
    if stack.iter().any(|parent| parent == path) {
        let mut cycle = stack.to_vec();
        cycle.push(path.into());
        return Err(error(
            span,
            ErrorCode::InvalidImport,
            format!("Cyclic source module dependency: {}", cycle.join(" -> ")),
        ));
    }
    if stack.len() >= 128 {
        return Err(error(
            span,
            ErrorCode::ResourceLimit,
            "Source module dependency depth exceeds 128 levels",
        ));
    }
    Ok(())
}

pub(crate) fn expand<'a>(
    module: &'a Module,
    registry: &TypeRegistry,
) -> Result<Cow<'a, Module>, Error> {
    if module.scope.is_some() {
        return Ok(Cow::Borrowed(module));
    }
    let standard = registry.standard.get_or_init(StandardLibrary::new);
    Ok(Cow::Owned(finish(module, registry, standard)?))
}

/// Bind `module` against `graph` and assemble the expanded result. Both
/// expansion entries (embedded-only and file-backed) share this epilogue.
pub(crate) fn finish(
    module: &Module,
    registry: &TypeRegistry,
    graph: &StandardLibrary,
) -> Result<Module, Error> {
    let (scope, required) = bind(module, registry, &graph.modules)?;
    let mut result = module.clone();
    result.scope = Some(Arc::new(scope));
    graph.append(&mut result, &required);
    result
        .imports
        .retain(|import| !graph.modules.contains_key(&import.path));
    sources::record_imported_functions(&mut result);
    Ok(result)
}

fn bind(
    module: &Module,
    registry: &TypeRegistry,
    libraries: &BTreeMap<String, sources::SourceModule>,
) -> Result<(ImportScope, BTreeSet<String>), Error> {
    let mut scope = ImportScope::default();
    let mut required = BTreeSet::new();
    let mut seen_paths = BTreeSet::new();
    let mut explicit_types = BTreeSet::new();
    let mut explicit_values = BTreeSet::new();
    let local_types: BTreeMap<_, _> = module
        .enums
        .iter()
        .map(|d| (d.name.as_str(), d.name_span))
        .chain(
            module
                .records
                .iter()
                .map(|d| (d.name.as_str(), d.name_span)),
        )
        .chain(
            module
                .type_aliases
                .iter()
                .map(|d| (d.name.as_str(), d.name_span)),
        )
        .collect();
    let local_values: BTreeSet<_> = module
        .items
        .iter()
        .map(|item| match item {
            Item::FnDef(f) => f.name.as_str(),
            Item::ExternFn(f) => f.name.as_str(),
        })
        .collect();
    if registry.prelude {
        required.insert(PRELUDE.into());
        let prelude = &libraries[PRELUDE].exports;
        scope.types.extend(prelude.types.clone());
        scope.constructors.extend(
            prelude
                .constructors
                .iter()
                .filter(|(name, _)| !local_values.contains(name.as_str()))
                .map(|(name, binding)| (name.clone(), binding.clone())),
        );
        scope.functions.extend(
            prelude
                .functions
                .iter()
                .filter(|(name, _)| !local_values.contains(name.as_str()))
                .map(|(name, binding)| (name.clone(), binding.clone())),
        );
    }
    for import in &module.imports {
        if !seen_paths.insert(import.path.as_str()) {
            return Err(error(
                import.span,
                ErrorCode::InvalidImport,
                format!("Duplicate import '{}'", import.path),
            ));
        }
        let root = import
            .alias
            .as_deref()
            .unwrap_or_else(|| import.path.rsplit('/').next().unwrap());
        if root == "_" {
            return Err(error(
                import.span,
                ErrorCode::ReservedName,
                "A module alias cannot be '_'",
            ));
        }
        if scope
            .modules
            .insert(root.into(), import.path.clone())
            .is_some()
            || local_values.contains(root)
            || local_types.contains_key(root)
        {
            return Err(error(
                import.span,
                ErrorCode::DuplicateDefinition,
                format!("Module alias '{root}' is already defined"),
            ));
        }
        let source = libraries.get(&import.path);
        let exports = source
            .map(|source| source.exports.clone())
            .unwrap_or_default();
        if source.is_some() {
            required.insert(import.path.clone());
        }
        for (name, binding) in &exports.types {
            scope
                .types
                .insert(format!("{root}.{name}"), binding.clone());
        }
        for (name, binding) in &exports.constructors {
            scope
                .constructors
                .insert(format!("{root}.{name}"), binding.clone());
        }
        for (name, binding) in &exports.functions {
            scope
                .functions
                .insert(format!("{root}.{name}"), binding.clone());
            scope.imported_functions.insert(binding.clone());
        }
        // Compatibility shorthand is metadata, separate from a module's exports.
        if import.members.is_empty() && import.alias.is_none() {
            if let Some(source) = source {
                if source.implicit != sources::ImplicitImports::None {
                    for (name, binding) in &exports.types {
                        scope
                            .types
                            .entry(name.clone())
                            .or_insert_with(|| binding.clone());
                    }
                }
                if source.implicit == sources::ImplicitImports::All {
                    for (name, binding) in &exports.constructors {
                        if !local_values.contains(name.as_str()) {
                            scope
                                .constructors
                                .entry(name.clone())
                                .or_insert_with(|| binding.clone());
                        }
                    }
                    for (name, binding) in &exports.functions {
                        if !local_values.contains(name.as_str()) {
                            scope
                                .functions
                                .entry(name.clone())
                                .or_insert_with(|| binding.clone());
                        }
                    }
                }
            }
        }
        // Host modules export the opaque types in their registered signatures.
        let host_types = registry.module_types.get(&import.path);
        for (short, canonical) in host_types.into_iter().flatten() {
            scope.types.insert(
                format!("{root}.{short}"),
                TypeBinding {
                    name: canonical.clone(),
                    declaration: None,
                    resolved: None,
                },
            );
        }
        for member in &import.members {
            let name = member.alias.as_deref().unwrap_or(&member.name);
            if name == "_" {
                return Err(error(
                    member.span,
                    ErrorCode::ReservedName,
                    "An imported name cannot be '_'",
                ));
            }
            if member.kind == ImportMemberKind::Type {
                if !explicit_types.insert(name.to_owned()) {
                    return Err(error(
                        member.span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate imported type '{name}'"),
                    ));
                }
                let target = exports.types.get(&member.name).cloned().or_else(|| {
                    host_types
                        .and_then(|exports| exports.get(member.name.as_str()))
                        .map(|canonical| TypeBinding {
                            name: canonical.clone(),
                            declaration: None,
                            resolved: None,
                        })
                });
                let target = target.ok_or_else(|| {
                    error(
                        member.span,
                        ErrorCode::InvalidImport,
                        format!(
                            "Module '{}' does not export type '{}'",
                            import.path, member.name
                        ),
                    )
                })?;
                if scope
                    .types
                    .get(name)
                    .is_some_and(|old| old.name != target.name)
                {
                    return Err(error(
                        member.span,
                        ErrorCode::DuplicateDefinition,
                        format!("Imported type '{name}' conflicts with the prelude"),
                    ));
                }
                scope.types.insert(name.into(), target);
            } else if source.is_some() {
                if !explicit_values.insert(name.to_owned())
                    || local_values.contains(name)
                    || scope.modules.contains_key(name)
                {
                    return Err(error(
                        member.span,
                        ErrorCode::DuplicateDefinition,
                        format!("Imported constructor '{name}' is already defined"),
                    ));
                }
                let constructor = exports.constructors.get(&member.name);
                let function = exports.functions.get(&member.name);
                if constructor.is_none() && function.is_none() {
                    return Err(error(
                        member.span,
                        ErrorCode::InvalidImport,
                        format!(
                            "Module '{}' does not export value '{}'",
                            import.path, member.name
                        ),
                    ));
                }
                if crate::core::integer::IntegerType::conversion(name).is_some() {
                    return Err(error(
                        member.span,
                        ErrorCode::ReservedName,
                        format!("Reserved integer conversion function name '{name}'"),
                    ));
                }
                scope.constructors.remove(name);
                scope.functions.remove(name);
                if let Some(binding) = constructor {
                    scope.constructors.insert(name.into(), binding.clone());
                }
                if let Some(binding) = function {
                    scope.functions.insert(name.into(), binding.clone());
                }
            }
        }
    }
    for (name, span) in &local_types {
        if scope.types.contains_key(*name) {
            return Err(error(*span, ErrorCode::DuplicateDefinition, format!("Type '{name}' is already defined by an import or the prelude; disable the prelude for a local declaration")));
        }
    }
    for name in scope.modules.keys() {
        if scope.types.contains_key(name)
            || scope.constructors.contains_key(name)
            || scope.functions.contains_key(name)
        {
            return Err(error(
                module
                    .imports
                    .iter()
                    .find(|i| {
                        i.alias
                            .as_deref()
                            .unwrap_or_else(|| i.path.rsplit('/').next().unwrap())
                            == name
                    })
                    .unwrap()
                    .span,
                ErrorCode::DuplicateDefinition,
                format!("Module alias '{name}' conflicts with an imported name"),
            ));
        }
    }
    scope.explicit_values = explicit_values;
    Ok((scope, required))
}
