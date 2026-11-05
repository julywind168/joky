//! Expand JSON derives into ordinary, hygienic trait implementations before binding.
use crate::core::types::{ParameterKind, Type, TypeApplication, TypeParameter};
use crate::parser::ast::{
    AttributeValue, Expr, FieldInit, FnDef, ImplDef, Import, Literal, MatchArm, MethodCall, Module,
    NoEscape, Param, Pattern, PayloadPattern, RecordDef, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::sync::Arc;

fn error(span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch, message).with_span(span)
}

/// Consume attributes on an unbound module. The resulting module can be normalized again.
pub(crate) fn expand(module: &mut Module) -> Result<(), Error> {
    super::key_derive::expand(module)?;
    super::clone_derive::expand(module)?;
    let mut requests = Vec::new();
    for (index, record) in module.records.iter().enumerate() {
        let mut seen = Vec::new();
        for attribute in record.attributes.iter().filter(|a| a.path == "derive") {
            if record
                .type_params
                .iter()
                .any(|p| p.kind == ParameterKind::Const)
            {
                return Err(error(attribute.span,"Derive with const parameters requires unsupported generic trait implementations"));
            }
            let Some(arguments) = &attribute.arguments else {
                return Err(error(
                    attribute.span,
                    "#derive requires Clone, Eq, Hash, ToJson and/or FromJson",
                ));
            };
            if arguments.is_empty() {
                return Err(error(attribute.span, "#derive requires at least one trait"));
            }
            for argument in arguments {
                let AttributeValue::Name(name) = &argument.value else {
                    return Err(error(
                        argument.span,
                        "Expected ToJson or FromJson in #derive",
                    ));
                };
                if argument.name.is_some()
                    || name.arguments.is_some()
                    || !matches!(name.path.as_str(), "ToJson" | "FromJson")
                {
                    return Err(error(
                        argument.span,
                        "Only #derive(Clone, Eq, Hash, ToJson, FromJson) is supported",
                    ));
                }
                if seen.contains(&name.path) {
                    return Err(error(
                        argument.span,
                        format!("Duplicate {} derive", name.path),
                    ));
                }
                seen.push(name.path.clone());
                requests.push((index, name.path.clone(), argument.span));
            }
        }
    }
    if requests.is_empty() {
        return Ok(());
    }
    let span = requests[0].2;
    let typed = import(module, "joky/json/typed", "$json_derive_typed", span);
    let json = import(module, "joky/json", "$json_derive_value", span);
    let result = import(module, "joky/result", "$json_derive_result", span);
    for (index, capability, span) in requests {
        let record = &module.records[index];
        module.implementations.push(implementation(
            record,
            &capability,
            span,
            &typed,
            &json,
            &result,
        ));
    }
    for record in &mut module.records {
        record.attributes.retain(|a| a.path != "derive");
    }
    Ok(())
}

pub(super) fn import(module: &mut Module, path: &str, alias: &str, span: Span) -> String {
    if let Some(import) = module.imports.iter().find(|i| i.path == path) {
        return import
            .alias
            .clone()
            .unwrap_or_else(|| path.rsplit('/').next().unwrap().into());
    }
    module.imports.push(Import {
        path: path.into(),
        alias: Some(alias.into()),
        members: vec![],
        span,
    });
    alias.into()
}

fn applied(name: String, arguments: Vec<Type>) -> Type {
    Type::Applied(Arc::new(TypeApplication {
        name,
        arguments: arguments.into_iter().map(Into::into).collect(),
    }))
}
fn ident(name: &str, span: Span) -> Expr {
    Expr::Ident {
        name: name.into(),
        span,
    }
}
fn borrow(value: Expr, mutable: bool, span: Span) -> Expr {
    Expr::Unary {
        op: if mutable {
            UnaryOp::BorrowMut
        } else {
            UnaryOp::Borrow
        },
        operand: Box::new(value),
        span,
    }
}
fn call(callee: String, args: Vec<Expr>, span: Span) -> Expr {
    Expr::Call {
        callee,
        type_args: vec![],
        args,
        span,
    }
}
fn context_call(name: &str, type_args: Vec<Type>, args: Vec<Expr>, span: Span) -> Expr {
    Expr::MethodCall(Box::new(MethodCall {
        receiver: ident("$context", span),
        name: name.into(),
        type_args: type_args.into_iter().map(Into::into).collect(),
        args,
        span,
    }))
}
fn propagate(value: Expr, result: &str, span: Span) -> Expr {
    Expr::Match {
        conditional: false,
        iteration: false,
        value: Box::new(value),
        arms: vec![
            MatchArm {
                guard: None,
                pattern: Pattern::Variant {
                    name: format!("{result}.Result.Ok"),
                    binding: Some(Box::new(PayloadPattern::Name {
                        name: "$ok".into(),
                        span,
                    })),
                    span,
                },
                body: ident("$ok", span),
                span,
            },
            MatchArm {
                guard: None,
                pattern: Pattern::Variant {
                    name: format!("{result}.Result.Err"),
                    binding: Some(Box::new(PayloadPattern::Name {
                        name: "$error".into(),
                        span,
                    })),
                    span,
                },
                body: Expr::Return {
                    value: Some(Box::new(call(
                        format!("{result}.Result.Err"),
                        vec![ident("$error", span)],
                        span,
                    ))),
                    span,
                },
                span,
            },
        ],
        span,
    }
}

fn implementation(
    record: &RecordDef,
    capability: &str,
    span: Span,
    typed: &str,
    json: &str,
    result: &str,
) -> ImplDef {
    let type_params: Vec<_> = record
        .type_params
        .iter()
        .map(|p| {
            let mut parameter = TypeParameter::new(p.name.clone(), p.span);
            parameter.copy = p.copy;
            Arc::new(parameter)
        })
        .collect();
    let substitutions: crate::core::types::Substitutions = record
        .type_params
        .iter()
        .zip(&type_params)
        .map(|(old, new)| (old.id, Type::Parameter(new.clone())))
        .collect();
    let fields: Vec<_> = record
        .fields
        .iter()
        .map(|f| Param {
            mutable: false,
            name: f.name.clone(),
            ty: crate::core::types::substitute_syntax(&f.ty, &substitutions),
            span: f.span,
        })
        .collect();
    let target = if type_params.is_empty() {
        Type::Named(record.name.clone())
    } else {
        applied(
            record.name.clone(),
            type_params.iter().cloned().map(Type::Parameter).collect(),
        )
    };
    let value_type = Type::Named(format!("{json}.Value"));
    let encoding = capability == "ToJson";
    let object = context_call(
        if encoding { "object" } else { "read_object" },
        vec![],
        if encoding {
            vec![]
        } else {
            vec![ident("$value", span)]
        },
        span,
    );
    let mut body = vec![Expr::Var {
        name: "$fields".into(),
        annotation: None,
        value: Box::new(propagate(object, result, span)),
        span,
    }];
    for (index, field) in fields.iter().enumerate() {
        let span = field.span;
        let mut args = vec![
            borrow(ident("$fields", span), true, span),
            Expr::Literal {
                literal: Literal::String(field.name.clone()),
                span,
            },
        ];
        if encoding {
            args.push(borrow(
                Expr::Field {
                    base: Box::new(ident("self", span)),
                    name: field.name.clone(),
                    type_args: vec![],
                    span,
                },
                false,
                span,
            ));
            body.push(propagate(
                context_call("put_field", vec![field.ty.clone()], args, span),
                result,
                span,
            ));
        } else {
            body.push(Expr::Let {
                name: format!("$field_{index}"),
                annotation: None,
                value: Box::new(propagate(
                    context_call("take_field", vec![field.ty.clone()], args, span),
                    result,
                    span,
                )),
                span,
            });
        }
    }
    let output = if encoding {
        call(
            format!("{json}.Value.Object"),
            vec![ident("$fields", span)],
            span,
        )
    } else {
        body.push(propagate(
            context_call("finish_object", vec![], vec![ident("$fields", span)], span),
            result,
            span,
        ));
        Expr::Record {
            base: None,
            name: record.name.clone(),
            type_args: type_params.iter().map(|p| p.argument()).collect(),
            fields: fields
                .iter()
                .enumerate()
                .map(|(index, field)| FieldInit {
                    name: field.name.clone(),
                    value: ident(&format!("$field_{index}"), field.span),
                    span: field.span,
                })
                .collect(),
            span,
        }
    };
    body.push(call(format!("{result}.Result.Ok"), vec![output], span));
    let method = FnDef {
        every: None,
        kind: if encoding {
            crate::parser::ast::FunctionKind::JsonEncode
        } else {
            crate::parser::ast::FunctionKind::Ordinary
        },
        noescape: Some(Box::new(NoEscape {
            all: true,
            params: vec![],
        })),
        public: false,
        buffer_intrinsic: None,
        compiler_intrinsic: None,
        buffer_element: None,
        scope: None,
        name: if encoding { "to_json" } else { "from_json" }.into(),
        type_params: vec![],
        params: vec![
            Param {
                mutable: false,
                name: if encoding { "self" } else { "$value" }.into(),
                ty: if encoding {
                    Type::reference(target.clone(), false)
                } else {
                    value_type.clone()
                },
                span,
            },
            Param {
                mutable: false,
                name: "$context".into(),
                ty: Type::reference(Type::Named(format!("{typed}.Context")), true),
                span,
            },
        ],
        ret_ty: applied(
            format!("{result}.Result"),
            vec![
                if encoding { value_type } else { target.clone() },
                Type::Named(format!("{typed}.MappingError")),
            ],
        ),
        suspends: false,
        body: Expr::Block { exprs: body, span },
        span,
    };
    ImplDef {
        trait_name: Some(format!("{typed}.{capability}")),
        origin: None,
        scope: None,
        derive_fields: Some(fields),
        type_params,
        target,
        target_name: record.name.clone(),
        associated_types: vec![],
        methods: vec![method],
        constants: vec![],
        span,
    }
}

/// Copy fields in an owning record need a local snapshot before borrowing.
/// Select this after substitution: a generic field can become Copy or owning.
pub(crate) fn prepare_encoding(function: &FnDef) -> std::borrow::Cow<'_, FnDef> {
    if function.kind != crate::parser::ast::FunctionKind::JsonEncode {
        return std::borrow::Cow::Borrowed(function);
    }
    let mut function = function.clone();
    let Expr::Block { exprs, .. } = &mut function.body else {
        unreachable!()
    };
    let mut body = Vec::new();
    for (index, mut expression) in std::mem::take(exprs).into_iter().enumerate() {
        if let Expr::Match { value, span, .. } = &mut expression {
            if let Expr::MethodCall(call) = value.as_mut() {
                if call.name == "put_field"
                    && !call.type_args[0].as_type().is_some_and(Type::is_owner)
                {
                    let Expr::Unary { operand, .. } = &call.args[2] else {
                        unreachable!()
                    };
                    let name = format!("$copy_{index}");
                    body.push(Expr::Let {
                        name: name.clone(),
                        annotation: None,
                        value: operand.clone(),
                        span: *span,
                    });
                    call.args[2] = borrow(ident(&name, *span), false, *span);
                }
            }
        }
        body.push(expression);
    }
    *exprs = body;
    std::borrow::Cow::Owned(function)
}
