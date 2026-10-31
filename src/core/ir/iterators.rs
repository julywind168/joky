//! Typed for-input conversion and compiler-owned borrowed cursor steps.
use super::*;
use crate::parser::ast::{BindingPattern, MatchArm, MethodCall, Pattern, PayloadPattern};

/// Only authored by borrowed_iteration, immediately after a successful advance.
pub(super) fn paired_binding(value: &Expr) -> Option<&Expr> {
    let Expr::Call { callee, args, .. } = value else {
        return None;
    };
    (callee == "$for_pair_binding").then(|| {
        let [cursor] = args.as_slice() else {
            unreachable!("compiler-authored paired binding")
        };
        cursor
    })
}

pub(super) fn exclusive_plan_input(input: &Expr) -> Option<&str> {
    let Expr::Sequence { exprs, .. } = input else {
        return None;
    };
    let Expr::Destructure {
        pattern: BindingPattern::Tuple { elements, .. },
        ..
    } = exprs.first()?
    else {
        return None;
    };
    let BindingPattern::Name { name, .. } = elements.get(1)? else {
        return None;
    };
    name.starts_with("$exclusive_plan_")
        .then_some(name.as_str())
}

impl FunctionPlan {
    fn is_exclusive_pipeline(&self, ty: &Type) -> bool {
        self.generics.borrow().members.methods.iter().any(|method| {
            method.function == "joky/iter.ExclusivePipeline.into_parts"
                && members::same_owner(ty, &method.target)
        })
    }
    fn is_shared_iterator(&self, ty: &Type) -> bool {
        let context = self.generics.borrow();
        let traits = &context.members.traits;
        traits
            .definitions
            .iter()
            .find(|definition| definition.name == "joky/iter.SharedIterator")
            .is_some_and(|definition| {
                traits
                    .satisfies(ty, definition.self_parameter.id)
                    .unwrap_or(false)
            })
    }

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
        if let Some(ty) = self.type_hint(value, vars, functions, types) {
            if self.is_exclusive_pipeline(&ty) {
                let Expr::Ident { name, .. } = value else {
                    return None;
                };
                let suffix = name.strip_prefix("$for_input_")?;
                let cursor = format!("$exclusive_cursor_{suffix}");
                let plan = format!("$exclusive_plan_{suffix}");
                return Some(Expr::Sequence {
                    exprs: vec![
                        Expr::Destructure {
                            pattern: BindingPattern::Tuple {
                                elements: [cursor.as_str(), plan.as_str()]
                                    .into_iter()
                                    .map(|name| BindingPattern::Name {
                                        name: name.into(),
                                        span: *span,
                                    })
                                    .collect(),
                                span: *span,
                            },
                            mutable: true,
                            annotation: None,
                            value: Box::new(Expr::Call {
                                callee: "$joky_iter.exclusive_parts".into(),
                                type_args: vec![],
                                args: vec![value.clone()],
                                span: *span,
                            }),
                            span: *span,
                        },
                        Expr::Ident {
                            name: cursor,
                            span: *span,
                        },
                    ],
                    trailing: false,
                    span: *span,
                });
            }
            if self.is_shared_iterator(&ty) {
                return Some(value.clone());
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
        let input_type = self.type_hint(operand, vars, functions, types)?;
        if self.is_shared_iterator(&input_type) {
            let Expr::Ident { name, .. } = operand.as_ref() else {
                return None;
            };
            let successor = format!("$shared_successor_{name}");
            let item = format!("$shared_item_{name}");
            let ident = |name: &str| Expr::Ident {
                name: name.into(),
                span,
            };
            return Some(Expr::Sequence {
                exprs: vec![
                    Expr::Destructure {
                        pattern: BindingPattern::Tuple {
                            elements: [successor.as_str(), item.as_str()]
                                .into_iter()
                                .map(|name| BindingPattern::Name {
                                    name: name.into(),
                                    span,
                                })
                                .collect(),
                            span,
                        },
                        mutable: false,
                        annotation: None,
                        value: Box::new(Expr::Call {
                            callee: "$joky_iter.step_shared".into(),
                            type_args: vec![],
                            args: vec![*operand.clone()],
                            span,
                        }),
                        span,
                    },
                    Expr::Assign {
                        name: name.clone(),
                        value: Box::new(ident(&successor)),
                        op: None,
                        span,
                    },
                    Expr::Match {
                        conditional: true,
                        iteration: false,
                        value: Box::new(ident(&item)),
                        arms: arms.to_vec(),
                        span,
                    },
                ],
                trailing: true,
                span,
            });
        }
        let Type::Reference(reference) = input_type else {
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
        let call = |method: &str| {
            if method == "advance" {
                if let Expr::Ident { name, .. } = operand.as_ref() {
                    if let Some(suffix) = name.strip_prefix("$for_iterator_") {
                        let plan = format!("$exclusive_plan_{suffix}");
                        if vars.contains_key(&plan) {
                            return Expr::Call {
                                callee: "$joky_iter.advance_exclusive".into(),
                                type_args: vec![],
                                args: vec![
                                    Expr::Unary {
                                        op: UnaryOp::BorrowMut,
                                        operand: Box::new(Expr::Ident { name: plan, span }),
                                        span,
                                    },
                                    *operand.clone(),
                                ],
                                span,
                            };
                        }
                    }
                }
            }
            Expr::Call {
                callee: format!(
                    "$joky_iter.{}{method}",
                    if paired { "entries_" } else { "" }
                ),
                type_args: vec![],
                args: vec![*operand.clone()],
                span: value.span(),
            }
        };
        // A direct pair pattern needs only the element addresses. Keep explicit
        // Entry bindings on the snapshot path, including let destructuring later.
        if paired {
            if let Expr::Sequence { exprs, .. } = &success.body {
                if let Some(Expr::Destructure {
                    pattern: pattern @ BindingPattern::Tuple { .. },
                    value: item,
                    span: pattern_span,
                    ..
                }) = exprs.first()
                {
                    if matches!(item.as_ref(), Expr::Ident { name: item, .. } if item == name) {
                        let mut body = exprs.clone();
                        body[0] = Expr::Destructure {
                            pattern: pattern.clone(),
                            mutable: false,
                            annotation: None,
                            value: Box::new(Expr::Call {
                                callee: "$for_pair_binding".into(),
                                type_args: vec![],
                                args: vec![Expr::Unary {
                                    op: if reference.mutable {
                                        UnaryOp::BorrowMut
                                    } else {
                                        UnaryOp::Borrow
                                    },
                                    operand: Box::new(Expr::Unary {
                                        op: UnaryOp::Deref,
                                        operand: operand.clone(),
                                        span: value.span(),
                                    }),
                                    span: value.span(),
                                }],
                                span: *pattern_span,
                            }),
                            span: *pattern_span,
                        };
                        return Some(Expr::If {
                            condition: Box::new(call("advance")),
                            then_branch: Box::new(Expr::Sequence {
                                exprs: body,
                                trailing: true,
                                span,
                            }),
                            else_branch: Some(Box::new(fallback.body.clone())),
                            span,
                        });
                    }
                }
            }
        }
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
        let pair = match &borrowed.target {
            Type::BufferEntry(pair) | Type::BufferPairIterator(pair) => pair,
            _ => unreachable!(),
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
