//! Source-defined methods for the standard sum types. The backend retains their
//! canonical identities so runtime producers and postfix operators share an ABI.
use crate::syntax::{ExprKind, Function, Program, TypeExpr, Visibility};
use crate::Diagnostic;
use std::collections::HashSet;
use std::sync::OnceLock;

pub(super) const SOURCES: [(&str, &str); 2] = [
    ("Option", include_str!("../../std/joky/option.jk")),
    ("Result", include_str!("../../std/joky/result.jk")),
];

pub(super) fn is_method(name: &str) -> bool {
    static METHODS: OnceLock<HashSet<String>> = OnceLock::new();
    METHODS
        .get_or_init(|| {
            SOURCES
                .into_iter()
                .flat_map(|(owner, source)| {
                    crate::syntax::parse_program(source)
                        .expect("valid standard sum module")
                        .functions
                        .into_iter()
                        .filter_map(move |function| {
                            let receiver = function.parameters.first()?;
                            let TypeExpr::Apply { callee, .. } = &receiver.ty.kind else {
                                return None;
                            };
                            (function.visibility == Visibility::Public
                                && callee.is_name(owner)
                                && !function.type_parameters.is_empty())
                            .then(|| format!("@prelude/{owner}/{}", function.name))
                        })
                })
                .collect()
        })
        .contains(name)
}

pub(super) fn functions(offset: u32) -> Result<Vec<Function>, Diagnostic> {
    let mut offset = offset;
    let mut functions = Vec::new();
    for (owner, source) in SOURCES {
        let program = crate::syntax::parse_program_at(source, offset)?;
        offset = offset
            .checked_add(source.len() as u32 + 1)
            .ok_or_else(|| Diagnostic::codegen("prelude exceeds AST identity space"))?;
        for mut function in program.functions {
            if function.name == owner && !canonical_type_definition(&function) {
                return Err(Diagnostic::codegen(format!(
                    "standard {owner} definition does not match its runtime ABI"
                )));
            }
            function.name = if function.name == owner {
                format!("@prelude/{owner}")
            } else {
                format!("@prelude/{owner}/{}", function.name)
            };
            function.visibility = Visibility::Private;
            functions.push(function);
        }
    }
    Ok(functions)
}

pub(super) fn canonical_type_definition(function: &Function) -> bool {
    let name = function
        .name
        .strip_prefix("@prelude/")
        .unwrap_or(&function.name);
    let expected: &[(&str, &[(&str, &str)])] = match name {
        "Option" => &[("Some", &[("value", "T")]), ("None", &[])],
        "Result" => &[("Ok", &[("value", "T")]), ("Err", &[("error", "E")])],
        _ => return false,
    };
    let parameters: &[&str] = if name == "Option" {
        &["T"]
    } else {
        &["T", "E"]
    };
    let ExprKind::Block(body) = &function.body.kind else {
        return false;
    };
    let [expression] = body.as_slice() else {
        return false;
    };
    let ExprKind::AnonymousEnum { variants } = &expression.kind else {
        return false;
    };
    function
        .type_parameters
        .iter()
        .map(|p| p.name.as_str())
        .eq(parameters.iter().copied())
        && variants.len() == expected.len()
        && variants
            .iter()
            .zip(expected)
            .all(|(variant, (name, fields))| {
                variant.name == *name
                    && variant.fields.len() == fields.len()
                    && variant
                        .fields
                        .iter()
                        .zip(*fields)
                        .all(|(field, (name, ty))| {
                            field.name == *name
                                && !field.borrowed
                                && matches!(&field.ty.kind, TypeExpr::Name(name) if name == ty)
                        })
            })
}

pub(super) fn source_end(program: &Program) -> usize {
    program
        .functions
        .iter()
        .map(|v| v.span.end())
        .chain(program.constants.iter().map(|v| v.span.end()))
        .chain(program.classes.iter().map(|v| v.span.end()))
        .chain(program.structs.iter().map(|v| v.span.end()))
        .chain(program.enums.iter().map(|v| v.span.end()))
        .chain(program.effects.iter().map(|v| v.span.end()))
        .chain(program.traits.iter().map(|v| v.span.end()))
        .chain(program.impls.iter().map(|v| v.span.end()))
        .chain(program.intrinsic_types.iter().map(|v| v.span.end()))
        .chain(program.imports.iter().map(|v| v.span.end()))
        .max()
        .unwrap_or(0)
}
