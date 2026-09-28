//! Import scopes bind public spellings to declarations, independently of locals.
use crate::{parser::ast::*, Error, ErrorCode, ErrorPhase, Span, TypeRegistry};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const OPTION: &str = "joky/option.Option";
const RESULT: &str = "joky/result.Result";

#[derive(Clone, Debug)]
pub(crate) struct TypeBinding {
    pub name: String,
    pub declaration: Option<u64>,
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
}

#[derive(Debug)]
pub(crate) struct StandardLibrary {
    option: EnumDef,
    result: EnumDef,
}

impl StandardLibrary {
    fn new() -> Self {
        let load = |source| {
            crate::parser::parse_library(
                source,
                Span::empty(0),
                &[("Option", OPTION), ("Result", RESULT)],
            )
            .expect("valid embedded standard library")
            .enums
            .remove(0)
        };
        Self {
            option: load(include_str!("../stdlib/option.jk")),
            result: load(include_str!("../stdlib/result.jk")),
        }
    }
}

pub(crate) fn is_source_library(path: &str) -> bool {
    matches!(
        path,
        "joky/option" | "joky/result" | "joky/integer" | "joky/prelude"
    )
}

fn error(span: Span, code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Resolve, code, message).with_span(span)
}

pub(crate) fn expand<'a>(
    module: &'a Module,
    registry: &TypeRegistry,
) -> Result<Cow<'a, Module>, Error> {
    if module.scope.is_some() {
        return Ok(Cow::Borrowed(module));
    }
    let standard = registry.standard.get_or_init(StandardLibrary::new);
    let binding = |declaration: &EnumDef| TypeBinding {
        name: declaration.name.clone(),
        declaration: Some(declaration.id),
    };
    let mut scope = ImportScope::default();
    let mut required = BTreeMap::new();
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
        required.insert("joky/prelude", Span::empty(0));
        for (name, declaration) in [("Option", &standard.option), ("Result", &standard.result)] {
            scope.types.insert(name.into(), binding(declaration));
            for variant in &declaration.variants {
                if !local_values.contains(variant.name.as_str()) {
                    scope.constructors.insert(
                        variant.name.clone(),
                        ConstructorBinding {
                            enumeration: binding(declaration),
                            variant: variant.name.clone(),
                        },
                    );
                }
            }
        }
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
        if is_source_library(&import.path) {
            required.insert(import.path.as_str(), import.span);
        }
        let exports: Vec<_> = match import.path.as_str() {
            "joky/option" | "joky/integer" => vec![("Option", &standard.option)],
            "joky/result" => vec![("Result", &standard.result)],
            "joky/prelude" => vec![("Option", &standard.option), ("Result", &standard.result)],
            _ => Vec::new(),
        };
        for (name, declaration) in &exports {
            scope
                .types
                .insert(format!("{root}.{name}"), binding(declaration));
            for variant in &declaration.variants {
                scope.constructors.insert(
                    format!("{root}.{}", variant.name),
                    ConstructorBinding {
                        enumeration: binding(declaration),
                        variant: variant.name.clone(),
                    },
                );
            }
        }
        // Preserve integer's existing Option export and explicit prelude imports.
        if import.members.is_empty()
            && import.alias.is_none()
            && matches!(import.path.as_str(), "joky/integer" | "joky/prelude")
        {
            for (name, declaration) in &exports {
                scope
                    .types
                    .entry((*name).into())
                    .or_insert_with(|| binding(declaration));
                if import.path == "joky/prelude" {
                    for variant in &declaration.variants {
                        if !local_values.contains(variant.name.as_str()) {
                            scope
                                .constructors
                                .entry(variant.name.clone())
                                .or_insert_with(|| ConstructorBinding {
                                    enumeration: binding(declaration),
                                    variant: variant.name.clone(),
                                });
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
                let target = exports
                    .iter()
                    .find(|(export, _)| *export == member.name)
                    .map(|(_, d)| binding(d))
                    .or_else(|| {
                        host_types
                            .and_then(|exports| exports.get(member.name.as_str()))
                            .map(|canonical| TypeBinding {
                                name: canonical.clone(),
                                declaration: None,
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
            } else if is_source_library(&import.path) {
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
                let (declaration, variant) = exports
                    .iter()
                    .find_map(|(_, d)| {
                        d.variants
                            .iter()
                            .find(|v| v.name == member.name)
                            .map(|v| (*d, v))
                    })
                    .ok_or_else(|| {
                        error(
                            member.span,
                            ErrorCode::InvalidImport,
                            format!(
                                "Module '{}' does not export value '{}'",
                                import.path, member.name
                            ),
                        )
                    })?;
                if crate::core::integer::IntegerType::conversion(name).is_some() {
                    return Err(error(
                        member.span,
                        ErrorCode::ReservedName,
                        format!("Reserved integer conversion function name '{name}'"),
                    ));
                }
                scope.constructors.insert(
                    name.into(),
                    ConstructorBinding {
                        enumeration: binding(declaration),
                        variant: variant.name.clone(),
                    },
                );
            }
        }
    }
    for (name, span) in &local_types {
        if scope.types.contains_key(*name) {
            return Err(error(*span, ErrorCode::DuplicateDefinition, format!("Type '{name}' is already defined by an import or the prelude; disable the prelude for a local declaration")));
        }
    }
    for name in scope.modules.keys() {
        if scope.types.contains_key(name) || scope.constructors.contains_key(name) {
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
    let integer_span = required
        .get("joky/integer")
        .or(required.get("joky/prelude"))
        .copied();
    let mut result = module.clone();
    for (path, declaration) in [
        ("joky/option", &standard.option),
        ("joky/result", &standard.result),
    ] {
        let span = required
            .get(path)
            .or(required.get("joky/prelude"))
            .copied()
            .or_else(|| (path == "joky/option").then_some(integer_span).flatten());
        if let Some(span) = span {
            let mut declaration = declaration.clone();
            declaration.span = span;
            declaration.name_span = span;
            for variant in &mut declaration.variants {
                variant.span = span;
            }
            result.enums.push(declaration);
        }
    }
    if let Some(span) = integer_span {
        let integer = crate::parser::parse_library(
            include_str!("../stdlib/integer.jk"),
            span,
            &[("Option", OPTION)],
        )?;
        result.implementations.extend(integer.implementations);
    }
    result.imports.retain(|i| !is_source_library(&i.path));
    scope.explicit_values = explicit_values;
    result.scope = Some(Arc::new(scope));
    Ok(Cow::Owned(result))
}
