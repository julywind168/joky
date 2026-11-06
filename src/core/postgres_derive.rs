//! Decode struct fields by SQL output name using ordinary FromSql calls.
use crate::core::types::{ParameterKind, Type, TypeApplication, TypeParameter};
use crate::parser::ast::{
    DeriveFields, Expr, FieldInit, FnDef, FunctionKind, ImplDef, Literal, MethodCall, Module,
    NoEscape, Param, UnaryOp,
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::sync::Arc;

pub(super) fn expand(module: &mut Module) -> Result<(), Error> {
    let mut requests = Vec::new();
    for record in &mut module.records {
        if let Some(span) = super::clone_derive::take(&mut record.attributes, "FromRow")? {
            requests.push((record.clone(), span));
        }
    }
    for enumeration in &mut module.enums {
        if let Some(span) = super::clone_derive::take(&mut enumeration.attributes, "FromRow")? {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::TypeMismatch,
                "FromRow derives require a struct",
            )
            .with_span(span));
        }
    }
    if requests.is_empty() {
        return Ok(());
    }
    let span = requests[0].1;
    let postgres = super::json_derive::import(module, "joky/postgres", "$row_derive", span);
    let result = super::json_derive::import(module, "joky/result", "$row_result", span);
    for (record, span) in requests {
        if record
            .type_params
            .iter()
            .any(|p| p.kind == ParameterKind::Const)
        {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::TypeMismatch,
                "Derive with const parameters requires unsupported generic trait implementations",
            )
            .with_span(span));
        }
        let type_params: Vec<_> = record
            .type_params
            .iter()
            .map(|p| {
                let mut parameter = TypeParameter::new(p.name.clone(), p.span);
                parameter.copy = p.copy;
                Arc::new(parameter)
            })
            .collect();
        let substitutions = record
            .type_params
            .iter()
            .zip(&type_params)
            .map(|(old, new)| (old.id, Type::Parameter(new.clone())))
            .collect();
        let fields: Vec<_> = record
            .fields
            .iter()
            .map(|field| Param {
                mutable: false,
                name: field.name.clone(),
                ty: crate::core::types::substitute_syntax(&field.ty, &substitutions),
                span: field.span,
            })
            .collect();
        let target = if type_params.is_empty() {
            Type::Named(record.name.clone())
        } else {
            Type::Applied(Arc::new(TypeApplication {
                name: record.name.clone(),
                arguments: type_params.iter().map(|p| p.argument()).collect(),
            }))
        };
        let value = Expr::Record {
            base: None,
            name: record.name.clone(),
            type_args: type_params.iter().map(|p| p.argument()).collect(),
            fields: fields
                .iter()
                .map(|field| FieldInit {
                    name: field.name.clone(),
                    value: Expr::Unary {
                        op: UnaryOp::Try,
                        operand: Box::new(Expr::MethodCall(Box::new(MethodCall {
                            receiver: Expr::Ident {
                                name: "$row".into(),
                                span: field.span,
                            },
                            name: "get".into(),
                            type_args: vec![field.ty.clone().into()],
                            args: vec![Expr::Literal {
                                literal: Literal::String(field.name.clone()),
                                span: field.span,
                            }],
                            span: field.span,
                        }))),
                        span: field.span,
                    },
                    span: field.span,
                })
                .collect(),
            span,
        };
        module.implementations.push(ImplDef {
            derive_fields: Some(DeriveFields {
                trait_name: format!("{postgres}.FromSql"),
                fields,
            }),
            trait_name: Some(format!("{postgres}.FromRow")),
            origin: None,
            scope: None,
            type_params,
            target: target.clone(),
            target_name: record.name,
            associated_types: vec![],
            methods: vec![FnDef {
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
                name: "from_row".into(),
                type_params: vec![],
                params: vec![Param {
                    mutable: false,
                    name: "$row".into(),
                    ty: Type::reference(Type::Named(format!("{postgres}.Row")), false),
                    span,
                }],
                ret_ty: Type::Applied(Arc::new(TypeApplication {
                    name: format!("{result}.Result"),
                    arguments: vec![
                        target.into(),
                        Type::Named(format!("{postgres}.DbError")).into(),
                    ],
                })),
                suspends: false,
                body: Expr::Call {
                    callee: format!("{result}.Result.Ok"),
                    type_args: vec![],
                    args: vec![value],
                    span,
                },
                span,
            }],
            constants: vec![],
            span,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::prepared::check_source;

    #[test]
    fn postgres_derive_rejects_invalid_attributes_fields_and_conflicting_impls() {
        for (source, message, field) in [
            ("#derive(FromRow, FromRow) struct Bad {}", "Duplicate FromRow", None),
            ("#derive(FromRow(1)) struct Bad {}", "takes no arguments", None),
            ("#derive(FromRow) enum Bad { A }", "require a struct", None),
            ("#derive(FromRow) struct Bad<const N: UInt64> {}", "const parameters", None),
            ("#derive(FromRow) struct Bad { value: Int32 }", "Cannot derive", Some("value: Int32")),
            ("#derive(FromRow) struct Bad { value: &String }", "Cannot derive", Some("value: &String")),
            (
                "import joky/postgres #derive(FromRow) struct Bad {} impl postgres.FromRow for Bad { fn from_row(row: &postgres.Row) @noescape -> Result<Bad, postgres.DbError> { Ok(Bad {}) } }",
                "Overlapping", None,
            ),
        ] {
            let error = check_source(source, &Default::default()).unwrap_err();
            assert!(error.message().contains(message), "{source}: {error}");
            if let Some(field) = field {
                assert_eq!(error.span().unwrap().slice(source), Some(field));
            }
        }
    }

    #[test]
    fn postgres_derive_infers_field_protocol_and_preserves_import_scope() {
        let source = r#"
            import joky/postgres as pg
            trait FromRow { fn unrelated(&self) -> Int32 }
            #derive(FromRow, Clone, Eq, ToJson)
            struct Entry<T> { id: T, text: String, flag: Bool }
            fn mapped(row: &pg.Row) -> Result<Entry<Int64>, pg.DbError> {
                pg.FromRow.from_row::<Entry<Int64>>(row)
            }
        "#;
        check_source(source, &Default::default()).unwrap_or_else(|error| panic!("{error}"));
        let error = check_source(
            &source.replace("Entry<Int64>", "Entry<Int32>"),
            &Default::default(),
        )
        .unwrap_err();
        assert!(error.message().contains("joky/postgres.FromRow"), "{error}");

        let registry = crate::TypeRegistry::new().with_prelude(false);
        let parsed = crate::parser::parse("#derive(FromRow) struct Row<T> { value: T }").unwrap();
        let first = registry
            .resolve_aliases(&parsed)
            .unwrap_or_else(|error| panic!("{error}"));
        let second = registry
            .resolve_aliases(&first)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(first.implementations.len(), second.implementations.len());
    }
}
