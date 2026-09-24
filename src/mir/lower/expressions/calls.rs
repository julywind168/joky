use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(in crate::mir::lower) fn lower_method_call_values(
        &mut self,
        receiver: MirValueId,
        method: &str,
        result_type: Type,
        arguments: Vec<MirCallArgument>,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let receiver_type = self.value_types[receiver.0];
        let key = method_key(receiver_type, method);
        // Generated Cursor calls need the same lexical resumable handler
        // inlining as explicit method calls. Recursive calls use the runtime chain.
        let inline = self.function_bodies.get(&key).copied().filter(|callee| {
            !self.resumable_handlers.is_empty()
                && !self.inline_functions.contains(&key)
                && callee.used_effects.iter().any(|operation| {
                    matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Resumable)
                    )
                })
        });
        if let Some(callee) = inline {
            self.inline_functions.push(key);
            self.bindings.push(HashMap::new());
            let local = self.new_local_with_ownership(
                "self",
                receiver_type,
                self.value_ownership[receiver.0],
            );
            self.bind_local("self", local);
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Bind {
                local,
                value: Some(receiver),
                destination,
            });
            for (index, parameter) in callee.parameters.iter().enumerate() {
                let value = arguments
                    .iter()
                    .find(|argument| argument.parameter == index)
                    .map(|argument| argument.value)
                    .ok_or_else(|| {
                        Diagnostic::codegen("inline method call is missing a parameter value")
                    })?;
                let local = self.new_local(&parameter.name, parameter.ty);
                self.bind_local(&parameter.name, local);
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(value),
                    destination,
                });
            }
            let result = self.lower_value(&callee.body, function_names, types)?;
            self.bindings.pop();
            self.inline_functions.pop();
            return Ok(result);
        }
        let method = *self
            .function_ids
            .get(&key)
            .ok_or_else(|| Diagnostic::codegen("MIR method was not resolved"))?;
        let destination = self.next_value(result_type);
        self.push_statement(MirStatement::MethodCall {
            destination,
            receiver,
            receiver_type,
            method,
            arguments,
            continuation: None,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_function_call(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let inline_name = match &callee.kind {
            CoreExprKind::Name(name) => self.closure_bindings.get(name).cloned(),
            CoreExprKind::Closure {
                function_name,
                captures,
                ..
            } => self.local_closure_binding(function_name, captures),
            _ => None,
        };
        // Locally bound, capture-free closures have a stable target even when
        // called through a function-valued local. Lower these through the
        // direct-call ABI so suspending closures can participate in Pending
        // propagation without an indirect closure trampoline.
        if let Some(LocalClosureBinding {
            function_name: target_name,
            captures,
        }) = &inline_name
        {
            if captures.is_empty() {
                if let Some(function) = self.closure_ids.get(target_name).copied() {
                    let Type::Function(signature_id) = callee.ty else {
                        unreachable!("closure binding requires function type");
                    };
                    let signature = types.function_type(signature_id);
                    let mut used = vec![false; signature.parameters.len()];
                    let mut next_positional = 0;
                    let mut mir_arguments = Vec::with_capacity(arguments.len());
                    for argument in arguments {
                        let Some(value) =
                            self.lower_value(&argument.value, function_names, types)?
                        else {
                            return Ok(None);
                        };
                        mir_arguments.push(MirCallArgument {
                            parameter: resolve_argument_index(
                                argument.label.as_deref(),
                                &signature.parameter_names,
                                &mut used,
                                &mut next_positional,
                            )?,
                            value,
                        });
                    }
                    let destination = self.next_value(expression.ty);
                    self.push_statement(MirStatement::Call {
                        destination,
                        function,
                        arguments: mir_arguments,
                        continuation: None,
                    });
                    let destination = self.lower_task_poll_result(destination)?;
                    return Ok(Some(destination));
                }
            }
        }
        if let Some(LocalClosureBinding {
            function_name: inline_name,
            captures: capture_specs,
        }) = inline_name
        {
            if let Some(inline_function) = self.function_bodies.get(&inline_name).copied() {
                let covered = inline_function.used_effects.iter().all(|operation| {
                    self.handlers.iter().rev().any(|handlers| {
                        handlers
                            .get(&operation)
                            .is_some_and(|target| target.runtime_dispatch)
                    })
                });
                if covered
                    && !self
                        .inline_functions
                        .iter()
                        .any(|active| active == &inline_name)
                {
                    let Type::Function(signature_id) = callee.ty else {
                        unreachable!("closure call guard requires function type");
                    };
                    let signature = types.function_type(signature_id);
                    if signature.parameters.len() + capture_specs.len()
                        == inline_function.parameters.len()
                    {
                        let mut used = vec![false; signature.parameters.len()];
                        let mut next_positional = 0;
                        let mut mir_arguments = Vec::with_capacity(arguments.len());
                        for argument in arguments {
                            let Some(value) =
                                self.lower_value(&argument.value, function_names, types)?
                            else {
                                return Ok(None);
                            };
                            mir_arguments.push(MirCallArgument {
                                parameter: resolve_argument_index(
                                    argument.label.as_deref(),
                                    &signature.parameter_names,
                                    &mut used,
                                    &mut next_positional,
                                )?,
                                value,
                            });
                        }
                        self.inline_functions.push(inline_name);
                        self.bindings.push(HashMap::new());
                        for (index, (_, capture_type, capture_local)) in
                            capture_specs.iter().enumerate()
                        {
                            let captured = self.next_value(*capture_type);
                            self.push_statement(MirStatement::Read {
                                destination: captured,
                                local: *capture_local,
                            });
                            let value = if types.is_shared(*capture_type) {
                                let duplicate = self.next_value(*capture_type);
                                self.push_statement(MirStatement::Dup {
                                    destination: duplicate,
                                    value: captured,
                                });
                                duplicate
                            } else {
                                captured
                            };
                            let parameter = &inline_function.parameters[index];
                            let local = self.new_local(&parameter.name, parameter.ty);
                            self.bind_local(&parameter.name, local);
                            let destination = self.next_value(Type::Unit);
                            self.push_statement(MirStatement::Bind {
                                local,
                                value: Some(value),
                                destination,
                            });
                        }
                        for (index, parameter) in inline_function
                            .parameters
                            .iter()
                            .skip(capture_specs.len())
                            .enumerate()
                        {
                            let value = mir_arguments
                                .iter()
                                .find(|argument| argument.parameter == index)
                                .map(|argument| argument.value)
                                .ok_or_else(|| {
                                    Diagnostic::codegen(
                                        "inline closure call is missing a parameter value",
                                    )
                                })?;
                            let local = self.new_local(&parameter.name, parameter.ty);
                            self.bind_local(&parameter.name, local);
                            let destination = self.next_value(Type::Unit);
                            self.push_statement(MirStatement::Bind {
                                local,
                                value: Some(value),
                                destination,
                            });
                        }
                        let result =
                            self.lower_value(&inline_function.body, function_names, types)?;
                        self.bindings.pop();
                        self.inline_functions.pop();
                        return Ok(result);
                    }
                }
            }
        }
        // A capture-free closure literal has a statically known target. Lower
        // it as a direct call so it can use the ordinary Pending ABI, while
        // retaining true dynamic dispatch for closure values stored in locals.
        if let CoreExprKind::Closure {
            function_name,
            captures,
            ..
        } = &callee.kind
        {
            let Some(function) = self.closure_ids.get(function_name).copied() else {
                return Err(Diagnostic::codegen("MIR closure function was not resolved"));
            };
            if !captures.is_empty() {
                // Capturing closures retain the dynamic closure ABI.
            } else {
                let Type::Function(signature_id) = callee.ty else {
                    unreachable!("closure call requires function type");
                };
                let signature = types.function_type(signature_id);
                let mut used = vec![false; signature.parameters.len()];
                let mut next_positional = 0;
                let mut mir_arguments = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    let Some(value) = self.lower_value(&argument.value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    mir_arguments.push(MirCallArgument {
                        parameter: resolve_argument_index(
                            argument.label.as_deref(),
                            &signature.parameter_names,
                            &mut used,
                            &mut next_positional,
                        )?,
                        value,
                    });
                }
                let destination = self.next_value(expression.ty);
                self.push_statement(MirStatement::Call {
                    destination,
                    function,
                    arguments: mir_arguments,
                    continuation: None,
                });
                let destination = self.lower_task_poll_result(destination)?;
                return Ok(Some(destination));
            }
        }
        let Some(callee_value) = self.lower_borrowed_value(callee, function_names, types)? else {
            return Ok(None);
        };
        let Type::Function(signature_id) = callee.ty else {
            unreachable!("closure call guard requires function type");
        };
        let signature = types.function_type(signature_id);
        let mut used = vec![false; signature.parameters.len()];
        let mut next_positional = 0;
        let mut mir_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Some(value) = self.lower_value(&argument.value, function_names, types)? else {
                return Ok(None);
            };
            mir_arguments.push(MirCallArgument {
                parameter: resolve_argument_index(
                    argument.label.as_deref(),
                    &signature.parameter_names,
                    &mut used,
                    &mut next_positional,
                )?,
                value,
            });
        }
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::CallIndirect {
            destination,
            callee: callee_value,
            arguments: mir_arguments,
            may_suspend: signature.suspends,
            continuation: None,
        });
        let destination = self.lower_task_poll_result(destination)?;
        Ok(Some(destination))
    }

    pub(super) fn lower_named_call(
        &mut self,
        expression: &CoreExpr,
        function: &str,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let function_id = *self
            .function_ids
            .get(function)
            .ok_or_else(|| Diagnostic::codegen("MIR function was not resolved"))?;
        let parameter_names = self
            .function_parameters
            .get(&function_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut used_parameters = vec![false; parameter_names.len()];
        let mut next_positional = 0;
        let mut mir_arguments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let parameter = resolve_argument_index(
                argument.label.as_deref(),
                parameter_names,
                &mut used_parameters,
                &mut next_positional,
            )?;
            let borrowed = self
                .function_bodies
                .get(function)
                .and_then(|body| body.parameter_ownership.get(parameter))
                .is_some_and(|mode| *mode == Some(crate::hir::CoreParameterOwnership::Borrowed));
            let Some(value) = (if borrowed {
                self.lower_borrowed_value(&argument.value, function_names, types)?
            } else {
                self.lower_value(&argument.value, function_names, types)?
            }) else {
                return Ok(None);
            };
            mir_arguments.push(MirCallArgument { parameter, value });
        }
        let resumable_callee = self
            .function_bodies
            .get(function)
            .copied()
            .filter(|callee| {
                let has_resumable_effect = callee.used_effects.iter().any(|operation| {
                    matches!(
                        types.effects().operation_mode(operation),
                        Some(crate::sema::EffectMode::Resumable)
                    )
                });
                !self.resumable_handlers.is_empty() && has_resumable_effect
            });
        if let Some(callee) = resumable_callee {
            if self
                .inline_functions
                .iter()
                .any(|active| active == function)
            {
                // Recursive calls use the dynamic frame chain.
            } else {
                self.inline_functions.push(function.to_owned());
                self.bindings.push(HashMap::new());
                for (index, parameter) in callee.parameters.iter().enumerate() {
                    let value = mir_arguments
                        .iter()
                        .find(|argument| argument.parameter == index)
                        .map(|argument| argument.value)
                        .ok_or_else(|| {
                            Diagnostic::codegen(
                                "resumable inline call is missing a parameter value",
                            )
                        })?;
                    let local = self.new_local_with_ownership(
                        &parameter.name,
                        parameter.ty,
                        self.value_ownership[value.0],
                    );
                    self.bind_local(&parameter.name, local);
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Bind {
                        local,
                        value: Some(value),
                        destination,
                    });
                }
                let result = self.lower_value(&callee.body, function_names, types)?;
                self.bindings.pop();
                self.inline_functions.pop();
                return Ok(result);
            }
        }
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::Call {
            destination,
            function: function_id,
            arguments: mir_arguments,
            continuation: None, // Phase 2: will be added in post-processing
        });
        let destination = self.lower_task_poll_result(destination)?;
        Ok(Some(destination))
    }
}
