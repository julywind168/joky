//! Expand the tagged object representation into ordinary enum matches.
use super::*;
use crate::parser::ast::EnumDef;

pub(super) fn implementation(
    enumeration: &EnumDef,
    capability: &str,
    span: Span,
    imports: &JsonImports,
) -> ImplDef {
    let encoding = capability == "ToJson";
    let (type_params, substitutions) = derive_parameters(&enumeration.type_params);
    let mut fields = Vec::new();
    let mut arms = Vec::with_capacity(enumeration.variants.len());
    for variant in &enumeration.variants {
        let payload = variant
            .payload
            .as_ref()
            .map(|ty| crate::core::types::substitute_syntax(ty, &substitutions));
        if let Some(ty) = &payload {
            fields.push(Param {
                mutable: false,
                name: variant.name.clone(),
                ty: ty.clone(),
                span: variant.span,
            });
        }
        let variant_span = variant.span;
        let name = format!("{}.{}", enumeration.name, variant.name);
        let (pattern, body) = if encoding {
            let pattern = Pattern::Variant {
                name,
                binding: payload.as_ref().map(|_| {
                    Box::new(PayloadPattern::Name {
                        name: "$payload".into(),
                        span: variant_span,
                    })
                }),
                span: variant_span,
            };
            (
                pattern,
                encode_variant(&variant.name, payload, &imports.result, variant_span),
            )
        } else {
            let pattern = Pattern::Value(Box::new(PayloadPattern::Literal {
                literal: Literal::String(variant.name.clone()),
                span: variant_span,
            }));
            let body = decode_variant(
                &enumeration.name,
                &variant.name,
                &type_params,
                payload,
                &imports.result,
                variant_span,
            );
            (pattern, body)
        };
        arms.push(MatchArm {
            pattern,
            guard: None,
            body,
            span: variant_span,
        });
    }

    let mut body = vec![start_object(encoding, &imports.result, span)];
    if !encoding {
        body.push(Expr::Let {
            name: "$tag".into(),
            annotation: None,
            value: Box::new(propagate(
                field_call("take_required_field", "type", Type::String, None, span),
                &imports.result,
                span,
            )),
            span,
        });
        arms.push(MatchArm {
            pattern: Pattern::Wildcard { span },
            guard: None,
            body: call(
                format!("{}.Result.Err", imports.result),
                vec![context_call(
                    "field_error",
                    vec![],
                    vec![
                        string("type", span),
                        Expr::Field {
                            base: Box::new(ident(
                                &format!("{}.MappingErrorKind", imports.typed),
                                span,
                            )),
                            name: "UnknownVariant".into(),
                            type_args: vec![],
                            span,
                        },
                    ],
                    span,
                )],
                span,
            ),
            span,
        });
    }
    body.push(Expr::Match {
        conditional: false,
        iteration: false,
        value: Box::new(ident(if encoding { "self" } else { "$tag" }, span)),
        arms,
        span,
    });
    if encoding {
        body.push(ok(object_value(&imports.json, span), &imports.result, span));
    }
    mapping_impl(
        &enumeration.name,
        type_params,
        fields,
        body,
        encoding,
        imports,
        span,
    )
}

fn field_call(method: &str, key: &str, ty: Type, value: Option<Expr>, span: Span) -> Expr {
    let mut args = vec![
        borrow(ident("$fields", span), true, span),
        string(key, span),
    ];
    args.extend(value);
    context_call(method, vec![ty], args, span)
}

fn encode_variant(name: &str, payload: Option<Type>, result: &str, span: Span) -> Expr {
    let mut exprs = vec![
        Expr::Let {
            name: "$tag".into(),
            annotation: None,
            value: Box::new(string(name, span)),
            span,
        },
        propagate(
            field_call(
                "put_field",
                "type",
                Type::String,
                Some(borrow(ident("$tag", span), false, span)),
                span,
            ),
            result,
            span,
        ),
    ];
    if let Some(ty) = payload {
        exprs.push(propagate(
            field_call(
                "put_field",
                "value",
                ty,
                Some(borrow(ident("$payload", span), false, span)),
                span,
            ),
            result,
            span,
        ));
    }
    Expr::Block { exprs, span }
}

fn decode_variant(
    name: &str,
    variant: &str,
    type_params: &[Arc<TypeParameter>],
    payload: Option<Type>,
    result: &str,
    span: Span,
) -> Expr {
    let mut exprs = Vec::new();
    let type_args = type_params.iter().map(|p| p.argument()).collect();
    let value = if let Some(ty) = payload {
        exprs.push(Expr::Let {
            name: "$payload".into(),
            annotation: None,
            value: Box::new(propagate(
                field_call("take_required_field", "value", ty, None, span),
                result,
                span,
            )),
            span,
        });
        Expr::Call {
            callee: format!("{name}.{variant}"),
            type_args,
            args: vec![ident("$payload", span)],
            span,
        }
    } else {
        Expr::Field {
            base: Box::new(ident(name, span)),
            name: variant.into(),
            type_args,
            span,
        }
    };
    exprs.push(finish_object(result, span));
    exprs.push(ok(value, result, span));
    Expr::Block { exprs, span }
}
