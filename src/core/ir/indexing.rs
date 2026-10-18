//! Checked indexing reuses element views and the ordinary memory place planner.
use super::*;

pub(super) fn is_index_place(mut expr: &Expr) -> bool {
    while let Expr::Field { base, .. } = expr {
        expr = base;
    }
    matches!(expr, Expr::Index { .. })
}

impl FunctionPlan {
    pub(super) fn index_reference(
        &mut self,
        expr: &Expr,
        mutable: bool,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let Expr::Index { base, index, span } = expr else {
            unreachable!()
        };
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
        root.ty = Type::reference(target, mutable);
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
        // Unlike sequence patterns, arbitrary indices never establish disjoint
        // projection paths. Every element retains the original storage loan.
        self.current_span = *span;
        let raw = self.sequence_element(&root, &element, index_value, mutable)?;
        let reference = self.pattern_reference(&raw, &element, 0, mutable, &[])?;
        Ok(Flow::Value(Some(reference)))
    }
}
