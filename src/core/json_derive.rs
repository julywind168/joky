//! Expand JSON derives into ordinary, hygienic trait implementations before binding.
use crate::core::types::{ParameterKind, Substitutions, Type, TypeApplication, TypeParameter};
use crate::parser::ast::{
    AnonymousFunction, Attribute, AttributeValue, Expr, FieldInit, FnDef, ImplDef, Import, Literal,
    MatchArm, MethodCall, Module, NoEscape, Param, Pattern, PayloadPattern, RecordDef, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::collections::HashMap;
use std::sync::Arc;

mod enums;

struct JsonImports {
    typed: String,
    json: String,
    result: String,
}

fn error(span: Span, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch, message).with_span(span)
}

/// Consume attributes on an unbound module. The resulting module can be normalized again.
pub(crate) fn expand(module: &mut Module) -> Result<(), Error> {
    super::postgres_derive::expand(module)?;
    super::key_derive::expand(module)?;
    super::clone_derive::expand(module)?;
    let mut records = Vec::new();
    for (index, record) in module.records.iter().enumerate() {
        let derives = requests(&record.attributes, &record.type_params)?;
        if derives.is_empty() && record.field_attributes.is_empty() {
            continue;
        }
        let fields = json_fields(record, &derives)?;
        if !derives.is_empty() {
            records.push((index, derives, fields));
        }
    }
    let mut enumerations = Vec::new();
    for (index, enumeration) in module.enums.iter().enumerate() {
        if let Some(attribute) = enumeration.attributes.iter().find(|a| a.path != "derive") {
            return Err(error(
                attribute.span,
                format!("Unknown enum attribute #{}", attribute.path),
            ));
        }
        let derives = requests(&enumeration.attributes, &enumeration.type_params)?;
        if !derives.is_empty() {
            enumerations.push((index, derives));
        }
    }
    let Some(span) = records
        .first()
        .map(|r| r.1[0].1)
        .or_else(|| enumerations.first().map(|e| e.1[0].1))
    else {
        return Ok(());
    };
    let imports = JsonImports {
        typed: import(module, "joky/json/typed", "$json_derive_typed", span),
        json: import(module, "joky/json", "$json_derive_value", span),
        result: import(module, "joky/result", "$json_derive_result", span),
    };
    for (index, derives, fields) in records {
        let record = &mut module.records[index];
        for (capability, span) in derives {
            module.implementations.push(implementation(
                record,
                &fields,
                &capability,
                span,
                &imports,
            ));
        }
        record.attributes.retain(|a| a.path != "derive");
        record.field_attributes.clear();
    }
    for (index, derives) in enumerations {
        let enumeration = &mut module.enums[index];
        for (capability, span) in derives {
            module.implementations.push(enums::implementation(
                enumeration,
                &capability,
                span,
                &imports,
            ));
        }
        enumeration.attributes.retain(|a| a.path != "derive");
    }
    Ok(())
}

fn requests(
    attributes: &[Attribute],
    parameters: &[Arc<TypeParameter>],
) -> Result<Vec<(String, Span)>, Error> {
    let mut seen = Vec::new();
    for attribute in attributes.iter().filter(|a| a.path == "derive") {
        if parameters.iter().any(|p| p.kind == ParameterKind::Const) {
            return Err(error(
                attribute.span,
                "Derive with const parameters requires unsupported generic trait implementations",
            ));
        }
        let Some(arguments) = &attribute.arguments else {
            return Err(error(
                attribute.span,
                "#derive requires Clone, Eq, Hash, ToJson, FromJson and/or FromRow",
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
                    "Only #derive(Clone, Eq, Hash, ToJson, FromJson, FromRow) is supported",
                ));
            }
            if seen.iter().any(|(capability, _)| capability == &name.path) {
                return Err(error(
                    argument.span,
                    format!("Duplicate {} derive", name.path),
                ));
            }
            seen.push((name.path.clone(), argument.span));
        }
    }
    Ok(seen)
}

enum JsonField {
    Mapped { key: String, default: Option<Expr> },
    Skipped { default: Option<Expr> },
}

fn json_fields(record: &RecordDef, derives: &[(String, Span)]) -> Result<Vec<JsonField>, Error> {
    let decoding = derives.iter().any(|(name, _)| name == "FromJson");
    let mut fields = Vec::with_capacity(record.fields.len());
    let mut seen = HashMap::new();
    for (index, field) in record.fields.iter().enumerate() {
        let mut rename = None;
        let mut default = None;
        let mut skip = None;
        for attribute in record.field_attributes.get(&index).into_iter().flatten() {
            if attribute.path != "json" {
                return Err(error(
                    attribute.span,
                    format!("Unknown field attribute #{}", attribute.path),
                ));
            }
            if derives.is_empty() {
                return Err(error(
                    attribute.span,
                    "#json requires a ToJson or FromJson derive",
                ));
            }
            let Some(arguments) = attribute
                .arguments
                .as_deref()
                .filter(|args| !args.is_empty())
            else {
                return Err(error(attribute.span, "#json requires field options"));
            };
            for argument in arguments {
                let span = argument.span;
                match (argument.name.as_deref(), &argument.value) {
                    (None, AttributeValue::Name(name))
                        if name.path == "skip" && name.arguments.is_none() =>
                    {
                        if skip.replace(span).is_some() {
                            return Err(error(span, "Duplicate #json skip"));
                        }
                    }
                    (Some("rename"), AttributeValue::Literal(Literal::String(key))) => {
                        if rename.replace((key, span)).is_some() {
                            return Err(error(span, "Duplicate #json rename"));
                        }
                    }
                    (Some("rename"), _) => {
                        return Err(error(
                            attribute.span,
                            "Expected #json rename to be a string",
                        ));
                    }
                    (Some("default"), value) => {
                        if !decoding {
                            return Err(error(span, "#json default requires a FromJson derive"));
                        }
                        let value = match value {
                            AttributeValue::Literal(literal) => Expr::Literal {
                                literal: literal.clone(),
                                span,
                            },
                            AttributeValue::Name(name) if name.arguments.is_none() => {
                                call(name.path.clone(), vec![], span)
                            }
                            _ => return Err(error(span, "Expected a literal or a zero-argument function name for #json default")),
                        };
                        if default.replace(value).is_some() {
                            return Err(error(span, "Duplicate #json default"));
                        }
                    }
                    (Some(name), _) => {
                        return Err(error(span, format!("Unknown #json option '{name}'")))
                    }
                    (None, _) => return Err(error(span, "Expected #json skip or a named option")),
                }
            }
        }
        if let Some(span) = skip {
            if rename.is_some() {
                return Err(error(span, "#json skip cannot be combined with rename"));
            }
            if decoding && default.is_none() {
                return Err(error(
                    span,
                    "#json skip requires a default when deriving FromJson",
                ));
            }
            fields.push(JsonField::Skipped { default });
            continue;
        }
        let (key, span) = rename.map_or((&field.name, None), |(key, span)| (key, Some(span)));
        if let Some(previous) = seen.insert(key, span) {
            if let Some(span) = span.or(previous) {
                return Err(error(span, format!("Duplicate JSON field name {key:?}")));
            }
        }
        fields.push(JsonField::Mapped {
            key: key.clone(),
            default,
        });
    }
    Ok(fields)
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

fn derive_parameters(
    parameters: &[Arc<TypeParameter>],
) -> (Vec<Arc<TypeParameter>>, Substitutions) {
    let type_params: Vec<_> = parameters
        .iter()
        .map(|p| {
            let mut parameter = TypeParameter::new(p.name.clone(), p.span);
            parameter.copy = p.copy;
            Arc::new(parameter)
        })
        .collect();
    let substitutions = parameters
        .iter()
        .zip(&type_params)
        .map(|(old, new)| (old.id, Type::Parameter(new.clone())))
        .collect();
    (type_params, substitutions)
}

fn string(text: &str, span: Span) -> Expr {
    Expr::Literal {
        literal: Literal::String(text.into()),
        span,
    }
}

fn start_object(encoding: bool, result: &str, span: Span) -> Expr {
    let object = if encoding {
        context_call("object", vec![], vec![], span)
    } else {
        context_call("read_object", vec![], vec![ident("$value", span)], span)
    };
    Expr::Var {
        name: "$fields".into(),
        annotation: None,
        value: Box::new(propagate(object, result, span)),
        span,
    }
}

fn finish_object(result: &str, span: Span) -> Expr {
    propagate(
        context_call("finish_object", vec![], vec![ident("$fields", span)], span),
        result,
        span,
    )
}

fn object_value(json: &str, span: Span) -> Expr {
    call(
        format!("{json}.Value.Object"),
        vec![ident("$fields", span)],
        span,
    )
}

fn ok(value: Expr, result: &str, span: Span) -> Expr {
    call(format!("{result}.Result.Ok"), vec![value], span)
}

fn implementation(
    record: &RecordDef,
    json_fields: &[JsonField],
    capability: &str,
    span: Span,
    imports: &JsonImports,
) -> ImplDef {
    let result = &imports.result;
    let (type_params, substitutions) = derive_parameters(&record.type_params);
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
    let encoding = capability == "ToJson";
    let mut body = vec![start_object(encoding, result, span)];
    for (index, (field, options)) in fields.iter().zip(json_fields).enumerate() {
        let span = field.span;
        let (key, default) = match options {
            JsonField::Mapped { key, default } => (key, default),
            JsonField::Skipped { default } => {
                if !encoding {
                    let fallback = default
                        .as_ref()
                        .expect("skipped decoder field has a default");
                    body.push(Expr::Let {
                        name: format!("$field_{index}"),
                        annotation: Some(field.ty.clone()),
                        value: Box::new(fallback.clone()),
                        span: fallback.span(),
                    });
                }
                continue;
            }
        };
        let mut args = vec![
            borrow(ident("$fields", span), true, span),
            string(key, span),
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
            let method = if let Some(fallback) = default {
                args.push(Expr::AnonymousFunction(Box::new(AnonymousFunction {
                    moving: false,
                    suspends: false,
                    params: vec![],
                    ret: Some(field.ty.clone()),
                    body: fallback.clone(),
                    span: fallback.span(),
                })));
                "take_field_or"
            } else {
                "take_field"
            };
            body.push(Expr::Let {
                name: format!("$field_{index}"),
                annotation: None,
                value: Box::new(propagate(
                    context_call(method, vec![field.ty.clone()], args, span),
                    result,
                    span,
                )),
                span,
            });
        }
    }
    let output = if encoding {
        object_value(&imports.json, span)
    } else {
        body.push(finish_object(result, span));
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
    body.push(ok(output, result, span));
    let obligations = fields
        .into_iter()
        .zip(json_fields)
        .filter_map(|(field, options)| matches!(options, JsonField::Mapped { .. }).then_some(field))
        .collect();
    mapping_impl(
        &record.name,
        type_params,
        obligations,
        body,
        encoding,
        imports,
        span,
    )
}

fn mapping_impl(
    name: &str,
    type_params: Vec<Arc<TypeParameter>>,
    fields: Vec<Param>,
    body: Vec<Expr>,
    encoding: bool,
    imports: &JsonImports,
    span: Span,
) -> ImplDef {
    let JsonImports {
        typed,
        json,
        result,
    } = imports;
    let target = if type_params.is_empty() {
        Type::Named(name.into())
    } else {
        applied(
            name.into(),
            type_params.iter().cloned().map(Type::Parameter).collect(),
        )
    };
    let capability = if encoding { "ToJson" } else { "FromJson" };
    let value_type = Type::Named(format!("{json}.Value"));
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
        derive_fields: Some(crate::parser::ast::DeriveFields {
            trait_name: format!("{typed}.{capability}"),
            fields,
        }),
        type_params,
        target,
        target_name: name.into(),
        associated_types: vec![],
        methods: vec![method],
        constants: vec![],
        span,
    }
}

/// Choose the borrow shape after substitution of generic fields and payloads.
pub(crate) fn prepare_encoding(function: &FnDef) -> std::borrow::Cow<'_, FnDef> {
    if function.kind != crate::parser::ast::FunctionKind::JsonEncode {
        return std::borrow::Cow::Borrowed(function);
    }
    let mut function = function.clone();
    let Expr::Block { exprs, .. } = &mut function.body else {
        unreachable!()
    };
    prepare_encoding_block(exprs);
    std::borrow::Cow::Owned(function)
}

fn prepare_encoding_block(exprs: &mut Vec<Expr>) {
    let mut body = Vec::new();
    for (index, mut expression) in std::mem::take(exprs).into_iter().enumerate() {
        if let Expr::Match {
            value, span, arms, ..
        } = &mut expression
        {
            if let Expr::MethodCall(call) = value.as_mut() {
                if call.name == "put_field" {
                    let Expr::Unary { operand, .. } = &call.args[2] else {
                        unreachable!()
                    };
                    let owner = call.type_args[0].as_type().is_some_and(Type::is_owner);
                    match operand.as_ref() {
                        // Borrowed enum matches already bind owning payloads by reference.
                        Expr::Ident { name, .. } if owner && name == "$payload" => {
                            call.args[2] = *operand.clone();
                        }
                        // Copy fields of an owning record must be read into a local first.
                        Expr::Field { .. } if !owner => {
                            let name = format!("$copy_{index}");
                            body.push(Expr::Let {
                                name: name.clone(),
                                annotation: None,
                                value: operand.clone(),
                                span: *span,
                            });
                            call.args[2] = borrow(ident(&name, *span), false, *span);
                        }
                        _ => {}
                    }
                }
            } else {
                for arm in arms {
                    if let Expr::Block { exprs, .. } = &mut arm.body {
                        prepare_encoding_block(exprs);
                    }
                }
            }
        }
        body.push(expression);
    }
    *exprs = body;
}
