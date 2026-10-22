//! Import scopes bind public spellings to declarations, independently of locals.
use crate::core::types::Type;
use crate::{
    parser::ast::{FnDef, Import, ImportMemberKind, IntrinsicAttribute, Item, Module},
    Error, ErrorCode, ErrorPhase, Span, TypeRegistry,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

mod files;
mod sources;
pub(crate) use files::{expand_with, resolve_file};
#[cfg(test)]
pub(crate) use sources::{
    list_test_library, map_test_library, string_test_library, tcp_test_library,
};
pub(crate) use sources::{DefinitionScope, StandardLibrary};

const PRELUDE: &str = "joky/prelude";

#[derive(Clone, Debug)]
pub(crate) struct TypeBinding {
    pub name: String,
    pub declaration: Option<u64>,
    pub resolved: Option<super::types::TypeDefinition>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConstructorBinding {
    pub enumeration: TypeBinding,
    pub variant: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TraitBinding {
    pub name: String,
    pub id: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ImportScope {
    /// Trusted declaration identities travel with normalized modules, including
    /// when another registry compiles them without loading its own stdlib cache.
    pub standard_enums: BTreeMap<u64, &'static str>,
    pub types: BTreeMap<String, TypeBinding>,
    pub traits: BTreeMap<String, TraitBinding>,
    /// Bare names retain every candidate; type-qualified lookup bypasses this map.
    pub constructors: BTreeMap<String, Vec<ConstructorBinding>>,
    pub functions: BTreeMap<String, String>,
    pub imported_functions: BTreeSet<String>,
    /// Module constants by visible spelling, mapped to the declaring module's
    /// canonical `<path>.<NAME>` key. Private constants never appear here.
    pub constants: BTreeMap<String, String>,
    pub imported_constants: BTreeSet<String>,
    /// Compiler-owned standard host declarations are required only by used bodies.
    pub lazy_hosts: BTreeSet<String>,
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
    let mut result = module.clone();
    super::json_derive::expand(&mut result)?;
    let (scope, required) = bind(&result, registry, &graph.modules, false)?;
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
    buffer_access: bool,
) -> Result<(ImportScope, BTreeSet<String>), Error> {
    if let Some((_, attribute)) = module.intrinsic_attributes.first() {
        return Err(error(
            attribute.span,
            ErrorCode::InvalidImport,
            "#intrinsic is restricted to trusted embedded standard-library definitions",
        ));
    }
    let mut scope = ImportScope::default();
    for (path, name, success) in [
        ("joky/option", "Option", "Some"),
        ("joky/result", "Result", "Ok"),
    ] {
        if let Some(id) = libraries
            .get(path)
            .and_then(|module| module.exports.types.get(name))
            .and_then(|binding| binding.declaration)
        {
            scope.standard_enums.insert(id, success);
        }
    }
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
    let local_traits: BTreeSet<_> = module.traits.iter().map(|d| d.name.as_str()).collect();
    let local_values: BTreeSet<_> = module
        .items
        .iter()
        .map(|item| match item {
            Item::FnDef(f) => f.name.as_str(),
            Item::ExternFn(f) => f.name.as_str(),
        })
        .chain(
            module
                .constants
                .iter()
                .map(|declaration| declaration.name.as_str()),
        )
        .collect();
    // Constants share the value namespace with functions and bare enum
    // constructors; the set above collapses same-name pairs, so collisions
    // with a constant are checked explicitly before names join the scope.
    let local_items_and_variants: BTreeSet<_> = module
        .items
        .iter()
        .map(|item| match item {
            Item::FnDef(f) => f.name.as_str(),
            Item::ExternFn(f) => f.name.as_str(),
        })
        .chain(
            module
                .enums
                .iter()
                .flat_map(|declaration| declaration.variants.iter().map(|v| v.name.as_str())),
        )
        .collect();
    for declaration in &module.constants {
        if local_items_and_variants.contains(declaration.name.as_str()) {
            return Err(error(
                declaration.span,
                ErrorCode::DuplicateDefinition,
                format!(
                    "Constant '{}' is already defined by a function or constructor in this module",
                    declaration.name
                ),
            ));
        }
    }
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
        scope.constants.extend(
            prelude
                .constants
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
            || local_traits.contains(root)
        {
            return Err(error(
                import.span,
                ErrorCode::DuplicateDefinition,
                format!("Module alias '{root}' is already defined"),
            ));
        }
        let source = libraries.get(&import.path);
        if source.is_some_and(|source| source.private) && !buffer_access {
            return Err(error(import.span, ErrorCode::InvalidImport,
                "Buffer storage primitives are private to compiler-bound standard library definitions"));
        }
        let exports = source
            .map(|source| source.exports.clone())
            .unwrap_or_default();
        if source.is_some() {
            required.insert(import.path.clone());
        }
        for (name, binding) in &exports.traits {
            scope
                .traits
                .insert(format!("{root}.{name}"), binding.clone());
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
        for (name, binding) in &exports.constants {
            scope
                .constants
                .insert(format!("{root}.{name}"), binding.clone());
            scope.imported_constants.insert(binding.clone());
        }
        // Compiler-generated calls use unspellable aliases so user names cannot
        // redirect array construction or interpolation. Existing imports suffice.
        let compiler_alias = match import.path.as_str() {
            "joky/array" => Some("$joky_array"),
            "joky/show" => Some("$joky_show"),
            "joky/string" => Some("$joky_string"),
            _ => None,
        };
        if let Some(alias) = compiler_alias {
            for (name, binding) in &exports.types {
                scope
                    .types
                    .insert(format!("{alias}.{name}"), binding.clone());
            }
            for (name, binding) in &exports.functions {
                scope
                    .functions
                    .insert(format!("{alias}.{name}"), binding.clone());
            }
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
                if matches!(name, "Copy" | "Fn" | "FnMut" | "FnOnce") {
                    return Err(error(
                        member.span,
                        ErrorCode::ReservedName,
                        "Callable bound names cannot be shadowed by type imports",
                    ));
                }
                if !explicit_types.insert(name.to_owned()) {
                    return Err(error(
                        member.span,
                        ErrorCode::DuplicateDefinition,
                        format!("Duplicate imported type '{name}'"),
                    ));
                }
                if let Some(target) = exports.traits.get(&member.name) {
                    if scope.types.contains_key(name) || scope.traits.contains_key(name) {
                        return Err(error(
                            member.span,
                            ErrorCode::DuplicateDefinition,
                            format!("Imported trait '{name}' is already defined"),
                        ));
                    }
                    scope.traits.insert(name.into(), target.clone());
                    continue;
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
                if scope.traits.contains_key(name)
                    || scope
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
                if constructor.is_some_and(|bindings| bindings.len() > 1) {
                    return Err(error(
                        member.span,
                        ErrorCode::InvalidImport,
                        format!(
                            "Ambiguous enum constructor '{}'; import its enum type and use Type.{}",
                            member.name, member.name
                        ),
                    ));
                }
                let function = exports.functions.get(&member.name);
                let constant = exports.constants.get(&member.name);
                if constructor.is_none() && function.is_none() && constant.is_none() {
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
                if name == "alloc" {
                    return Err(error(
                        member.span,
                        ErrorCode::ReservedName,
                        format!("Reserved memory intrinsic name '{name}'"),
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
                if let Some(binding) = constant {
                    scope.constants.insert(name.into(), binding.clone());
                    scope.imported_constants.insert(binding.clone());
                }
            }
        }
    }
    // A module reaches its own constants by bare name, and by the canonical
    // `<path>.<NAME>` spelling that imports resolve to. A declaration without a
    // path belongs to the entry module, which has no canonical prefix.
    for declaration in &module.constants {
        let canonical = declaration.path.as_ref().map_or_else(
            || declaration.name.clone(),
            |path| format!("{path}.{}", declaration.name),
        );
        scope
            .constants
            .entry(declaration.name.clone())
            .or_insert_with(|| canonical.clone());
        scope
            .constants
            .entry(canonical.clone())
            .or_insert_with(|| canonical);
    }
    for (name, span) in &local_types {
        if scope.types.contains_key(*name) || scope.traits.contains_key(*name) {
            return Err(error(*span, ErrorCode::DuplicateDefinition, format!("Type '{name}' is already defined by an import or the prelude; disable the prelude for a local declaration")));
        }
    }
    for declaration in &module.traits {
        let name = &declaration.name;
        if name == "_" || name == "Self" {
            return Err(error(
                declaration.span,
                ErrorCode::ReservedName,
                "Reserved trait name",
            ));
        }
        if local_types.contains_key(name.as_str())
            || local_values.contains(name.as_str())
            || scope.types.contains_key(name)
            || scope.functions.contains_key(name)
            || scope.constructors.contains_key(name)
            || scope
                .traits
                .insert(
                    name.clone(),
                    TraitBinding {
                        name: name.clone(),
                        id: declaration.self_parameter.id,
                    },
                )
                .is_some()
        {
            return Err(error(
                declaration.span,
                ErrorCode::DuplicateDefinition,
                format!("Trait '{name}' is already defined"),
            ));
        }
    }
    for name in scope.modules.keys() {
        if scope.types.contains_key(name)
            || scope.traits.contains_key(name)
            || scope.constructors.contains_key(name)
            || scope.functions.contains_key(name)
            || scope.constants.contains_key(name)
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
    // Local enum variants shadow implicit/prelude constructors as a group, so
    // declaration order never chooses one of several same-name variants.
    let mut constructors: BTreeMap<String, Vec<ConstructorBinding>> = BTreeMap::new();
    for declaration in &module.enums {
        for variant in &declaration.variants {
            if local_values.contains(variant.name.as_str())
                || explicit_values.contains(&variant.name)
                || scope.modules.contains_key(&variant.name)
            {
                return Err(error(
                    variant.span,
                    ErrorCode::DuplicateDefinition,
                    format!(
                        "Enum constructor '{}' conflicts with an existing value or module alias",
                        variant.name
                    ),
                ));
            }
            constructors
                .entry(variant.name.clone())
                .or_default()
                .push(ConstructorBinding {
                    enumeration: TypeBinding {
                        name: declaration.name.clone(),
                        declaration: Some(declaration.id),
                        resolved: None,
                    },
                    variant: variant.name.clone(),
                });
        }
    }
    scope.constructors.extend(constructors);
    scope.explicit_values = explicit_values;
    Ok((scope, required))
}
