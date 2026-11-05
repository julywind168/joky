//! Expand Clone into ordinary checked borrowing implementations. Field obligations
//! use the same conditional-bound inference as the other built-in derives.
use crate::core::types::{ParameterKind, Type, TypeApplication, TypeParameter};
use crate::parser::ast::{
    Attribute, AttributeValue, BindingPattern, Expr, FieldInit, FnDef, ImplDef, MatchArm, Module,
    NoEscape, Param, Pattern, PayloadPattern, RecordFieldPattern, RecordPattern, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::sync::Arc;

fn error(span: Span, message: &str) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch, message).with_span(span)
}

pub(super) fn take(
    attributes: &mut Vec<Attribute>,
    capability: &str,
) -> Result<Option<Span>, Error> {
    let mut found = None;
    for attribute in attributes.iter_mut().filter(|a| a.path == "derive") {
        if let Some(arguments) = &mut attribute.arguments {
            if arguments.is_empty() {
                return Err(error(attribute.span, "#derive requires at least one trait"));
            }
            let mut retained = Vec::new();
            for argument in std::mem::take(arguments) {
                if matches!(&argument.value, AttributeValue::Name(a) if a.path == capability) {
                    let AttributeValue::Name(name) = &argument.value else {
                        unreachable!()
                    };
                    if argument.name.is_some() || name.arguments.is_some() {
                        return Err(error(
                            argument.span,
                            &format!("{capability} derive takes no arguments"),
                        ));
                    }
                    if found.replace(argument.span).is_some() {
                        return Err(error(
                            argument.span,
                            &format!("Duplicate {capability} derive"),
                        ));
                    }
                } else {
                    retained.push(argument);
                }
            }
            *arguments = retained;
        }
    }
    if found.is_some() {
        attributes
            .retain(|a| !(a.path == "derive" && a.arguments.as_ref().is_some_and(Vec::is_empty)));
    }
    Ok(found)
}
fn ident(name: &str, span: Span) -> Expr {
    Expr::Ident {
        name: name.into(),
        span,
    }
}
fn copy(name: &str, alias: &str, span: Span) -> Expr {
    Expr::Call {
        callee: format!("{alias}.Clone.clone"),
        type_args: vec![],
        args: vec![Expr::Unary {
            op: UnaryOp::Borrow,
            operand: Box::new(ident(name, span)),
            span,
        }],
        span,
    }
}

pub(super) fn expand(module: &mut Module) -> Result<(), Error> {
    let mut requests = Vec::new();
    for record in &mut module.records {
        if let Some(span) = take(&mut record.attributes, "Clone")? {
            requests.push((
                record.name.clone(),
                record.type_params.clone(),
                Some(record.fields.clone()),
                None,
                span,
            ));
        }
    }
    for enumeration in &mut module.enums {
        if let Some(span) = take(&mut enumeration.attributes, "Clone")? {
            requests.push((
                enumeration.name.clone(),
                enumeration.type_params.clone(),
                None,
                Some(enumeration.variants.clone()),
                span,
            ));
        }
        if let Some(attribute) = enumeration.attributes.first() {
            return Err(error(
                attribute.span,
                "JSON derives require a struct; enums support #derive(Clone, Eq, Hash)",
            ));
        }
    }
    if requests.is_empty() {
        return Ok(());
    }
    let alias = super::json_derive::import(module, "joky/clone", "$clone_derive", requests[0].4);
    for (name, parameters, fields, variants, span) in requests {
        if parameters.iter().any(|p| p.kind == ParameterKind::Const) {
            return Err(error(
                span,
                "Derive with const parameters requires unsupported generic trait implementations",
            ));
        }
        let type_params: Vec<_> = parameters
            .iter()
            .map(|p| {
                let mut next = TypeParameter::new(p.name.clone(), p.span);
                next.copy = p.copy;
                Arc::new(next)
            })
            .collect();
        let substitutions = parameters
            .iter()
            .zip(&type_params)
            .map(|(old, new)| (old.id, Type::Parameter(new.clone())))
            .collect();
        let target = if type_params.is_empty() {
            Type::Named(name.clone())
        } else {
            Type::Applied(Arc::new(TypeApplication {
                name: name.clone(),
                arguments: type_params.iter().map(|p| p.argument()).collect(),
            }))
        };
        let mut obligations = Vec::new();
        let body = if let Some(fields) = fields {
            let pattern = BindingPattern::Record {
                inner: Box::new(RecordPattern {
                    resolved: None,
                    name: name.clone(),
                    type_args: type_params.iter().map(|p| p.argument()).collect(),
                    rest: false,
                    fields: fields
                        .iter()
                        .enumerate()
                        .map(|(i, f)| RecordFieldPattern {
                            name: f.name.clone(),
                            pattern: BindingPattern::Name {
                                name: format!("$field{i}"),
                                span: f.span,
                            },
                            span: f.span,
                        })
                        .collect(),
                }),
                span,
            };
            for f in &fields {
                obligations.push(Param {
                    mutable: false,
                    name: f.name.clone(),
                    ty: crate::core::types::substitute_syntax(&f.ty, &substitutions),
                    span: f.span,
                });
            }
            Expr::Block {
                exprs: vec![
                    Expr::Destructure {
                        pattern,
                        mutable: false,
                        annotation: None,
                        value: Box::new(ident("self", span)),
                        span,
                    },
                    Expr::Record {
                        base: None,
                        name: name.clone(),
                        type_args: type_params.iter().map(|p| p.argument()).collect(),
                        fields: fields
                            .iter()
                            .enumerate()
                            .map(|(i, f)| FieldInit {
                                name: f.name.clone(),
                                value: copy(&format!("$field{i}"), &alias, f.span),
                                span: f.span,
                            })
                            .collect(),
                        span,
                    },
                ],
                span,
            }
        } else {
            Expr::Match {
                conditional: false,
                iteration: false,
                value: Box::new(ident("self", span)),
                arms: variants
                    .unwrap()
                    .into_iter()
                    .map(|v| {
                        if let Some(ty) = &v.payload {
                            obligations.push(Param {
                                mutable: false,
                                name: v.name.clone(),
                                ty: crate::core::types::substitute_syntax(ty, &substitutions),
                                span: v.span,
                            });
                        }
                        let has_payload = v.payload.is_some();
                        MatchArm {
                            guard: None,
                            pattern: Pattern::Variant {
                                name: format!("{name}.{}", v.name),
                                binding: has_payload.then(|| {
                                    Box::new(PayloadPattern::Name {
                                        name: "$payload".into(),
                                        span: v.span,
                                    })
                                }),
                                span: v.span,
                            },
                            body: if has_payload {
                                Expr::Call {
                                    callee: format!("{name}.{}", v.name),
                                    type_args: type_params.iter().map(|p| p.argument()).collect(),
                                    args: vec![copy("$payload", &alias, v.span)],
                                    span: v.span,
                                }
                            } else {
                                Expr::Field {
                                    base: Box::new(ident(&name, v.span)),
                                    name: v.name,
                                    type_args: type_params.iter().map(|p| p.argument()).collect(),
                                    span: v.span,
                                }
                            },
                            span: v.span,
                        }
                    })
                    .collect(),
                span,
            }
        };
        let method = FnDef {
            every: None,
            kind: crate::parser::ast::FunctionKind::Clone,
            noescape: Some(Box::new(NoEscape {
                all: true,
                params: vec![],
            })),
            public: false,
            buffer_intrinsic: None,
            compiler_intrinsic: None,
            buffer_element: None,
            scope: None,
            name: "clone".into(),
            type_params: vec![],
            params: vec![Param {
                mutable: false,
                name: "self".into(),
                ty: Type::reference(target.clone(), false),
                span,
            }],
            ret_ty: target.clone(),
            suspends: false,
            body,
            span,
        };
        module.implementations.push(ImplDef {
            trait_name: Some(format!("{alias}.Clone")),
            origin: None,
            scope: None,
            derive_fields: Some(obligations),
            type_params,
            target,
            target_name: name,
            associated_types: vec![],
            methods: vec![method],
            constants: vec![],
            span,
        });
    }
    Ok(())
}

/// Borrowed patterns snapshot Copy payloads but retain references to owners.
/// Select the argument shape after substitution and call the actual Clone trait,
/// never an unrelated inherent method with the same spelling.
pub(crate) fn prepare(function: &FnDef) -> std::borrow::Cow<'_, FnDef> {
    if function.kind != crate::parser::ast::FunctionKind::Clone {
        return std::borrow::Cow::Borrowed(function);
    }
    let mut function = function.clone();
    fn argument(expr: &mut Expr, ty: &Type) {
        if ty.is_owner() {
            let Expr::Call { args, .. } = expr else {
                unreachable!()
            };
            let Expr::Unary { operand, .. } = &args[0] else {
                unreachable!()
            };
            args[0] = *operand.clone();
        }
    }
    match (&function.ret_ty, &mut function.body) {
        (Type::Record(record), Expr::Block { exprs, .. }) => {
            let Expr::Record { fields, .. } = exprs.last_mut().unwrap() else {
                unreachable!()
            };
            for (field, definition) in fields.iter_mut().zip(&record.fields) {
                argument(&mut field.value, &definition.ty);
            }
        }
        (Type::Enum(enumeration), Expr::Match { arms, .. }) => {
            for (arm, variant) in arms.iter_mut().zip(&enumeration.variants) {
                if let Some(ty) = &variant.payload {
                    let Expr::Call { args, .. } = &mut arm.body else {
                        unreachable!()
                    };
                    argument(&mut args[0], ty);
                }
            }
        }
        _ => unreachable!("resolved Clone derive"),
    }
    std::borrow::Cow::Owned(function)
}
