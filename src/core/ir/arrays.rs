//! Contextual array construction uses the existing storage and cleanup contracts.
use super::*;
use crate::parser::ast::{ConstArgument, GenericArgument};

pub(super) fn element_type(ty: &Type) -> Option<&Type> {
    match ty {
        Type::FixedArray(a) => Some(&a.element),
        Type::Record(r) if r.array => r.arguments.first()?.as_type(),
        Type::BufferSlice(t) => Some(t),
        _ => None,
    }
}

impl FunctionPlan {
    pub(super) fn dynamic_array_type(
        &self,
        element: Type,
        types: &TypeRegistry,
        span: Span,
    ) -> Result<Type, Error> {
        let Some(Type::Record(template)) = self.named_type("$joky_array.Array") else {
            return Err(type_error(
                &span,
                ErrorCode::UnknownType,
                "Missing array literal library",
            ));
        };
        types.instantiate_record(template, vec![GenericArgument::Type(element)])
    }

    pub(super) fn array_element_hint(
        &self,
        elements: &[Expr],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        default: bool,
    ) -> Option<Type> {
        for use_default in [false, true].into_iter().take(if default { 2 } else { 1 }) {
            let mut local = vars.clone();
            for expr in elements {
                if let Some(ty) = self.hint(expr, &local, functions, types, use_default) {
                    return Some(ty);
                }
                self.hint_bindings(expr, &mut local, functions, types);
            }
        }
        None
    }

    pub(super) fn array_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let Expr::Array { elements, span } = expr else {
            unreachable!()
        };
        let element = expected
            .and_then(element_type)
            .cloned()
            .or_else(|| self.array_element_hint(elements, vars, functions, types, true))
            .ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Cannot infer array element type; add an array type annotation",
                )
            })?;
        if let Some(Type::FixedArray(shape)) = expected {
            if let ConstArgument::Value(length) = shape.length {
                if length != elements.len() as u64 {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Array literal has {} elements, expected {length}",
                            elements.len()
                        ),
                    ));
                }
            } else {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Array literal requires a concrete fixed length",
                ));
            }
            types
                .validate(expected.unwrap())
                .map_err(|e| e.with_span(*span))?;
            let token = self.slot(Type::FixedArrayInit(shape.clone()))?;
            self.emit(Op::Place(PlaceOp::Begin(token.clone())));
            let reference = self.slot(Type::reference(token.ty.clone(), true))?;
            self.emit(Op::Local {
                dst: reference.clone(),
                value: Some(token.clone()),
                target: token.ty.clone(),
            });
            self.local_scopes
                .last_mut()
                .unwrap()
                .push(reference.clone());
            for (index, expr) in elements.iter().enumerate() {
                let Flow::Value(value) =
                    self.expr_expected(expr, vars, functions, suspends, types, Some(&element))?
                else {
                    if let Some(next) = elements.get(index + 1) {
                        return Err(unreachable_error(next));
                    }
                    return Ok(Flow::Diverged);
                };
                if slot_type(&value) != element {
                    return Err(type_error(
                        &expr.span(),
                        ErrorCode::TypeMismatch,
                        format!("Array element expects {element}, got {}", slot_type(&value)),
                    ));
                }
                self.emit(Op::Place(PlaceOp::WriteNext {
                    reference: reference.clone(),
                    value,
                }));
            }
            let result = self.slot(Type::FixedArray(shape.clone()))?;
            self.emit(Op::Place(PlaceOp::MoveInit {
                dst: result.clone(),
                src: token,
                publish: true,
            }));
            return Ok(Flow::Value(Some(result)));
        }
        // Each element is evaluated before borrowing the growing owner. This
        // permits suspension in an element without retaining an exclusive loan.
        let name = format!("$array_literal_{}", span.start());
        let ident = |name: String| Expr::Ident { name, span: *span };
        let Flow::Value(Some(array)) = self.expr(
            &Expr::Call {
                callee: "$joky_array.with_capacity".into(),
                type_args: vec![GenericArgument::Type(element.clone())],
                args: vec![Expr::Literal {
                    literal: Literal::UInt64(elements.len() as u64),
                    span: *span,
                }],
                span: *span,
            },
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            unreachable!()
        };
        for (index, expr) in elements.iter().enumerate() {
            let Flow::Value(value) =
                self.expr_expected(expr, vars, functions, suspends, types, Some(&element))?
            else {
                if let Some(next) = elements.get(index + 1) {
                    return Err(unreachable_error(next));
                }
                return Ok(Flow::Diverged);
            };
            if slot_type(&value) != element {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::TypeMismatch,
                    format!("Array element expects {element}, got {}", slot_type(&value)),
                ));
            }
            let temporary = format!("{name}_element_{index}");
            let mut local = vars.clone();
            local.insert(
                name.clone(),
                Binding {
                    slot: Some(array.clone()),
                    mutable: true,
                    place: None,
                },
            );
            local.insert(
                temporary.clone(),
                Binding {
                    slot: value,
                    mutable: false,
                    place: None,
                },
            );
            self.expr(
                &Expr::Call {
                    callee: "$joky_array.push".into(),
                    type_args: vec![GenericArgument::Type(element.clone())],
                    args: vec![
                        Expr::Unary {
                            op: UnaryOp::BorrowMut,
                            operand: Box::new(ident(name.clone())),
                            span: *span,
                        },
                        ident(temporary),
                    ],
                    span: expr.span(),
                },
                &mut local,
                functions,
                suspends,
                types,
            )?;
        }
        Ok(Flow::Value(Some(array)))
    }
}
