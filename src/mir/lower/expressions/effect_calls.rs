use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_effect_call(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        operation: crate::sema::EffectOperationId,
        arguments: &[CoreCallArgument],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let mode = types
            .effects()
            .operation_mode(operation)
            .ok_or_else(|| Diagnostic::codegen("MIR effect operation was not resolved"))?;
        let suspends = types
            .effects()
            .operation_info(operation)
            .is_some_and(|info| info.suspends);
        let parameter_borrows = types
            .effects()
            .operation_info(operation)
            .map(|info| info.parameter_borrows.clone())
            .unwrap_or_default();
        let method_receiver = matches!(
            &callee.kind,
            CoreExprKind::Field { value, .. }
                if matches!(value.ty, Type::Native(_))
        );
        let mut mir_arguments = Vec::with_capacity(arguments.len() + method_receiver as usize);
        if method_receiver {
            let CoreExprKind::Field { value, .. } = &callee.kind else {
                unreachable!("socket method call receiver must be a field value");
            };
            let Some(receiver) = (if parameter_borrows.first() == Some(&true) {
                self.lower_borrowed_value(value, function_names, types)?
            } else {
                self.lower_value(value, function_names, types)?
            }) else {
                return Ok(None);
            };
            mir_arguments.push(MirCallArgument {
                parameter: 0,
                value: receiver,
            });
        }
        for (parameter, argument) in arguments.iter().enumerate() {
            let parameter = parameter + usize::from(method_receiver);
            let Some(value) = (if parameter_borrows.get(parameter) == Some(&true) {
                self.lower_borrowed_value(&argument.value, function_names, types)?
            } else {
                self.lower_value(&argument.value, function_names, types)?
            }) else {
                return Ok(None);
            };
            mir_arguments.push(MirCallArgument { parameter, value });
        }
        if let Some(target) = self
            .resumable_handlers
            .iter()
            .rev()
            .find_map(|handlers| handlers.get(&operation).cloned())
        {
            if mode != crate::sema::EffectMode::Resumable {
                return Err(Diagnostic::codegen(
                    "resuming handler matched an invalid operation",
                ));
            }
            if target.runtime_resume_block.is_some() {
                let destination = self.next_value(expression.ty);
                let suspend_block = self.current;
                // Each request needs its own local continuation. The handler
                // frame's shared join is only registration metadata; reusing
                // it here would make a request in one branch jump past that
                // branch's merge and can create a CFG cycle for nested
                // conditionals.
                let resume_block = self.new_block();
                let continuation = self.new_continuation_with_kind(
                    operation,
                    suspend_block,
                    resume_block,
                    destination,
                    MirContinuationKind::Resumable,
                );
                self.push_statement(MirStatement::ResumableRequest {
                    destination,
                    operation,
                    continuation,
                    arguments: mir_arguments,
                });
                self.terminate(MirTerminator::Goto {
                    target: resume_block,
                    arguments: Vec::new(),
                })?;
                self.switch_to(resume_block);
                self.push_statement(MirStatement::Resume { continuation });
                // The value is materialized by the backend from the scalar
                // payload registered by HandlerEnter.
                return Ok(Some(destination));
            }
            if target.aborts {
                let Some(abort_target) = target.abort_target else {
                    return Err(Diagnostic::codegen(
                        "resumable abort handler has no abort target",
                    ));
                };
                self.bindings.push(HashMap::new());
                for (pattern, argument) in target.parameters.iter().zip(&mir_arguments) {
                    self.lower_pattern_bindings(pattern, argument.value, types)?;
                }
                let result = self.lower_value(&target.value, function_names, types)?;
                self.bindings.pop();
                let Some(result) = result else {
                    return Ok(None);
                };
                self.terminate(MirTerminator::Goto {
                    target: abort_target,
                    arguments: vec![result],
                })?;
                return Ok(None);
            }
            self.bindings.push(HashMap::new());
            for (pattern, argument) in target.parameters.iter().zip(&mir_arguments) {
                self.lower_pattern_bindings(pattern, argument.value, types)?;
            }
            let result = self.lower_value(&target.value, function_names, types)?;
            self.bindings.pop();
            return Ok(result);
        }
        if matches!(mode, crate::sema::EffectMode::Resumable) {
            let destination = self.next_value(expression.ty);
            let suspend_block = self.current;
            let resume_block = self.new_block();
            let continuation = self.new_continuation_with_kind(
                operation,
                suspend_block,
                resume_block,
                destination,
                MirContinuationKind::Resumable,
            );
            self.push_statement(MirStatement::ResumableRequest {
                destination,
                operation,
                continuation,
                arguments: mir_arguments,
            });
            self.terminate(MirTerminator::Goto {
                target: resume_block,
                arguments: Vec::new(),
            })?;
            self.switch_to(resume_block);
            self.push_statement(MirStatement::Resume { continuation });
            return Ok(Some(destination));
        }
        if !matches!(
            mode,
            crate::sema::EffectMode::Normal
                | crate::sema::EffectMode::Resumable
                | crate::sema::EffectMode::Aborts
        ) {
            return Err(Diagnostic::codegen(
                "effect operation mode is not lowered by MIR",
            ));
        }
        if let Some(target) = self
            .handlers
            .iter()
            .rev()
            .find_map(|handlers| handlers.get(&operation).cloned())
        {
            if target.runtime_dispatch {
                let destination = self.next_value(expression.ty);
                let suspend_block = self.current;
                let resume_block = self.new_block();
                let continuation = self.new_continuation_with_kind(
                    operation,
                    suspend_block,
                    resume_block,
                    destination,
                    MirContinuationKind::Normal,
                );
                self.push_statement(MirStatement::HandlerRequest {
                    destination,
                    operation,
                    continuation,
                    arguments: mir_arguments,
                });
                self.terminate(MirTerminator::Goto {
                    target: resume_block,
                    arguments: Vec::new(),
                })?;
                self.switch_to(resume_block);
                self.push_statement(MirStatement::Resume { continuation });
                return Ok(Some(destination));
            }
            self.terminate(MirTerminator::Goto {
                target: target.arm.target,
                arguments: mir_arguments
                    .iter()
                    .map(|argument| argument.value)
                    .collect(),
            })?;
            return Ok(None);
        }
        // An unhandled suspending operation transfers control to its provider.
        // A matching Normal handler above services it synchronously as an
        // ordinary operation result.
        if suspends {
            let destination = self.next_value(expression.ty);
            let suspend_block = self.current;
            let resume_block = self.new_block();
            let continuation =
                self.new_continuation(operation, suspend_block, resume_block, destination);
            self.push_statement(MirStatement::Suspend {
                destination,
                operation,
                continuation,
                arguments: mir_arguments,
            });
            self.terminate(MirTerminator::Goto {
                target: resume_block,
                arguments: Vec::new(),
            })?;
            self.switch_to(resume_block);
            self.push_statement(MirStatement::Resume { continuation });
            let destination = self.lower_task_poll_result(destination)?;
            return Ok(Some(destination));
        }
        // Normal operations always use the canonical runtime-frame request,
        // including calls from recursive or indirect functions.
        if mode == crate::sema::EffectMode::Normal {
            let destination = self.next_value(expression.ty);
            let suspend_block = self.current;
            let resume_block = self.new_block();
            let continuation = self.new_continuation_with_kind(
                operation,
                suspend_block,
                resume_block,
                destination,
                MirContinuationKind::Normal,
            );
            self.push_statement(MirStatement::HandlerRequest {
                destination,
                operation,
                continuation,
                arguments: mir_arguments,
            });
            self.terminate(MirTerminator::Goto {
                target: resume_block,
                arguments: Vec::new(),
            })?;
            self.switch_to(resume_block);
            self.push_statement(MirStatement::Resume { continuation });
            return Ok(Some(destination));
        }
        if matches!(
            mode,
            crate::sema::EffectMode::Resumable | crate::sema::EffectMode::Aborts
        ) {
            let arguments = mir_arguments
                .iter()
                .map(|argument| argument.value)
                .collect::<Vec<_>>();
            self.release_cown_leases_from(0);
            for scope in self.task_scopes.clone().into_iter().rev() {
                self.push_statement(MirStatement::ScopeExit { scope });
            }
            let result = self.next_value(self.return_type);
            self.push_statement(MirStatement::TaskAbort {
                destination: result,
                operation,
                arguments,
            });
            let block = &mut self.blocks[self.current.0];
            block.terminator = Some(MirTerminator::Return(Some(result)));
            return Ok(None);
        }
        unreachable!("all supported unhandled effect modes return through TaskAbort")
    }
}
