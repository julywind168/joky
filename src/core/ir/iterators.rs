//! Typed for-input conversion and compiler-owned borrowed cursor steps.
use super::*;
use crate::parser::ast::{MatchArm, MethodCall, Pattern, PayloadPattern};

impl FunctionPlan {
    pub(super) fn iteration_input(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Expr> {
        let Expr::Call {
            callee, args, span, ..
        } = expr
        else {
            return None;
        };
        if callee != "$for_into_iter" {
            return None;
        }
        let [value] = args.as_slice() else {
            unreachable!("compiler-authored for input")
        };
        if let Some(Type::Reference(reference)) = self.type_hint(value, vars, functions, types) {
            match &reference.target {
                Type::BufferIterator(_) | Type::BufferPairIterator(_) => {
                    return Some(value.clone())
                }
                Type::BufferSlice(_) => {
                    return Some(Expr::Call {
                        callee: format!(
                            "$joky_iter.slice_iter{}",
                            if reference.mutable { "_mut" } else { "" }
                        ),
                        type_args: vec![],
                        args: vec![value.clone()],
                        span: *span,
                    })
                }
                target => {
                    // Match the embedded container declaration, not a user's
                    // spelling or an arbitrary method named iter.
                    let standard = matches!(target, Type::Record(record) if record.array)
                        || self.generics.borrow().members.methods.iter().any(|method| {
                            method.function == "joky/map.Map.iter"
                                && members::same_owner(target, &method.target)
                        });
                    if standard {
                        return Some(Expr::MethodCall(Box::new(MethodCall {
                            receiver: value.clone(),
                            name: if reference.mutable {
                                "iter_mut"
                            } else {
                                "iter"
                            }
                            .into(),
                            type_args: vec![],
                            args: vec![],
                            span: *span,
                        })));
                    }
                }
            }
        }
        Some(Expr::Call {
            callee: "$joky_iter.into_iter".into(),
            type_args: vec![],
            args: vec![value.clone()],
            span: *span,
        })
    }

    pub(super) fn borrowed_iteration(
        &self,
        value: &Expr,
        arms: &[MatchArm],
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        span: Span,
    ) -> Option<Expr> {
        // Only the parser-authored step has this shape. Ordinary Iterator calls
        // and user matches retain the protocol's Option and @noescape contract.
        let Expr::Call { args, .. } = value else {
            return None;
        };
        let [Expr::Unary { operand, .. }] = args.as_slice() else {
            return None;
        };
        let Type::Reference(reference) = self.type_hint(operand, vars, functions, types)? else {
            return None;
        };
        if !matches!(
            reference.target,
            Type::BufferIterator(_) | Type::BufferPairIterator(_)
        ) {
            return None;
        }
        let [success, fallback] = arms else {
            return None;
        };
        let Pattern::Variant {
            binding: Some(binding),
            ..
        } = &success.pattern
        else {
            return None;
        };
        let PayloadPattern::Name {
            name,
            span: binding_span,
        } = binding.as_ref()
        else {
            return None;
        };
        let paired = matches!(reference.target, Type::BufferPairIterator(_));
        let call = |method: &str| Expr::Call {
            callee: format!(
                "$joky_iter.{}{method}",
                if paired { "entries_" } else { "" }
            ),
            type_args: vec![],
            args: vec![*operand.clone()],
            span: value.span(),
        };
        // References stay in direct local bindings, never in Option payloads.
        // Calls use the same provenance, reborrow and suspension checks as any
        // explicit use of advance/current; the for syntax grants no exemptions.
        Some(Expr::If {
            condition: Box::new(call("advance")),
            then_branch: Box::new(Expr::Sequence {
                exprs: vec![
                    Expr::Let {
                        name: name.clone(),
                        annotation: None,
                        value: Box::new(call(if reference.mutable {
                            "current_mut"
                        } else {
                            "current"
                        })),
                        span: *binding_span,
                    },
                    success.body.clone(),
                ],
                trailing: true,
                span,
            }),
            else_branch: Some(Box::new(fallback.body.clone())),
            span,
        })
    }
}

impl FunctionPlan {
    pub(super) fn bind_entry_pattern(
        &mut self,
        pattern: &crate::parser::ast::BindingPattern,
        entry: &Slot,
        binding_mutable: bool,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        use crate::parser::ast::BindingPattern;
        let BindingPattern::Tuple { elements, span } = pattern else {
            return Err(type_error(
                &pattern.span(),
                ErrorCode::InvalidPattern,
                "Entry destructuring requires a (key, value) pattern",
            ));
        };
        if elements.len() != 2
            || elements.iter().any(|p| {
                !matches!(
                    p,
                    BindingPattern::Name { .. } | BindingPattern::Discard { .. }
                )
            })
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidPattern,
                "Entry patterns require exactly two names or discards",
            ));
        }
        let borrowed = entry.ty.reference_type().unwrap();
        let Type::BufferEntry(pair) = &borrowed.target else {
            unreachable!()
        };
        let Type::Tuple(pair) = pair.as_ref() else {
            unreachable!()
        };
        for (column, pattern) in elements.iter().enumerate() {
            if let BindingPattern::Name { name, span } = pattern {
                self.current_span = *span;
                let dst = self.slot(Type::reference(
                    pair.elements[column].clone(),
                    column == 1 && borrowed.mutable,
                ))?;
                self.emit(Op::EntryProject {
                    dst: dst.clone(),
                    src: entry.clone(),
                    column,
                });
                let binding = self.local_binding(
                    name,
                    Binding {
                        slot: Some(dst),
                        mutable: binding_mutable,
                        place: None,
                    },
                )?;
                vars.insert(name.clone(), binding);
            }
        }
        Ok(())
    }
}
