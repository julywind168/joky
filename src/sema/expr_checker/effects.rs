use super::*;

impl Checker {
    pub(super) fn check_do(
        &mut self,
        body: &Expr,
        handlers: &[crate::syntax::HandlerArm],
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        self.check_mutable_captures(body, [], "do")?;
        let saved_effects = std::mem::take(&mut self.current_effects);
        let has_abort_handler = handlers.iter().any(|handler| {
            let operation_is_abortive = self
                .effects
                .by_name(&handler.effect)
                .and_then(|effect| self.effects.operation_by_name(effect, &handler.operation))
                .and_then(|operation| self.effects.operation_info(operation))
                .is_some_and(|operation| {
                    operation.mode == crate::sema::effects::EffectMode::Aborts
                });
            operation_is_abortive || terminal_abort_payload(&handler.value).is_some()
        });
        let body_type = self.check_expression(
            body,
            if has_abort_handler {
                TypeExpectation::none()
            } else {
                expectation
            },
        )?;
        let do_result_type = if has_abort_handler {
            expectation.ty().unwrap_or(body_type)
        } else {
            body_type
        };
        let body_effects = std::mem::take(&mut self.current_effects);

        let mut handled = crate::sema::effects::EffectSet::new();
        let mut handler_effects = crate::sema::effects::EffectSet::new();
        let mut seen = std::collections::HashSet::new();
        let mut has_resumable = false;
        let mut has_abortive = false;
        for handler in handlers {
            let Some(effect_id) = self.effects.by_name(&handler.effect) else {
                return Err(SemanticError::UnknownType {
                    name: handler.effect.clone(),
                    span: handler.span,
                });
            };
            let Some(operation_id) = self
                .effects
                .operation_by_name(effect_id, &handler.operation)
            else {
                return Err(SemanticError::UnknownEffectOperation {
                    effect: handler.effect.clone(),
                    operation: handler.operation.clone(),
                    span: handler.span,
                });
            };
            let operation = self
                .effects
                .operation_info(operation_id)
                .expect("operation id")
                .clone();
            if !seen.insert(operation_id) {
                return Err(SemanticError::InvalidEffectDefinition {
                    name: format!("{}.{}", handler.effect, handler.operation),
                    span: handler.span,
                });
            }
            if operation.mode == crate::sema::effects::EffectMode::Resumable {
                has_resumable = true;
            } else if operation.mode == crate::sema::effects::EffectMode::Aborts {
                has_abortive = true;
            }
            if has_resumable && has_abortive {
                return Err(SemanticError::FunctionNotSupported {
                    name: "a do expression cannot mix resumable and abortive handler arms yet"
                        .to_owned(),
                    span: handler.span,
                });
            }
            if handler.parameters.len() != operation.parameters.len() {
                return Err(SemanticError::WrongArgumentCount {
                    function: format!("{}.{} handler", handler.effect, handler.operation),
                    expected: operation.parameters.len(),
                    span: handler.span,
                });
            }
            self.scopes.push(std::collections::HashMap::new());
            for (pattern, ty) in handler.parameters.iter().zip(&operation.parameters) {
                self.bind_handler_pattern(pattern, *ty)?;
            }
            self.check_mutable_captures(&handler.value, [], "handler")?;
            let allows_abort = operation.mode != crate::sema::effects::EffectMode::Normal;
            if allows_abort {
                self.handler_abort_types.push(do_result_type);
            }
            let abort_action = terminal_abort_payload(&handler.value).is_some();
            let legacy_resume = legacy_resume_payload(&handler.value);
            let checked = if operation.mode == crate::sema::effects::EffectMode::Aborts {
                self.check_expression(&handler.value, TypeExpectation::require(do_result_type))
            } else if abort_action {
                if operation.mode != crate::sema::effects::EffectMode::Resumable {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "abort(value) is only valid for resumable or aborting handlers"
                            .to_owned(),
                        span: handler.value.span,
                    });
                }
                self.check_expression(&handler.value, TypeExpectation::require(do_result_type))
            } else if operation.mode == crate::sema::effects::EffectMode::Resumable {
                self.check_expression(
                    legacy_resume.unwrap_or(&handler.value),
                    TypeExpectation::require(operation.return_type),
                )
            } else {
                self.check_expression(
                    &handler.value,
                    TypeExpectation::require(operation.return_type),
                )
            };
            let handler_effects_for_check = std::mem::take(&mut self.current_effects);
            if allows_abort {
                self.handler_abort_types.pop();
            }
            let value_type = checked?;
            let expected_handler_type =
                if abort_action || operation.mode == crate::sema::effects::EffectMode::Aborts {
                    do_result_type
                } else {
                    operation.return_type
                };
            if value_type != expected_handler_type {
                return Err(type_mismatch(
                    expected_handler_type,
                    value_type,
                    handler.value.span,
                ));
            }
            if handler_effects_for_check.may_suspend(&self.effects) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "a suspending effect handler cannot suspend again".to_owned(),
                    span: handler.value.span,
                });
            }
            self.scopes.pop();
            self.handler_operations.insert(handler.id, operation_id);
            handled.insert(operation_id);
            handler_effects.extend(&handler_effects_for_check);
            self.current_effects = crate::sema::effects::EffectSet::new();
        }
        self.current_effects = saved_effects;
        self.current_effects.extend(&body_effects.without(&handled));
        self.current_effects.extend(&handler_effects);
        Ok(do_result_type)
    }

    fn bind_handler_pattern(
        &mut self,
        pattern: &crate::syntax::Pattern,
        ty: Type,
    ) -> Result<(), SemanticError> {
        match pattern {
            crate::syntax::Pattern::Wildcard { .. } => Ok(()),
            crate::syntax::Pattern::Binding { name, .. } => {
                self.bind(name.clone(), ty);
                Ok(())
            }
            crate::syntax::Pattern::EnumVariant { .. } | crate::syntax::Pattern::Tuple { .. } => {
                Err(SemanticError::FunctionNotSupported {
                    name: "complex handler patterns are not supported yet".to_owned(),
                    span: pattern.span(),
                })
            }
        }
    }

    fn readonly_until(&self, expr: &Expr, names: &[Option<String>]) -> bool {
        let primitive = |expr: &Expr| {
            self.types.get(&expr.id).is_some_and(|ty| {
                ty.is_numeric() || matches!(ty, Type::Bool | Type::String | Type::Duration)
            })
        };
        match &expr.kind {
            ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::String(_)
            | ExprKind::Bytes(_)
            | ExprKind::Boolean(_) => true,
            ExprKind::Name(name) => names.iter().flatten().any(|n| n == name),
            ExprKind::Field { value, .. } => self.readonly_until(value, names),
            ExprKind::Unary { expression, .. } => {
                primitive(expression) && self.readonly_until(expression, names)
            }
            ExprKind::Binary { left, right, .. } => {
                primitive(left)
                    && primitive(right)
                    && self.readonly_until(left, names)
                    && self.readonly_until(right, names)
            }
            ExprKind::Call { callee, arguments } => {
                if !arguments.is_empty() {
                    return false;
                }
                let ExprKind::Field {
                    value,
                    access: FieldAccess::Name(method),
                } = &callee.kind
                else {
                    return false;
                };
                matches!(method.as_str(), "is_empty" | "length")
                    && matches!(
                        self.types.get(&value.id),
                        Some(
                            Type::String
                                | Type::Bytes
                                | Type::MutBytes
                                | Type::List(_)
                                | Type::MutList(_)
                                | Type::Map(_)
                                | Type::MutMap(_)
                                | Type::MutSet(_)
                        )
                    )
                    && self.readonly_until(value, names)
            }
            _ => false,
        }
    }

    pub(super) fn check_when(
        &mut self,
        cowns: &[Expr],
        bindings: Option<&[crate::syntax::Pattern]>,
        until: Option<&Expr>,
        body: &Expr,
        span: Span,
    ) -> Result<Type, SemanticError> {
        if cowns.is_empty() {
            return Err(SemanticError::FunctionNotSupported {
                name: "when requires at least one Cown".to_owned(),
                span,
            });
        }
        if self.when_depth > 0 {
            return Err(SemanticError::FunctionNotSupported {
                name: "nested when leases are not supported; list all Cowns together".to_owned(),
                span,
            });
        }
        let mut cown_types = Vec::with_capacity(cowns.len());
        let mut seen_names = std::collections::HashSet::new();
        for cown in cowns {
            let cown_type = self.check_expression(cown, TypeExpectation::none())?;
            if let Type::Cown(_) = cown_type {
                if let ExprKind::Name(name) = &cown.kind {
                    if !seen_names.insert(name.clone()) {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "when cannot acquire the same Cown twice".to_owned(),
                            span: cown.span,
                        });
                    }
                }
                cown_types.push(cown_type);
            } else {
                return Err(SemanticError::TypeMismatch {
                    expected: "Cown(T)".to_owned(),
                    actual: type_name(cown_type),
                    span: cown.span,
                });
            }
        }
        let names = match bindings {
            Some(patterns) => {
                if patterns.len() != cowns.len() {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "when payload parameters must match the Cown count".to_owned(),
                        span,
                    });
                }
                patterns
                    .iter()
                    .map(|pattern| match pattern {
                        crate::syntax::Pattern::Binding { name, .. } => Some(name.clone()),
                        crate::syntax::Pattern::Wildcard { .. } => None,
                        crate::syntax::Pattern::EnumVariant { .. }
                        | crate::syntax::Pattern::Tuple { .. } => None,
                    })
                    .collect::<Vec<_>>()
            }
            None => cowns
                .iter()
                .map(|cown| match &cown.kind {
                    ExprKind::Name(name) => Some(name.clone()),
                    _ => None,
                })
                .collect(),
        };
        if names.iter().any(|name| name.is_none()) && bindings.is_none() {
            return Err(SemanticError::FunctionNotSupported {
                name: "complex Cown expressions require an explicit |name| payload parameter list"
                    .to_owned(),
                span,
            });
        }
        if let Some(patterns) = bindings {
            if patterns.iter().any(|pattern| {
                matches!(
                    pattern,
                    crate::syntax::Pattern::EnumVariant { .. }
                        | crate::syntax::Pattern::Tuple { .. }
                )
            }) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "complex when bindings are not supported yet".to_owned(),
                    span: patterns
                        .iter()
                        .find(|pattern| {
                            matches!(
                                pattern,
                                crate::syntax::Pattern::EnumVariant { .. }
                                    | crate::syntax::Pattern::Tuple { .. }
                            )
                        })
                        .map(crate::syntax::Pattern::span)
                        .unwrap_or(span),
                });
            }
        }
        let mut binding_names = std::collections::HashSet::new();
        for name in names.iter().flatten() {
            if !binding_names.insert(name) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "when bindings must have distinct names".to_owned(),
                    span,
                });
            }
        }
        self.scopes.push(std::collections::HashMap::new());
        for (cown_type, name) in cown_types.iter().zip(names.iter()) {
            let Type::Cown(cown_id) = cown_type else {
                unreachable!("validated Cown type");
            };
            if let Some(name) = name {
                self.bind(name.clone(), self.cowns[*cown_id]);
            }
        }
        let effects_before_body = std::mem::take(&mut self.current_effects);
        self.when_depth += 1;
        let result = (|| {
            if let Some(until) = until {
                self.check_expression(until, TypeExpectation::require(Type::Bool))?;
                if !self.readonly_until(until, &names) || !self.current_effects.is_empty() {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "until requires a read-only condition: payload fields, primitive operators, length() or is_empty(); effects, mutation and arbitrary calls are not allowed".into(),
                        span: until.span,
                    });
                }
            }
            self.check_expression(body, TypeExpectation::none())
        })();
        self.when_depth -= 1;
        self.scopes.pop();
        let body_effects = std::mem::take(&mut self.current_effects);
        self.current_effects = effects_before_body;

        let result = result?;
        if body_effects.may_suspend(&self.effects) {
            return Err(SemanticError::FunctionNotSupported {
                name: "when body cannot perform a suspending effect operation".to_owned(),
                span: body.span,
            });
        }
        self.current_effects.extend(&body_effects);
        Ok(result)
    }
}

fn terminal_abort_payload(expression: &Expr) -> Option<&Expr> {
    match &expression.kind {
        ExprKind::Abort { value } => Some(value),
        ExprKind::Block(expressions) => expressions.last().and_then(terminal_abort_payload),
        _ => None,
    }
}

fn legacy_resume_payload(expression: &Expr) -> Option<&Expr> {
    let ExprKind::Call {
        callee, arguments, ..
    } = &expression.kind
    else {
        return None;
    };
    if !matches!(&callee.kind, ExprKind::Name(name) if name == "resume") || arguments.len() != 1 {
        return None;
    }
    Some(&arguments[0].value)
}
