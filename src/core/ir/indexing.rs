//! Checked indexing reuses element views and the ordinary memory place planner.
use super::*;

pub(super) fn is_index_place(mut expr: &Expr) -> bool {
    while let Expr::Field { base, .. } = expr {
        expr = base;
    }
    matches!(expr, Expr::Index { .. })
}

/// Only parser-authored bounded range expressions select slicing. Iterator
/// values and ordinary function calls remain ordinary index operands.
pub(super) fn slice_bounds(expr: &Expr) -> Option<(&Expr, &Expr, bool)> {
    let Expr::Call {
        callee,
        args,
        type_args,
        ..
    } = expr
    else {
        return None;
    };
    if !type_args.is_empty() || args.len() != 2 {
        return None;
    }
    match callee.as_str() {
        "$joky_iter.range" => Some((&args[0], &args[1], false)),
        "$joky_iter.range_inclusive" => Some((&args[0], &args[1], true)),
        _ => None,
    }
}

pub(super) enum IndexAccess {
    Read,
    Place { mutable: bool },
}

impl FunctionPlan {
    pub(super) fn index_access(
        &mut self,
        expr: &Expr,
        access: IndexAccess,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::Index { base, index, span } = expr else {
            unreachable!()
        };
        let mutable = matches!(access, IndexAccess::Place { mutable: true });
        let hint = self.type_hint(base, vars, functions, types);
        let mut base_place = base.as_ref();
        while let Expr::Field { base, .. } = base_place {
            base_place = base;
        }
        let flow = if hint
            .as_ref()
            .is_some_and(|ty| ty.reference_type().is_some())
        {
            self.expr(base, vars, functions, suspends, types)?
        } else if matches!(
            base_place,
            Expr::Ident { .. }
                | Expr::Index { .. }
                | Expr::Unary {
                    op: UnaryOp::Deref,
                    ..
                }
        ) {
            self.expr(
                &Expr::Unary {
                    op: if mutable {
                        UnaryOp::BorrowMut
                    } else {
                        UnaryOp::Borrow
                    },
                    operand: base.clone(),
                    span: base.span(),
                },
                vars,
                functions,
                suspends,
                types,
            )?
        } else {
            if mutable {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Mutable indexing requires a stable array or an exclusive reference",
                ));
            }
            let Flow::Value(value) = self.expr(base, vars, functions, suspends, types)? else {
                return Ok(Flow::Diverged);
            };
            let value = value.ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Indexing requires an array or slice",
                )
            })?;
            if super::arrays::element_type(&value.ty).is_none() {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Indexing requires an array or slice, got {}", value.ty),
                ));
            }
            Flow::Value(Some(self.sequence_value_reference(&value, false)?))
        };
        let Flow::Value(Some(mut root)) = flow else {
            return Ok(Flow::Diverged);
        };
        let reference = root.ty.reference_type().ok_or_else(|| {
            type_error(
                span,
                ErrorCode::TypeMismatch,
                "Indexing requires an array or slice reference",
            )
        })?;
        if mutable && !reference.mutable {
            return Err(type_error(
                span,
                ErrorCode::InvalidAssignment,
                "Cannot mutate an element through a shared reference",
            ));
        }
        let target = self.pointee(&reference.target, types)?;
        let element = super::arrays::element_type(&target)
            .cloned()
            .ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Indexing requires an array or slice, got {target}"),
                )
            })?;
        let load = match &target {
            Type::Record(r) if r.array => Some(crate::parser::ast::BufferIntrinsic::Load),
            Type::BufferSlice(_) => Some(crate::parser::ast::BufferIntrinsic::SliceLoad),
            _ => None,
        };
        root.ty = Type::reference(target, mutable);
        if let Some((start, end, inclusive)) = slice_bounds(index) {
            // A later bound may rebind the source reference variable.
            root = self.reference_value(Some(root), false)?.unwrap();
            let mut bounds = Vec::with_capacity(2);
            for bound in [start, end] {
                let Flow::Value(value) = self.expr_expected(
                    bound,
                    vars,
                    functions,
                    suspends,
                    types,
                    Some(&Type::UInt64),
                )?
                else {
                    return Ok(Flow::Diverged);
                };
                if slot_type(&value) != Type::UInt64 {
                    return Err(type_error(
                        &bound.span(),
                        ErrorCode::TypeMismatch,
                        "Slice bounds must have type UInt64",
                    ));
                }
                // Freeze the start before effects in the end expression.
                bounds.push(self.snapshot(&value)?.unwrap());
            }
            let mut end = bounds.pop().unwrap();
            let start = bounds.pop().unwrap();
            if inclusive {
                let mut locals = HashMap::new();
                for (name, slot) in [("$slice_start", &start), ("$slice_end", &end)] {
                    locals.insert(
                        name.into(),
                        Binding {
                            slot: Some(slot.clone()),
                            mutable: false,
                            place: None,
                        },
                    );
                }
                let ident = |name: &str| Expr::Ident {
                    name: name.into(),
                    span: *span,
                };
                let guard = Expr::If {
                    condition: Box::new(Expr::Logical {
                        op: crate::parser::ast::LogicalOp::Or,
                        lhs: Box::new(Expr::BinOp {
                            op: BinOp::Gt,
                            lhs: Box::new(ident("$slice_start")),
                            rhs: Box::new(ident("$slice_end")),
                            span: *span,
                        }),
                        rhs: Box::new(Expr::BinOp {
                            op: BinOp::Eq,
                            lhs: Box::new(ident("$slice_end")),
                            rhs: Box::new(Expr::Literal {
                                literal: Literal::UInt64(u64::MAX),
                                span: *span,
                            }),
                            span: *span,
                        }),
                        span: *span,
                    }),
                    then_branch: Box::new(Expr::Call {
                        callee: "$joky_check.panic".into(),
                        type_args: vec![],
                        args: vec![Expr::Literal {
                            literal: Literal::String("Invalid inclusive slice bounds".into()),
                            span: *span,
                        }],
                        span: *span,
                    }),
                    else_branch: None,
                    span: *span,
                };
                self.expr(&guard, &mut locals, functions, suspends, types)?;
                self.current_span = *span;
                let one = self.pattern_u64(1)?;
                let exclusive = self.slot(Type::UInt64)?;
                self.emit(Op::Binary(exclusive.clone(), BinOp::Add, end, one));
                end = exclusive;
            }
            self.current_span = *span;
            return Ok(Flow::Value(Some(
                self.sequence_slice(&root, &element, start, end, mutable)?,
            )));
        }
        let Flow::Value(index_value) =
            self.expr_expected(index, vars, functions, suspends, types, Some(&Type::UInt64))?
        else {
            return Ok(Flow::Diverged);
        };
        let index_value = index_value
            .filter(|s| s.ty == Type::UInt64)
            .ok_or_else(|| {
                type_error(
                    &index.span(),
                    ErrorCode::TypeMismatch,
                    "Array index must have type UInt64",
                )
            })?;
        self.current_span = *span;
        let read_type = match access {
            IndexAccess::Read => Some(self.pointee(&element, types)?),
            IndexAccess::Place { .. } => None,
        };
        if let (Some(load), Some(ty @ Type::Enum(e))) = (load, &read_type) {
            if super::enum_arrays::candidate(e) {
                // Load checks the index and copies the value without lending an element address.
                return Ok(Flow::Value(self.sequence_storage(
                    load,
                    ty,
                    vec![root, index_value],
                    ty.clone(),
                )?));
            }
        }
        // Arbitrary index projections retain the original storage loan.
        let raw = self.sequence_element(&root, &element, index_value, mutable)?;
        let mut reference = self.pattern_reference(&raw, &element, 0, mutable, &[])?;
        if let Some(ty) = read_type {
            reference.ty = Type::reference(ty, false);
            self.read_copy_place(reference, *span)
        } else {
            Ok(Flow::Value(Some(reference)))
        }
    }
}
