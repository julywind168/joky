use super::super::*;
use crate::hir::{CoreExpr, CoreHandlerArm, CorePattern};

impl Lowerer<'_> {
    pub(super) fn lower_resumable_do(
        &mut self,
        body: &CoreExpr,
        handlers: &[CoreHandlerArm],
        result_type: Type,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if !handlers
            .iter()
            .all(|handler| handler.resumes || handler.aborts)
        {
            return Err(Diagnostic::codegen(
                "MIR cannot mix controlled and ordinary handler arms",
            ));
        }
        let abort_target = handlers
            .iter()
            .any(|handler| handler.aborts)
            .then(|| self.new_block());
        let mut frame = HashMap::new();
        for handler in handlers {
            if types.effects().operation_mode(handler.operation)
                != Some(crate::sema::EffectMode::Resumable)
            {
                return Err(Diagnostic::codegen(
                    "controlled handler targets an invalid operation",
                ));
            }
            if frame
                .insert(
                    handler.operation,
                    ResumableHandlerTarget {
                        parameters: handler.parameters.clone(),
                        value: handler.value.clone(),
                        aborts: handler.aborts,
                        abort_target,
                        runtime_value: if handler.aborts {
                            None
                        } else {
                            resumable_runtime_constant(&handler.value, types)
                        },
                        runtime_parameter: resumable_forward_parameter(
                            &handler.value,
                            &handler.parameters,
                            types,
                        )
                        .filter(|index| {
                            types
                                .effects()
                                .operation_info(handler.operation)
                                .is_some_and(|operation| {
                                    operation.parameters.get(*index).is_some_and(|ty| {
                                        ownership_for_type(*ty, types) == MirOwnership::Copy
                                    })
                                })
                        }),
                        runtime_transform: resumable_scalar_transform(
                            &handler.value,
                            &handler.parameters,
                        )
                        .filter(|transform| {
                            types
                                .effects()
                                .operation_info(handler.operation)
                                .is_some_and(|info| {
                                    info.parameters.get(transform.parameter).is_some_and(|ty| {
                                        ty.is_integer() && *ty == handler.value.ty
                                    })
                                })
                        }),
                        runtime_function: if handler.aborts {
                            None
                        } else {
                            self.handler_function_ids.get(&handler.value.id).copied()
                        },
                        runtime_captures: Vec::new(),
                        runtime_resume_block: None,
                    },
                )
                .is_some()
            {
                return Err(Diagnostic::codegen("duplicate resumable handler operation"));
            }
        }
        // The runtime token path handles a single parameterless handler whose
        // response is a compile-time constant. Parameterized and dynamic arms
        // still use the direct lexical continuation path below.
        for target in frame.values_mut() {
            if target.runtime_function.is_some() {
                let parameter_names = target
                    .parameters
                    .iter()
                    .filter_map(|pattern| match pattern {
                        CorePattern::Binding { name, .. } => Some(name.as_str()),
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let captures = crate::hir::task_captures(&target.value)
                    .into_iter()
                    .filter(|(name, _)| !parameter_names.contains(name.as_str()))
                    .collect::<Vec<_>>();
                let mut capture_values = Vec::with_capacity(captures.len());
                for (name, ty) in &captures {
                    let supported = matches!(
                        ty,
                        Type::I32 | Type::I64 | Type::F64 | Type::Bool | Type::Class(_)
                    ) || types.is_shared(*ty)
                        || types.is_owned(*ty);
                    if !supported {
                        target.runtime_function = None;
                        break;
                    }
                    let Some(local) = self.resolve_local(name) else {
                        target.runtime_function = None;
                        break;
                    };
                    let capture_ownership = if matches!(ty, Type::Class(_)) {
                        // Class references remain owned by their enclosing
                        // scope, so handler environments borrow them.
                        MirOwnership::Borrowed
                    } else if types.is_owned(*ty) {
                        match self.locals[local.0].ownership {
                            MirOwnership::Owned => MirOwnership::Owned,
                            MirOwnership::Borrowed => MirOwnership::Borrowed,
                            _ => {
                                target.runtime_function = None;
                                break;
                            }
                        }
                    } else {
                        MirOwnership::Copy
                    };
                    let value = self.next_value_with_ownership(*ty, capture_ownership);
                    if matches!(ty, Type::Class(_)) {
                        self.push_statement(MirStatement::Read {
                            destination: value,
                            local,
                        });
                    } else if capture_ownership == MirOwnership::Owned {
                        self.push_statement(MirStatement::TakeLocal {
                            destination: value,
                            local,
                        });
                    } else if capture_ownership == MirOwnership::Borrowed {
                        self.push_statement(MirStatement::BorrowLocal {
                            destination: value,
                            local,
                        });
                    } else {
                        self.push_statement(MirStatement::Read {
                            destination: value,
                            local,
                        });
                    }
                    capture_values.push(value);
                }
                if target.runtime_function.is_some() {
                    target.runtime_captures = capture_values;
                } else {
                    target.runtime_captures.clear();
                }
            }
        }
        let runtime_arms = frame
            .iter()
            .filter(|(operation, target)| {
                target.runtime_value.is_some()
                    && (target.parameters.is_empty()
                        || types
                            .effects()
                            .operation_info(**operation)
                            .is_some_and(|info| {
                                info.parameters
                                    .iter()
                                    .all(|ty| ownership_for_type(*ty, types) == MirOwnership::Copy)
                            }))
            })
            .map(|(operation, target)| {
                (
                    *operation,
                    target.runtime_value.clone().expect("runtime value"),
                )
            })
            .collect::<Vec<_>>();
        let forward_arms = frame
            .iter()
            .filter_map(|(operation, target)| {
                if target.runtime_function.is_some() {
                    return None;
                }
                target
                    .runtime_parameter
                    .map(|parameter| (*operation, parameter))
            })
            .collect::<Vec<_>>();
        let transform_arms = frame
            .iter()
            .filter_map(|(operation, target)| {
                if target.runtime_function.is_some() {
                    return None;
                }
                target
                    .runtime_transform
                    .map(|transform| (*operation, transform))
            })
            .collect::<Vec<_>>();
        let function_arms = frame
            .iter()
            .filter_map(|(operation, target)| {
                target
                    .runtime_function
                    .map(|function| (*operation, function))
            })
            .collect::<Vec<_>>();
        if (!runtime_arms.is_empty()
            || !forward_arms.is_empty()
            || !transform_arms.is_empty()
            || !function_arms.is_empty())
            && abort_target.is_none()
            && runtime_arms.len() + forward_arms.len() + transform_arms.len() + function_arms.len()
                == frame.len()
        {
            let resume_block = self.new_block();
            for (operation, _) in &runtime_arms {
                if let Some(target_info) = frame.get_mut(operation) {
                    target_info.runtime_resume_block = Some(resume_block);
                }
            }
            for (operation, _) in &function_arms {
                if let Some(target_info) = frame.get_mut(operation) {
                    target_info.runtime_resume_block = Some(resume_block);
                }
            }
            for (operation, _) in &forward_arms {
                if let Some(target_info) = frame.get_mut(operation) {
                    target_info.runtime_resume_block = Some(resume_block);
                }
            }
            for (operation, _) in &transform_arms {
                if let Some(target_info) = frame.get_mut(operation) {
                    target_info.runtime_resume_block = Some(resume_block);
                }
            }
            let target_block = self.new_block();
            let mut arms = runtime_arms
                .iter()
                .map(|(operation, target)| MirHandlerArm {
                    operation: *operation,
                    target: target_block,
                    parameter_indices: Vec::new(),
                    parameter_locals: Vec::new(),
                    join: resume_block,
                    result_type,
                    resumable_value: Some(target.clone()),
                    resumable_parameter: None,
                    resumable_transform: None,
                    resumable_function: None,
                    resumable_captures: Vec::new(),
                })
                .collect::<Vec<_>>();
            arms.extend(
                forward_arms
                    .iter()
                    .map(|(operation, parameter)| MirHandlerArm {
                        operation: *operation,
                        target: target_block,
                        parameter_indices: Vec::new(),
                        parameter_locals: Vec::new(),
                        join: resume_block,
                        result_type,
                        resumable_value: None,
                        resumable_parameter: Some(*parameter),
                        resumable_transform: None,
                        resumable_function: None,
                        resumable_captures: Vec::new(),
                    }),
            );
            arms.extend(
                transform_arms
                    .iter()
                    .map(|(operation, transform)| MirHandlerArm {
                        operation: *operation,
                        target: target_block,
                        parameter_indices: Vec::new(),
                        parameter_locals: Vec::new(),
                        join: resume_block,
                        result_type,
                        resumable_value: None,
                        resumable_parameter: None,
                        resumable_transform: Some(*transform),
                        resumable_function: None,
                        resumable_captures: Vec::new(),
                    }),
            );
            arms.extend(function_arms.iter().map(|(operation, function)| {
                MirHandlerArm {
                    operation: *operation,
                    target: target_block,
                    parameter_indices: Vec::new(),
                    parameter_locals: Vec::new(),
                    join: resume_block,
                    result_type,
                    resumable_value: None,
                    resumable_parameter: None,
                    resumable_transform: None,
                    resumable_function: Some(*function),
                    resumable_captures: frame
                        .get(operation)
                        .map(|target| target.runtime_captures.clone())
                        .unwrap_or_default(),
                }
            }));
            self.push_statement(MirStatement::HandlerEnter { handlers: arms });
            self.resumable_handlers.push(frame);
            let value = self.lower_value(body, function_names, types)?;
            self.resumable_handlers.pop();
            let body_block = self.current;
            self.switch_to(target_block);
            self.terminate(MirTerminator::Unreachable)?;
            if body_block != resume_block {
                self.switch_to(body_block);
                if self.is_open() {
                    self.terminate(MirTerminator::Goto {
                        target: resume_block,
                        arguments: Vec::new(),
                    })?;
                }
            }
            self.switch_to(resume_block);
            self.push_statement(MirStatement::HandlerExit);
            return Ok(value);
        }
        self.resumable_handlers.push(frame);
        let value = self.lower_value(body, function_names, types)?;
        self.resumable_handlers.pop();
        let Some(abort_target) = abort_target else {
            return Ok(value);
        };
        let join = self.new_block();
        let mut incoming = Vec::new();
        if let Some(value) = value {
            if self.is_open() {
                let from = self.current;
                self.terminate(MirTerminator::Goto {
                    target: join,
                    arguments: vec![value],
                })?;
                incoming.push((from, value));
            }
        }
        let abort_incoming = self
            .blocks
            .iter()
            .filter_map(|block| match block.terminator.as_ref() {
                Some(MirTerminator::Goto { target, arguments }) if *target == abort_target => {
                    arguments.first().copied().map(|value| (block.id, value))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if !abort_incoming.is_empty() {
            self.switch_to(abort_target);
            let abort_value = self.next_value(result_type);
            self.push_statement(MirStatement::Phi {
                destination: abort_value,
                incoming: abort_incoming,
            });
            let from = self.current;
            self.terminate(MirTerminator::Goto {
                target: join,
                arguments: vec![abort_value],
            })?;
            incoming.push((from, abort_value));
        }
        if incoming.is_empty() {
            return Ok(None);
        }
        self.switch_to(join);
        let result = self.next_value(result_type);
        self.push_statement(MirStatement::Phi {
            destination: result,
            incoming,
        });
        Ok(Some(result))
    }
}
