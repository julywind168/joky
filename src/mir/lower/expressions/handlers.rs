//! Lowering of `do` expressions and their effect handlers.

use super::super::*;
use super::effects::contains_aborting_effect;
use crate::hir::{CoreExpr, CoreHandlerArm, CorePattern};
use crate::mir::lower::synthetic_handlers::synthetic_handler_captures;

impl Lowerer<'_> {
    pub(super) fn lower_do(
        &mut self,
        expression_type: Type,
        body: &CoreExpr,
        handlers: &[CoreHandlerArm],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if handlers.iter().any(|handler| handler.resumes) {
            return self.lower_resumable_do(body, handlers, expression_type, function_names, types);
        }
        // Every Normal handler is represented by a runtime frame.
        // Constant arms are kept inline in the frame metadata, while
        // general pure arms use a canonical typed handler thunk. The
        // body can therefore continue through the normal continuation
        // ABI without a private task or task-failure transport.
        let runtime_normal_handlers = handlers
            .iter()
            .filter(|handler| {
                types.effects().operation_mode(handler.operation)
                    == Some(crate::sema::EffectMode::Normal)
            })
            .map(|handler| {
                (
                    handler,
                    Some(crate::sema::EffectMode::Normal),
                    resumable_runtime_constant(&handler.value, types),
                    self.handler_function_ids.get(&handler.value.id).copied(),
                )
            })
            .collect::<Vec<_>>();
        let single_direct_effect_body = crate::hir::is_single_effect_call(body);
        let has_abortive_handlers = handlers.iter().any(|handler| {
            types.effects().operation_mode(handler.operation)
                == Some(crate::sema::EffectMode::Aborts)
        });
        let runtime_handlers_supported =
            runtime_normal_handlers
                .iter()
                .all(|(_, mode, value, function)| {
                    *mode == Some(crate::sema::EffectMode::Normal)
                        && (value.is_some() || function.is_some())
                });
        if !runtime_normal_handlers.is_empty() && runtime_handlers_supported {
            let mut frame = HashMap::new();
            let mut arms = Vec::with_capacity(runtime_normal_handlers.len());
            let handler_join = self.new_block();
            for (handler, _, value, function) in runtime_normal_handlers {
                let target_block = self.new_block();
                self.blocks[target_block.0].terminator = Some(MirTerminator::Unreachable);
                let mut capture_values = Vec::new();
                if value.is_none() && function.is_some() {
                    for (name, ty) in synthetic_handler_captures(handler, types, function_names) {
                        let Some(local) = self.resolve_local(&name) else {
                            capture_values.clear();
                            break;
                        };
                        let capture =
                            self.next_value_with_ownership(ty, ownership_for_type(ty, types));
                        self.push_statement(MirStatement::Read {
                            destination: capture,
                            local,
                        });
                        capture_values.push(capture);
                    }
                }
                let arm = MirHandlerArm {
                    operation: handler.operation,
                    target: target_block,
                    parameter_indices: Vec::new(),
                    parameter_locals: Vec::new(),
                    join: handler_join,
                    result_type: handler.value.ty,
                    resumable_value: value,
                    resumable_parameter: None,
                    resumable_transform: None,
                    resumable_function: function,
                    resumable_captures: capture_values,
                };
                frame.insert(
                    handler.operation,
                    HandlerTarget {
                        arm: arm.clone(),
                        parameter_values: Vec::new(),
                        runtime_dispatch: true,
                    },
                );
                arms.push(arm);
            }
            // Mixed `Normal`/`Aborts` handlers keep the abortive
            // operation on the typed task-failure path, but all
            // Normal operations share the runtime frame above. This
            // avoids giving Normal requests a second, incompatible
            // task transport.
            let mut abortive_targets = Vec::new();
            for handler in handlers.iter().filter(|handler| {
                types.effects().operation_mode(handler.operation)
                    == Some(crate::sema::EffectMode::Aborts)
            }) {
                let operation = types
                    .effects()
                    .operation_info(handler.operation)
                    .ok_or_else(|| Diagnostic::codegen("MIR handler operation was not resolved"))?;
                if handler.parameters.iter().any(|pattern| {
                    !matches!(
                        pattern,
                        CorePattern::Binding { .. } | CorePattern::Wildcard { .. }
                    )
                }) {
                    return Err(Diagnostic::codegen(
                        "complex handler patterns are not supported by MIR",
                    ));
                }
                let target = self.new_block();
                self.blocks[target.0].scope_depth = self.scope_depth + 1;
                self.blocks[target.0].scoped = true;
                let mut parameter_indices = Vec::new();
                let parameter_locals = handler
                    .parameters
                    .iter()
                    .zip(&operation.parameters)
                    .enumerate()
                    .filter_map(|(index, (pattern, ty))| match pattern {
                        CorePattern::Binding { name, .. } => {
                            let mode = if operation.parameter_borrows[index] {
                                MirOwnership::Borrowed
                            } else {
                                ownership_for_type(*ty, types)
                            };
                            let local = self.new_local_with_ownership(name, *ty, mode);
                            self.locals[local.0].scope_depth = self.scope_depth + 1;
                            parameter_indices.push(index);
                            Some(local)
                        }
                        CorePattern::Wildcard { .. } => None,
                        CorePattern::EnumVariant { .. } | CorePattern::Tuple { .. } => {
                            unreachable!()
                        }
                    })
                    .collect::<Vec<_>>();
                let parameter_values = operation
                    .parameters
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        self.next_value_with_ownership(
                            *ty,
                            if operation.parameter_borrows[index] {
                                MirOwnership::Borrowed
                            } else {
                                ownership_for_type(*ty, types)
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                let arm = MirHandlerArm {
                    operation: handler.operation,
                    target,
                    parameter_indices,
                    parameter_locals,
                    join: handler_join,
                    result_type: expression_type,
                    resumable_value: None,
                    resumable_parameter: None,
                    resumable_transform: None,
                    resumable_function: None,
                    resumable_captures: Vec::new(),
                };
                frame.insert(
                    handler.operation,
                    HandlerTarget {
                        arm: arm.clone(),
                        parameter_values,
                        runtime_dispatch: false,
                    },
                );
                arms.push(arm.clone());
                abortive_targets.push((handler, frame[&handler.operation].clone()));
            }
            self.push_statement(MirStatement::HandlerEnter { handlers: arms });
            self.handlers.push(frame);
            // A Normal frame itself is synchronous, but its body may
            // still request an Aborts operation. In that case keep
            // the body in a structured task so failure propagation
            // remains typed; the task inherits this runtime frame
            // for any Normal requests it makes.
            let body_requires_task = has_abortive_handlers
                || contains_aborting_effect(body, self.function_bodies, types);
            let value = if !body_requires_task {
                self.lower_value(body, function_names, types)?
            } else {
                let scope = self.new_task_scope();
                self.push_statement(MirStatement::ScopeEnter {
                    scope,
                    region: false,
                });
                self.task_scopes.push(scope);
                let task = self.lower_task_create(body.id, body, scope, function_names, types)?;
                let body_destination = self.next_value(body.ty);
                self.push_statement(MirStatement::TaskJoin {
                    destination: body_destination,
                    scope,
                    task,
                });
                let continuation = self.new_block();
                if self.lower_task_failure_check(scope, continuation, types)? {
                    self.switch_to(continuation);
                }
                self.push_statement(MirStatement::TaskClaimResult { scope, task });
                self.push_statement(MirStatement::ScopeExit { scope });
                self.task_scopes.pop();
                Some(body_destination)
            };
            self.handlers.pop();
            let result = value.map(|value| {
                let destination =
                    self.next_value_with_ownership(body.ty, self.value_ownership[value.0]);
                (destination, value)
            });
            let mut incoming = Vec::new();
            if self.is_open() {
                let arguments = result
                    .as_ref()
                    .map(|(_, value)| vec![*value])
                    .unwrap_or_default();
                self.terminate(MirTerminator::Goto {
                    target: handler_join,
                    arguments,
                })?;
                if let Some((_, value)) = &result {
                    incoming.push((self.current, *value));
                }
            }
            for (handler, target) in &abortive_targets {
                self.switch_to(target.arm.target);
                for (index, value) in target.parameter_values.iter().enumerate() {
                    self.push_statement(MirStatement::Phi {
                        destination: *value,
                        incoming: self
                            .blocks
                            .iter()
                            .filter_map(|block| match block.terminator.as_ref() {
                                Some(MirTerminator::Goto {
                                    target: destination,
                                    arguments,
                                }) if *destination == target.arm.target => arguments
                                    .get(index)
                                    .copied()
                                    .map(|argument| (block.id, argument)),
                                _ => None,
                            })
                            .collect(),
                    });
                }
                self.bindings.push(HashMap::new());
                for (parameter_index, pattern) in handler.parameters.iter().enumerate() {
                    let value = target.parameter_values[parameter_index];
                    match pattern {
                        CorePattern::Binding { name, .. } => {
                            let local_index = target
                                .arm
                                .parameter_indices
                                .iter()
                                .position(|index| *index == parameter_index)
                                .expect("handler binding parameter index");
                            let local = target.arm.parameter_locals[local_index];
                            self.bind_local(name, local);
                            let destination = self.next_value(Type::Unit);
                            self.push_statement(MirStatement::Bind {
                                local,
                                value: Some(value),
                                destination,
                            });
                        }
                        CorePattern::Wildcard { .. }
                            if matches!(
                                self.value_ownership[value.0],
                                MirOwnership::Owned | MirOwnership::Shared
                            ) =>
                        {
                            let destination = self.next_value(Type::Unit);
                            self.push_statement(MirStatement::Drop { destination, value });
                        }
                        CorePattern::Wildcard { .. } => {}
                        CorePattern::EnumVariant { .. } | CorePattern::Tuple { .. } => {
                            unreachable!()
                        }
                    }
                }
                let Some(value) = self.lower_value(&handler.value, function_names, types)? else {
                    self.bindings.pop();
                    continue;
                };
                let from = self.current;
                if self.is_open() {
                    self.terminate(MirTerminator::Goto {
                        target: handler_join,
                        arguments: vec![value],
                    })?;
                    incoming.push((from, value));
                }
                self.bindings.pop();
            }
            self.switch_to(handler_join);
            let result_value = if let Some((destination, _)) = result {
                self.push_statement(MirStatement::Phi {
                    destination,
                    incoming,
                });
                Some(destination)
            } else {
                None
            };
            self.push_statement(MirStatement::HandlerExit);
            return Ok(result_value);
        }
        // A Normal-only frame must never fall through to the legacy
        // task/failure transport. If runtime registration could not
        // be built above, surface the unsupported handler shape now
        // instead of silently changing its control-flow semantics.
        if handlers.iter().any(|handler| {
            types.effects().operation_mode(handler.operation)
                == Some(crate::sema::EffectMode::Normal)
        }) {
            return Err(Diagnostic::codegen(
                "normal handler could not be lowered to a runtime continuation",
            ));
        }

        let join = self.new_block();
        let mut targets = Vec::with_capacity(handlers.len());
        let mut frame = HashMap::new();
        for handler in handlers {
            let operation = types
                .effects()
                .operation_info(handler.operation)
                .ok_or_else(|| Diagnostic::codegen("MIR handler operation was not resolved"))?;
            if operation.mode != crate::sema::EffectMode::Aborts {
                return Err(Diagnostic::codegen(
                    "only Aborts effect handlers use the legacy task transport",
                ));
            }
            if handler.parameters.iter().any(|pattern| {
                !matches!(
                    pattern,
                    CorePattern::Binding { .. } | CorePattern::Wildcard { .. }
                )
            }) {
                return Err(Diagnostic::codegen(
                    "complex handler patterns are not supported by MIR",
                ));
            }
            if frame.contains_key(&handler.operation) {
                return Err(Diagnostic::codegen("duplicate MIR handler operation"));
            }
            let target = self.new_block();
            self.blocks[target.0].scope_depth = self.scope_depth + 1;
            self.blocks[target.0].scoped = true;
            let mut parameter_indices = Vec::new();
            let parameter_locals = handler
                .parameters
                .iter()
                .zip(&operation.parameters)
                .enumerate()
                .filter_map(|(index, (pattern, ty))| match pattern {
                    CorePattern::Binding { name, .. } => {
                        let mode = if operation.parameter_borrows[index] {
                            MirOwnership::Borrowed
                        } else {
                            ownership_for_type(*ty, types)
                        };
                        let local = self.new_local_with_ownership(name, *ty, mode);
                        self.locals[local.0].scope_depth = self.scope_depth + 1;
                        parameter_indices.push(index);
                        Some(local)
                    }
                    CorePattern::Wildcard { .. } => None,
                    CorePattern::EnumVariant { .. } | CorePattern::Tuple { .. } => unreachable!(),
                })
                .collect::<Vec<_>>();
            let parameter_values = operation
                .parameters
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    self.next_value_with_ownership(
                        *ty,
                        if operation.parameter_borrows[index] {
                            MirOwnership::Borrowed
                        } else {
                            ownership_for_type(*ty, types)
                        },
                    )
                })
                .collect::<Vec<_>>();
            let arm = MirHandlerArm {
                operation: handler.operation,
                target,
                parameter_indices,
                parameter_locals,
                join,
                result_type: expression_type,
                resumable_value: None,
                resumable_parameter: None,
                resumable_transform: None,
                resumable_function: None,
                resumable_captures: Vec::new(),
            };
            let target_info = HandlerTarget {
                arm: arm.clone(),
                parameter_values,
                runtime_dispatch: false,
            };
            frame.insert(handler.operation, target_info.clone());
            targets.push((handler, target_info));
        }
        self.push_statement(MirStatement::HandlerEnter {
            handlers: targets
                .iter()
                .map(|(_, target)| target.arm.clone())
                .collect(),
        });
        self.handlers.push(frame);
        // Aborts handlers retain the typed task-failure transport;
        // Normal handlers have already been lowered through the
        // runtime continuation path above.
        let body_value = if single_direct_effect_body {
            let value = self.lower_value(body, function_names, types)?;
            self.handlers.pop();
            value
        } else {
            let scope = self.new_task_scope();
            self.push_statement(MirStatement::ScopeEnter {
                scope,
                region: false,
            });
            self.task_scopes.push(scope);
            let task = self.lower_task_create(body.id, body, scope, function_names, types)?;
            let body_destination = self.next_value(body.ty);
            self.push_statement(MirStatement::TaskJoin {
                destination: body_destination,
                scope,
                task,
            });
            let continuation = self.new_block();
            if self.lower_task_failure_check(scope, continuation, types)? {
                self.switch_to(continuation);
            }
            self.push_statement(MirStatement::TaskClaimResult { scope, task });
            self.push_statement(MirStatement::ScopeExit { scope });
            self.task_scopes.pop();
            self.handlers.pop();
            Some(body_destination)
        };
        let mut incoming = Vec::new();
        if let Some(value) = body_value {
            let from = self.current;
            if self.is_open() {
                self.terminate(MirTerminator::Goto {
                    target: join,
                    arguments: vec![value],
                })?;
                incoming.push((from, value));
            }
        }
        for (handler, target) in &targets {
            self.switch_to(target.arm.target);
            for (index, value) in target.parameter_values.iter().enumerate() {
                self.push_statement(MirStatement::Phi {
                    destination: *value,
                    incoming: self
                        .blocks
                        .iter()
                        .filter_map(|block| match block.terminator.as_ref() {
                            Some(MirTerminator::Goto {
                                target: destination,
                                arguments,
                            }) if *destination == target.arm.target => arguments
                                .get(index)
                                .copied()
                                .map(|argument| (block.id, argument)),
                            _ => None,
                        })
                        .collect(),
                });
            }
            self.bindings.push(HashMap::new());
            for (parameter_index, pattern) in handler.parameters.iter().enumerate() {
                let value = target.parameter_values[parameter_index];
                match pattern {
                    CorePattern::Binding { name, .. } => {
                        let local_index = target
                            .arm
                            .parameter_indices
                            .iter()
                            .position(|index| *index == parameter_index)
                            .expect("handler binding parameter index");
                        let local = target.arm.parameter_locals[local_index];
                        self.bind_local(name, local);
                        let destination = self.next_value(Type::Unit);
                        self.push_statement(MirStatement::Bind {
                            local,
                            value: Some(value),
                            destination,
                        });
                    }
                    CorePattern::Wildcard { .. }
                        if matches!(
                            self.value_ownership[value.0],
                            MirOwnership::Owned | MirOwnership::Shared
                        ) =>
                    {
                        let destination = self.next_value(Type::Unit);
                        self.push_statement(MirStatement::Drop { destination, value });
                    }
                    CorePattern::Wildcard { .. } => {}
                    CorePattern::EnumVariant { .. } | CorePattern::Tuple { .. } => unreachable!(),
                }
            }
            let Some(value) = self.lower_value(&handler.value, function_names, types)? else {
                self.bindings.pop();
                continue;
            };
            let from = self.current;
            if self.is_open() {
                self.terminate(MirTerminator::Goto {
                    target: join,
                    arguments: vec![value],
                })?;
                incoming.push((from, value));
            }
            self.bindings.pop();
        }
        // Handler bodies can themselves request an operation handled by an
        // enclosing frame. Refresh target Phi incoming edges after all
        // handler bodies have been lowered so those edges are represented.
        let mut reachable = HashSet::from([MirBlockId(0)]);
        let mut pending = vec![MirBlockId(0)];
        while let Some(block_id) = pending.pop() {
            let Some(terminator) = self.blocks[block_id.0].terminator.as_ref() else {
                continue;
            };
            let successors = match terminator {
                MirTerminator::Goto { target, .. } => vec![*target],
                MirTerminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![*then_block, *else_block],
                MirTerminator::Return(_) | MirTerminator::Unreachable => Vec::new(),
            };
            for successor in successors {
                if reachable.insert(successor) {
                    pending.push(successor);
                }
            }
        }
        for (_, target) in &targets {
            let incoming = (0..target.parameter_values.len())
                .map(|index| {
                    self.blocks
                        .iter()
                        .filter_map(|block| match block.terminator.as_ref() {
                            Some(MirTerminator::Goto {
                                target: destination,
                                arguments,
                            }) if *destination == target.arm.target => arguments
                                .get(index)
                                .copied()
                                .map(|argument| (block.id, argument)),
                            _ => None,
                        })
                        .filter(|(block, _)| reachable.contains(block))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            for (index, value) in target.parameter_values.iter().enumerate() {
                if let Some(MirStatement::Phi {
                    incoming: values, ..
                }) = self.blocks[target.arm.target.0].statements.get_mut(index)
                {
                    *values = incoming[index].clone();
                }
                debug_assert_eq!(
                    self.value_types[value.0],
                    types
                        .effects()
                        .operation_info(target.arm.operation)
                        .expect("handler operation")
                        .parameters[index]
                );
            }
        }
        self.switch_to(join);
        incoming.retain(|(block, _)| reachable.contains(block));
        if incoming.is_empty() {
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        }
        let destination = self.next_value(expression_type);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        self.push_statement(MirStatement::HandlerExit);
        Ok(Some(destination))
    }
}
