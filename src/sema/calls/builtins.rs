use crate::diagnostic::SemanticError;
use crate::sema::checker::{intern_cown, intern_list, intern_option, intern_result, Checker};
use crate::sema::expectation::TypeExpectation;
use crate::sema::types::Type;
use crate::syntax::{CallArgument, Expr, ExprKind, FieldAccess};
use crate::Span;
use std::collections::HashSet;

impl Checker {
    pub(super) fn resolve_type_value_arguments(
        &mut self,
        call_id: crate::syntax::NodeId,
        arguments: &[CallArgument],
        arity: usize,
    ) -> Result<Option<Vec<Type>>, SemanticError> {
        if arguments.len() != arity || arguments.iter().any(|argument| argument.label.is_some()) {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(arity);
        for argument in arguments {
            let Some(value) = self.resolve_type_value_expr(&argument.value)? else {
                return Ok(None);
            };
            values.push(value);
        }
        self.type_value_calls.insert(call_id, values.clone());
        Ok(Some(values))
    }

    pub(super) fn check_builtin_call(
        &mut self,
        callee: &Expr,
        arguments: &[CallArgument],
        call_span: Span,
        call_id: crate::syntax::NodeId,
        expectation: TypeExpectation,
    ) -> Result<Option<Type>, SemanticError> {
        if let ExprKind::Field {
            value,
            access: FieldAccess::Name(method_name),
        } = &callee.kind
        {
            if let ExprKind::Name(name) = &value.kind {
                if matches!(name.as_str(), "CPtr" | "CMutPtr" | "CStr") && method_name == "null" {
                    let arity = usize::from(name != "CStr");
                    if arguments.len() != arity {
                        return Err(SemanticError::WrongArgumentCount {
                            function: format!("{name}.null"),
                            expected: arity,
                            span: call_span,
                        });
                    }
                    let Some(values) =
                        self.resolve_type_value_arguments(call_id, arguments, arity)?
                    else {
                        return Err(SemanticError::FunctionNotSupported {
                            name: format!(
                                "{name}.null requires a positional compile-time type value"
                            ),
                            span: call_span,
                        });
                    };
                    return if name == "CStr" {
                        Ok(Some(Type::CStr))
                    } else {
                        self.apply_type_value(
                            name,
                            &[(None, values[0], call_span)],
                            call_span,
                            &mut Vec::new(),
                        )
                        .map(Some)
                    };
                }
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "CCallback")
                && method_name == "new"
            {
                if arguments.len() != 2 || arguments.iter().any(|arg| arg.label.is_some()) {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "CCallback.new".into(),
                        expected: 2,
                        span: call_span,
                    });
                }
                let callback = &arguments[0].value;
                if !matches!(callback.kind, ExprKind::Closure { .. }) {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "CCallback.new requires a closure literal and an explicit failure result".into(),
                        span: callback.span,
                    });
                }
                let Type::Function(id) =
                    self.check_expression(callback, TypeExpectation::none())?
                else {
                    return Err(SemanticError::NotCallable {
                        span: callback.span,
                    });
                };
                if self
                    .closure_capture_bindings
                    .get(&callback.id)
                    .is_some_and(|captures| captures.iter().any(|capture| capture.mutable))
                {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "CCallback cannot capture mutable bindings; use an explicit immutable snapshot".into(),
                        span: callback.span,
                    });
                }
                let signature = self.function_types[id].clone();
                if signature
                    .parameters
                    .iter()
                    .any(|ty| !ty.is_c_abi_compatible())
                    || !(signature.return_type.is_c_abi_compatible()
                        || signature.return_type == Type::Unit)
                {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "CCallback requires C scalar/pointer parameters and a C scalar/pointer or Unit result".into(),
                        span: callback.span,
                    });
                }
                // A retained callback runs with an independent handler context.
                // Start with the runtime-owned timer provider; lexical handlers
                // and their borrowed environments cannot escape into C.
                if signature.effects.iter().any(|op| {
                    self.effects
                        .effect(op.effect)
                        .is_none_or(|effect| effect.name != "time")
                        || self.effects.operation_info(op).is_none_or(|op| {
                            op.name != "sleep"
                                || !op.suspends
                                || op.parameters != [Type::Duration]
                                || op.parameter_borrows != [false]
                                || op.return_type != Type::Unit
                                || op.mode != crate::sema::effects::EffectMode::Normal
                        })
                }) {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "CCallback currently supports only runtime time.sleep effects; lexical handlers cannot escape into C".into(),
                        span: callback.span,
                    });
                }
                if self
                    .closure_captures
                    .get(&callback.id)
                    .is_some_and(|captures| {
                        captures
                            .iter()
                            .any(|(_, ty)| !self.is_callback_capture(*ty, &mut HashSet::new()))
                    })
                {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "CCallback captures must contain only immutable shared values, scalars, or C pointers; owned resources, Cown, and closures are not supported".into(),
                        span: callback.span,
                    });
                }
                self.check_expression(
                    &arguments[1].value,
                    TypeExpectation::require(signature.return_type),
                )?;
                return Ok(Some(Type::CCallback));
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "CMutPtr")
                && method_name == "alloc"
            {
                if arguments.len() != 1 || arguments[0].label.is_some() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "CMutPtr.alloc".to_owned(),
                        expected: 1,
                        span: call_span,
                    });
                }
                let content =
                    self.check_expression(&arguments[0].value, TypeExpectation::none())?;
                if !(content.is_c_scalar() || content.is_c_pointer()) {
                    return Err(SemanticError::TypeMismatch {
                        expected: "a C scalar or C pointer value".to_owned(),
                        actual: crate::sema::type_name(content),
                        span: arguments[0].value.span,
                    });
                }
                return self
                    .apply_type_value(
                        "CMutPtr",
                        &[(None, content, call_span)],
                        call_span,
                        &mut Vec::new(),
                    )
                    .map(Some);
            }
            if matches!(&value.kind, ExprKind::Name(name) if name == "Cown") && method_name == "new"
            {
                if arguments.len() == 1 {
                    let payload =
                        self.check_expression(&arguments[0].value, TypeExpectation::none())?;
                    if self.drop_contract(payload).0 {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "Cown cannot own values with user Drop".into(),
                            span: call_span,
                        });
                    }
                    if !matches!(
                        payload,
                        Type::Class(_)
                            | Type::MutBytes
                            | Type::MutList(_)
                            | Type::MutMap(_)
                            | Type::MutSet(_)
                    ) {
                        return Err(SemanticError::TypeMismatch {
                            expected: "a uniquely owned pointer payload".to_owned(),
                            actual: crate::sema::type_name(payload),
                            span: arguments[0].value.span,
                        });
                    }
                    return Ok(Some(Type::Cown(intern_cown(&mut self.cowns, payload))));
                }
                return Err(SemanticError::WrongArgumentCount {
                    function: "Cown.new".to_owned(),
                    expected: 1,
                    span: call_span,
                });
            }
        }

        if matches!(&callee.kind, ExprKind::Name(name) if name == "List") {
            let element_hint = expectation
                .ty()
                .and_then(|ty| match ty {
                    Type::List(id) => Some(self.list_types[id]),
                    _ => None,
                })
                .map(TypeExpectation::require)
                .unwrap_or_else(TypeExpectation::none);
            let Some(first) = arguments.first() else {
                return Err(SemanticError::WrongArgumentCount {
                    function: "List".to_owned(),
                    expected: 1,
                    span: call_span,
                });
            };
            let element = self.check_expression(&first.value, element_hint)?;
            self.check_list_element(element, first.value.span)?;
            for argument in &arguments[1..] {
                self.check_expression(&argument.value, TypeExpectation::require(element))?;
            }
            return Ok(Some(Type::List(intern_list(&mut self.list_types, element))));
        }
        if matches!(&callee.kind, ExprKind::Name(name) if name == "Bytes") {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Bytes".to_owned(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Some(Type::Bytes));
        }
        if matches!(&callee.kind, ExprKind::Name(name) if name == "MutBytes") {
            if !arguments.is_empty() {
                return Err(SemanticError::WrongArgumentCount {
                    function: "MutBytes".to_owned(),
                    expected: 0,
                    span: call_span,
                });
            }
            return Ok(Some(Type::MutBytes));
        }
        if matches!(&callee.kind, ExprKind::Name(name) if name == "Some") {
            if arguments.len() != 1 {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Some".to_owned(),
                    expected: 1,
                    span: call_span,
                });
            }
            let hint = expectation
                .ty()
                .and_then(|ty| match ty {
                    Type::Option(id) => Some(self.option_types[id]),
                    _ => None,
                })
                .map(TypeExpectation::require)
                .unwrap_or_else(TypeExpectation::none);
            let value = self.check_expression(&arguments[0].value, hint)?;
            return Ok(Some(Type::Option(intern_option(
                &mut self.option_types,
                value,
            ))));
        }
        if matches!(&callee.kind, ExprKind::Name(name) if matches!(name.as_str(), "Ok" | "Err")) {
            if arguments.len() != 1 {
                return Err(SemanticError::WrongArgumentCount {
                    function: "Result constructor".to_owned(),
                    expected: 1,
                    span: call_span,
                });
            }
            let ExprKind::Name(name) = &callee.kind else {
                unreachable!()
            };
            let expected = expectation.ty().and_then(|ty| match ty {
                Type::Result(id) => Some(self.result_types[id]),
                _ => None,
            });
            let (value_hint, other) = match (name.as_str(), expected) {
                ("Ok", Some((ok, err))) => (TypeExpectation::require(ok), Some(err)),
                ("Err", Some((ok, err))) => (TypeExpectation::require(err), Some(ok)),
                _ => (TypeExpectation::none(), None),
            };
            let value = self.check_expression(&arguments[0].value, value_hint)?;
            let (ok, err) = if name == "Ok" {
                (value, other.unwrap_or(Type::Unit))
            } else {
                (other.unwrap_or(Type::Unit), value)
            };
            return Ok(Some(Type::Result(intern_result(
                &mut self.result_types,
                ok,
                err,
            ))));
        }
        Ok(None)
    }
}
