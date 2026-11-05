//! Expand Eq and Hash into ordinary borrowing implementations, with field-based
//! conditional bounds inferred by the shared derive machinery.
use crate::core::types::{ParameterKind, Type, TypeApplication, TypeParameter};
use crate::parser::ast::{
    BinOp, Expr, FnDef, FunctionKind, ImplDef, Literal, MatchArm, Module, NoEscape, Param, Pattern,
    PayloadPattern, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::sync::Arc;

fn ident(name: &str, span: Span) -> Expr {
    Expr::Ident {
        name: name.into(),
        span,
    }
}

fn literal(literal: Literal, span: Span) -> Expr {
    Expr::Literal { literal, span }
}

fn field(receiver: &str, name: &str, span: Span) -> Expr {
    Expr::Unary {
        op: UnaryOp::Borrow,
        operand: Box::new(Expr::Field {
            base: Box::new(ident(receiver, span)),
            name: name.into(),
            type_args: vec![],
            span,
        }),
        span,
    }
}

fn call(alias: &str, capability: &str, args: Vec<Expr>, span: Span) -> Expr {
    Expr::Call {
        callee: format!("{alias}.{capability}.{}", capability.to_ascii_lowercase()),
        type_args: vec![],
        args,
        span,
    }
}

fn equal(comparisons: Vec<Expr>, span: Span) -> Expr {
    let mut exprs: Vec<_> = comparisons
        .into_iter()
        .map(|comparison| Expr::If {
            condition: Box::new(Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(comparison),
                span,
            }),
            then_branch: Box::new(Expr::Return {
                value: Some(Box::new(literal(Literal::Bool(false), span))),
                span,
            }),
            else_branch: None,
            span,
        })
        .collect();
    exprs.push(literal(Literal::Bool(true), span));
    Expr::Block { exprs, span }
}

fn hash(components: Vec<Expr>, span: Span) -> Expr {
    // Ordered FNV-style combination of component hashes. This is not a stable
    // serialization format; the Eq/Hash contract is the only public guarantee.
    let mut exprs = vec![Expr::Var {
        name: "$hash".into(),
        annotation: Some(Type::UInt64),
        value: Box::new(literal(Literal::UInt64(14695981039346656037), span)),
        span,
    }];
    for component in components {
        exprs.push(Expr::Assign {
            op: None,
            name: "$hash".into(),
            value: Box::new(Expr::BinOp {
                op: BinOp::Mul,
                lhs: Box::new(Expr::BinOp {
                    op: BinOp::BitXor,
                    lhs: Box::new(ident("$hash", span)),
                    rhs: Box::new(component),
                    span,
                }),
                rhs: Box::new(literal(Literal::UInt64(1099511628211), span)),
                span,
            }),
            span,
        });
    }
    exprs.push(ident("$hash", span));
    Expr::Block { exprs, span }
}

fn variant(name: &str, payload: bool, binding: &str, span: Span) -> Pattern {
    Pattern::Variant {
        name: name.into(),
        binding: payload.then(|| {
            Box::new(PayloadPattern::Name {
                name: binding.into(),
                span,
            })
        }),
        span,
    }
}

pub(super) fn expand(module: &mut Module) -> Result<(), Error> {
    for capability in ["Eq", "Hash"] {
        let mut requests = Vec::new();
        for record in &mut module.records {
            if let Some(span) = super::clone_derive::take(&mut record.attributes, capability)? {
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
            if let Some(span) = super::clone_derive::take(&mut enumeration.attributes, capability)?
            {
                requests.push((
                    enumeration.name.clone(),
                    enumeration.type_params.clone(),
                    None,
                    Some(enumeration.variants.clone()),
                    span,
                ));
            }
        }
        let Some(first) = requests.first() else {
            continue;
        };
        let alias = super::json_derive::import(
            module,
            &format!("joky/{}", capability.to_ascii_lowercase()),
            &format!("${}_derive", capability.to_ascii_lowercase()),
            first.4,
        );
        for (name, parameters, fields, variants, span) in requests {
            if parameters.iter().any(|p| p.kind == ParameterKind::Const) {
                return Err(Error::new_with_code(
                    ErrorPhase::TypeCheck,
                    ErrorCode::TypeMismatch,
                    "Derive with const parameters requires unsupported generic trait implementations",
                ).with_span(span));
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
                let mut components = Vec::new();
                for f in fields {
                    obligations.push(Param {
                        mutable: false,
                        name: f.name.clone(),
                        ty: crate::core::types::substitute_syntax(&f.ty, &substitutions),
                        span: f.span,
                    });
                    let mut args = vec![field("self", &f.name, f.span)];
                    if capability == "Eq" {
                        args.push(field("other", &f.name, f.span));
                    }
                    components.push(call(&alias, capability, args, f.span));
                }
                if capability == "Eq" {
                    equal(components, span)
                } else {
                    hash(components, span)
                }
            } else {
                let variants = variants.unwrap();
                let multiple = variants.len() > 1;
                let mut arms = Vec::new();
                for (tag, v) in variants.into_iter().enumerate() {
                    if let Some(ty) = &v.payload {
                        obligations.push(Param {
                            mutable: false,
                            name: v.name.clone(),
                            ty: crate::core::types::substitute_syntax(ty, &substitutions),
                            span: v.span,
                        });
                    }
                    let path = format!("{name}.{}", v.name);
                    let payload = v.payload.is_some();
                    let body = if capability == "Eq" {
                        let mut inner = vec![MatchArm {
                            pattern: variant(&path, payload, "$right", v.span),
                            body: if payload {
                                call(
                                    &alias,
                                    capability,
                                    vec![ident("$left", v.span), ident("$right", v.span)],
                                    v.span,
                                )
                            } else {
                                literal(Literal::Bool(true), v.span)
                            },
                            span: v.span,
                        }];
                        if multiple {
                            inner.push(MatchArm {
                                pattern: Pattern::Wildcard { span: v.span },
                                body: literal(Literal::Bool(false), v.span),
                                span: v.span,
                            });
                        }
                        Expr::Match {
                            conditional: false,
                            iteration: false,
                            value: Box::new(ident("other", v.span)),
                            arms: inner,
                            span: v.span,
                        }
                    } else {
                        let mut components = vec![literal(Literal::UInt64(tag as u64), v.span)];
                        if payload {
                            components.push(call(
                                &alias,
                                capability,
                                vec![ident("$left", v.span)],
                                v.span,
                            ));
                        }
                        hash(components, v.span)
                    };
                    arms.push(MatchArm {
                        pattern: variant(&path, payload, "$left", v.span),
                        body,
                        span: v.span,
                    });
                }
                Expr::Match {
                    conditional: false,
                    iteration: false,
                    value: Box::new(ident("self", span)),
                    arms,
                    span,
                }
            };
            let mut params = vec![Param {
                mutable: false,
                name: "self".into(),
                ty: Type::reference(target.clone(), false),
                span,
            }];
            if capability == "Eq" {
                params.push(Param {
                    mutable: false,
                    name: "other".into(),
                    ty: Type::reference(target.clone(), false),
                    span,
                });
            }
            let method = FnDef {
                every: None,
                kind: FunctionKind::Ordinary,
                noescape: Some(Box::new(NoEscape {
                    all: true,
                    params: vec![],
                })),
                public: false,
                buffer_intrinsic: None,
                compiler_intrinsic: None,
                buffer_element: None,
                scope: None,
                name: capability.to_ascii_lowercase(),
                type_params: vec![],
                params,
                ret_ty: if capability == "Eq" {
                    Type::Bool
                } else {
                    Type::UInt64
                },
                suspends: false,
                body,
                span,
            };
            module.implementations.push(ImplDef {
                trait_name: Some(format!("{alias}.{capability}")),
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
    }
    Ok(())
}
