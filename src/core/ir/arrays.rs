//! Contextual array construction uses the existing storage and cleanup contracts.
use super::*;
use crate::core::types::{ConstArgument, GenericArgument};

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
            let hint = super::inference::sibling_hint(
                elements.iter().map(|expr| {
                    let hint = match expr {
                        Expr::Spread { value, .. } => {
                            if let Expr::Array { elements, .. } = value.as_ref() {
                                self.array_element_hint(
                                    elements,
                                    &local,
                                    functions,
                                    types,
                                    use_default,
                                )
                            } else {
                                self.hint(value, &local, functions, types, use_default)
                                    .and_then(|ty| spread_element_type(&ty).cloned())
                            }
                        }
                        _ => self.hint(expr, &local, functions, types, use_default),
                    };
                    self.hint_bindings(expr, &mut local, functions, types);
                    hint
                }),
                use_default,
            );
            if hint.is_some() {
                return hint;
            }
        }
        None
    }

    pub(super) fn array_static_length(
        &self,
        elements: &[Expr],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<u64> {
        let mut local = vars.clone();
        let mut length = 0u64;
        for expr in elements {
            let count = if let Expr::Spread { value, .. } = expr {
                let ty = self.hint(value, &local, functions, types, true)?;
                let ty = ty.reference_type().map_or(&ty, |r| &r.target);
                let Type::FixedArray(a) = ty else { return None };
                let ConstArgument::Value(n) = a.length else {
                    return None;
                };
                n
            } else {
                1
            };
            length = length.checked_add(count)?;
            self.hint_bindings(expr, &mut local, functions, types);
        }
        Some(length)
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
        let context = ArrayContext {
            functions,
            suspends,
            types,
        };
        let (array, fixed_reference) = if let Some(Type::FixedArray(shape)) = expected {
            let ConstArgument::Value(length) = shape.length else {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Array literal requires a concrete fixed length",
                ));
            };
            let actual = self
                .array_static_length(elements, vars, functions, types)
                .ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "FixedArray literal requires statically known spread lengths",
                    )
                })?;
            if actual != length {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Array literal has {actual} elements, expected {length}"),
                ));
            }
            types
                .validate(expected.unwrap())
                .map_err(|e| e.with_span(*span))?;
            let token = self.slot(Type::FixedArrayInit(shape.clone()))?;
            self.emit(Op::Place(PlaceOp::Begin(token.clone())));
            let reference = self.sequence_value_reference(&token, true)?;
            (token, Some(reference))
        } else {
            let capacity = self.pattern_u64(
                elements
                    .iter()
                    .filter(|e| !matches!(e, Expr::Spread { .. }))
                    .count() as u64,
            )?;
            let value = self
                .array_library_call(
                    "with_capacity",
                    &element,
                    vec![Some(capacity)],
                    &context,
                    *span,
                )?
                .unwrap();
            (value, None)
        };
        for (index, expr) in elements.iter().enumerate() {
            let (value_expr, expected) = if let Expr::Spread { value, .. } = expr {
                (
                    value.as_ref(),
                    self.hint(value, vars, functions, types, false)
                        .map(Ok)
                        .unwrap_or_else(|| {
                            self.dynamic_array_type(element.clone(), types, expr.span())
                        })?,
                )
            } else {
                (expr, element.clone())
            };
            let Flow::Value(value) = self.expr_expected(
                value_expr,
                vars,
                functions,
                suspends,
                types,
                Some(&expected),
            )?
            else {
                if let Some(next) = elements.get(index + 1) {
                    return Err(unreachable_error(next));
                }
                return Ok(Flow::Diverged);
            };
            self.current_span = expr.span();
            let destination = ArrayDestination {
                array: &array,
                fixed_reference: fixed_reference.as_ref(),
            };
            if matches!(expr, Expr::Spread { .. }) {
                self.array_spread(value, &destination, &element, &context)?;
            } else {
                if slot_type(&value) != element {
                    return Err(type_error(
                        &expr.span(),
                        ErrorCode::TypeMismatch,
                        format!("Array element expects {element}, got {}", slot_type(&value)),
                    ));
                }
                if let Some(reference) = &fixed_reference {
                    self.emit(Op::Place(PlaceOp::WriteNext {
                        reference: reference.clone(),
                        value,
                    }));
                } else {
                    // Evaluate the element before taking an exclusive destination loan.
                    let root = self.sequence_value_reference(&array, true)?;
                    self.array_library_call(
                        "push",
                        &element,
                        vec![Some(root), value],
                        &context,
                        expr.span(),
                    )?;
                }
            }
        }
        if let Some(Type::FixedArray(shape)) = expected {
            let result = self.slot(Type::FixedArray(shape.clone()))?;
            self.emit(Op::Place(PlaceOp::MoveInit {
                dst: result.clone(),
                src: array,
                publish: true,
            }));
            Ok(Flow::Value(Some(result)))
        } else {
            Ok(Flow::Value(Some(array)))
        }
    }

    fn array_library_call(
        &mut self,
        name: &str,
        element: &Type,
        args: Vec<Option<Slot>>,
        context: &ArrayContext<'_>,
        span: Span,
    ) -> Result<Option<Slot>, Error> {
        let mut local = HashMap::new();
        let args = args
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                let name = format!("$array_argument_{i}");
                local.insert(
                    name.clone(),
                    Binding {
                        slot,
                        mutable: false,
                        place: None,
                    },
                );
                Expr::Ident { name, span }
            })
            .collect();
        let Flow::Value(value) = self.expr(
            &Expr::Call {
                callee: format!("$joky_array.{name}"),
                type_args: vec![GenericArgument::Type(element.clone())],
                args,
                span,
            },
            &mut local,
            context.functions,
            context.suspends,
            context.types,
        )?
        else {
            unreachable!()
        };
        Ok(value)
    }

    fn array_spread(
        &mut self,
        value: Option<Slot>,
        destination: &ArrayDestination<'_>,
        element: &Type,
        context: &ArrayContext<'_>,
    ) -> Result<(), Error> {
        use crate::parser::ast::BufferIntrinsic as BI;
        let ty = slot_type(&value);
        if spread_element_type(&ty) != Some(element) {
            return Err(type_error(
                &self.current_span,
                ErrorCode::TypeMismatch,
                format!(
                    "Cannot spread {ty}: expected an array or slice with element type {element}"
                ),
            ));
        }
        let moving = matches!(&ty, Type::Record(r) if r.array);
        if !moving && !element.is_copy_value() {
            return Err(type_error(
                &self.current_span,
                ErrorCode::TypeMismatch,
                "Borrowed array spread requires Copy elements; move the array or clone explicitly",
            ));
        }
        self.local_scopes.push(Vec::new());
        // A private snapshot consumes even an empty Array and freezes a Copy
        // FixedArray before any following entry can mutate its original binding.
        let value = value.unwrap();
        let root = if ty.reference_type().is_some() {
            self.reference_value(Some(value), false)?.unwrap()
        } else {
            let value = self.snapshot(&Some(value))?.unwrap();
            self.sequence_value_reference(&value, moving)?
        };
        let length = self.sequence_len(&root, element)?;
        let target = if let Some(fixed) = destination.fixed_reference {
            fixed.clone()
        } else {
            let reference = self.sequence_value_reference(destination.array, true)?;
            self.array_library_call(
                "reserve",
                element,
                vec![Some(reference), Some(length.clone())],
                context,
                self.current_span,
            )?;
            self.sequence_value_reference(destination.array, true)?
        };
        let one = self.pattern_u64(1)?;
        if moving {
            // Reverse the private source once, then take_last transfers elements
            // in source order. Every intermediate state remains fully droppable.
            let half = self.slot(Type::UInt64)?;
            let two = self.pattern_u64(2)?;
            self.emit(Op::Binary(half.clone(), BinOp::Div, length.clone(), two));
            let index = self.pattern_u64(0)?;
            let (header, exit) = self.array_loop(&index, half)?;
            let end = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(
                end.clone(),
                BinOp::Sub,
                length.clone(),
                one.clone(),
            ));
            let opposite = self.slot(Type::UInt64)?;
            self.emit(Op::Binary(opposite.clone(), BinOp::Sub, end, index.clone()));
            self.sequence_storage(
                BI::SwapElements,
                element,
                vec![root.clone(), index.clone(), opposite],
                Type::Unit,
            )?;
            self.array_loop_end(index, &one, header, exit)?;
        }
        let index = self.pattern_u64(0)?;
        let (header, exit) = self.array_loop(&index, length)?;
        self.local_scopes.push(Vec::new());
        let value = if moving {
            self.sequence_storage(BI::TakeLast, element, vec![root], element.clone())?
        } else {
            let raw = self.sequence_element(&root, element, index.clone(), false)?;
            let reference = self.pattern_reference(&raw, element, 0, false, &[])?;
            self.read_local(reference)?
        };
        if destination.fixed_reference.is_some() {
            self.emit(Op::Place(PlaceOp::WriteNext {
                reference: target,
                value,
            }));
        } else {
            let value = match value {
                Some(value) => value,
                None => self.slot(Type::Unit)?,
            };
            self.sequence_storage(BI::InitializeNext, element, vec![target, value], Type::Unit)?;
        }
        // Temporary element places belong to one iteration; the private source
        // belongs to this spread, rather than the surrounding literal or block.
        self.end_local_scope();
        self.array_loop_end(index, &one, header, exit)?;
        self.end_local_scope();
        Ok(())
    }

    fn array_loop(&mut self, index: &Slot, length: Slot) -> Result<(usize, usize), Error> {
        let header = self.label_count;
        let exit = header + 1;
        self.label_count += 2;
        self.emit(Op::Label(header));
        self.pattern_compare(index.clone(), length, BinOp::Lt, exit)?;
        Ok((header, exit))
    }

    fn array_loop_end(
        &mut self,
        index: Slot,
        one: &Slot,
        header: usize,
        exit: usize,
    ) -> Result<(), Error> {
        let next = self.slot(Type::UInt64)?;
        self.emit(Op::Binary(
            next.clone(),
            BinOp::Add,
            index.clone(),
            one.clone(),
        ));
        self.emit(Op::Copy(index, next));
        self.emit(Op::Jump(header));
        self.emit(Op::Label(exit));
        Ok(())
    }
}

pub(super) fn spread_element_type(ty: &Type) -> Option<&Type> {
    element_type(ty.reference_type().map_or(ty, |r| &r.target))
}

struct ArrayContext<'a> {
    functions: &'a HashMap<String, FunctionInfo>,
    types: &'a TypeRegistry,
    suspends: bool,
}

struct ArrayDestination<'a> {
    array: &'a Slot,
    fixed_reference: Option<&'a Slot>,
}
