//! Task-related MIR statement compilation for the Cranelift backend.

use super::*;

/// Compile one of the task failure transport statements.
///
/// The dispatcher only passes matching MIR variants here, so an unexpected
/// variant is reported as an internal codegen error.
pub(crate) fn compile_task_failure(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
) -> Result<(), CodegenError> {
    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;

    match statement {
        MirStatement::TaskAbort {
            destination,
            operation,
            arguments,
        } => {
            let drop_payload_ref = context.tasks.failure_drops.get(operation).copied();
            let encoded_operation =
                ((operation.effect.0 as u64) << 32) | operation.operation as u64;
            let encoded_operation = builder.ins().iconst(types::I64, encoded_operation as i64);
            if arguments.is_empty() {
                builder
                    .ins()
                    .call(context.tasks.refs.abort, &[encoded_operation]);
            } else {
                let mut flattened = Vec::new();
                for argument in arguments {
                    let value = values.get(argument).cloned().ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "MIR abort argument was not compiled".to_owned(),
                        }
                    })?;
                    flattened.extend(value_arguments(value));
                }
                let (slot, pointer) = create_task_slot(builder, pointer_type, flattened.len())?;
                for (index, value) in flattened.iter().enumerate() {
                    builder
                        .ins()
                        .stack_store(pointer_type, *value, slot, (index * 8) as i32);
                }
                let size = builder
                    .ins()
                    .iconst(pointer_type, (flattened.len() * 8) as i64);
                let drop_payload = drop_payload_ref
                    .map(|drop| builder.ins().func_addr(pointer_type, drop))
                    .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                builder.ins().call(
                    context.tasks.refs.abort_payload,
                    &[encoded_operation, pointer, size, drop_payload],
                );
            }
            let value = super::super::super::constructors::zero_compiled_value(
                builder,
                function.value_types[destination.0],
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
            Ok(())
        }
        MirStatement::TaskFailureOperation { destination, scope } => {
            let group = task_group(&context.tasks, *scope)?;
            let call = builder
                .ins()
                .call(context.tasks.refs.failure_operation, &[group]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
            Ok(())
        }
        MirStatement::TaskFailurePayload {
            destination,
            scope,
            operation,
            parameter,
        } => {
            let group = task_group(&context.tasks, *scope)?;
            let call = builder
                .ins()
                .call(context.tasks.refs.failure_payload, &[group]);
            let payload = builder.inst_results(call)[0];
            let operation = types.effects().operation_info(*operation).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "task failure payload operation is unknown".to_owned(),
                }
            })?;
            let parameter_type =
                operation
                    .parameters
                    .get(*parameter)
                    .copied()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "task failure payload parameter is invalid".to_owned(),
                    })?;
            let word_offset = operation.parameters[..*parameter]
                .iter()
                .map(|ty| abi_types(*ty, pointer_type, types).len())
                .sum::<usize>();
            let components = abi_types(parameter_type, pointer_type, types)
                .into_iter()
                .enumerate()
                .map(|(index, ty)| {
                    builder.ins().load(
                        ty,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        payload,
                        ((word_offset + index) * 8) as i32,
                    )
                })
                .collect::<Vec<_>>();
            let value = value_from_params(&components, parameter_type, types).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            values.insert(*destination, value);
            Ok(())
        }
        MirStatement::TaskFailureClaim { scope } => {
            let group = task_group(&context.tasks, *scope)?;
            builder
                .ins()
                .call(context.tasks.refs.failure_claim, &[group]);
            Ok(())
        }
        MirStatement::TaskFailureRethrow { destination, scope } => {
            let group = task_group(&context.tasks, *scope)?;
            builder
                .ins()
                .call(context.tasks.refs.failure_rethrow, &[group]);
            let value = super::super::super::constructors::zero_compiled_value(
                builder,
                function.value_types[destination.0],
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
            Ok(())
        }
        _ => Err(CodegenError::RuntimeError {
            message: "non-failure statement passed to task failure compiler".to_owned(),
        }),
    }
}

/// Compile task scopes, task handles and race results.
pub(crate) fn compile_task_lifecycle(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;

    match statement {
        MirStatement::TaskPoll { destination } => {
            let call = builder.ins().call(context.tasks.refs.is_cancelled, &[]);
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
        }
        MirStatement::TaskCancelled { destination } => {
            builder
                .ins()
                .call(context.tasks.refs.mark_cancelled_result, &[]);
            let value = super::super::super::constructors::zero_compiled_value(
                builder,
                function.value_types[destination.0],
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
        }
        MirStatement::ScopeEnter { scope, region } => {
            let region = builder.ins().iconst(pointer_type, i64::from(*region));
            let call = builder.ins().call(context.tasks.refs.group_new, &[region]);
            environment.bind_task_group(*scope, builder.inst_results(call)[0]);
            context
                .tasks
                .groups
                .insert(*scope, builder.inst_results(call)[0]);
        }
        MirStatement::ScopeExit { scope } => {
            if scope.0 == 0 && !context.tasks.groups.contains_key(scope) {
                return Ok(());
            }
            let group = context.tasks.groups.get(scope).copied().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: format!("task scope s{} exits before entering", scope.0),
                }
            })?;
            builder.ins().call(context.tasks.refs.group_free, &[group]);
            environment.forget_task_group(*scope, function);
        }
        MirStatement::TaskCreate {
            scope,
            task,
            function: task_function,
            arguments,
        } => {
            let group = task_group(&context.tasks, *scope)?;
            let function_type =
                context
                    .tasks
                    .function_types
                    .get(task_function)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!(
                            "task function {} has no codegen signature",
                            task_function.0
                        ),
                    })?;
            if function_type.receiver.is_some() {
                return Err(CodegenError::RuntimeError {
                    message: "task functions with an implicit receiver are not supported"
                        .to_owned(),
                });
            }
            let thunk = context
                .tasks
                .thunks
                .get(task_function)
                .copied()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!("task function {} has no thunk", task_function.0),
                })?;
            let mut flattened = Vec::new();
            for (parameter, ty) in function_type.parameters.iter().enumerate() {
                let argument = arguments
                    .iter()
                    .find(|argument| argument.parameter == parameter)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!(
                            "task function {} is missing capture {}",
                            task_function.0, parameter
                        ),
                    })?;
                let value = values.get(&argument.value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "task capture value was not compiled".to_owned(),
                    }
                })?;
                let actual = super::super::super::abi::value_type(value.clone());
                if actual != *ty {
                    return Err(CodegenError::RuntimeError {
                        message: "task capture type does not match its thunk parameter".to_owned(),
                    });
                }
                flattened.extend(value_arguments(value));
            }
            if arguments.len() != function_type.parameters.len() {
                return Err(CodegenError::RuntimeError {
                    message: "task capture list has duplicate or unknown parameters".to_owned(),
                });
            }
            let (capture_slot, capture_pointer) =
                create_task_slot(builder, pointer_type, flattened.len())?;
            for (index, value) in flattened.iter().enumerate() {
                builder
                    .ins()
                    .stack_store(pointer_type, *value, capture_slot, (index * 8) as i32);
            }
            let result_components = abi_types(function_type.return_type, pointer_type, types);
            if function
                .continuations
                .iter()
                .any(|continuation| continuation.kind.is_suspending())
            {
                let result_size = builder
                    .ins()
                    .iconst(pointer_type, (result_components.len() * 8) as i64);
                let capture_size = builder
                    .ins()
                    .iconst(pointer_type, (flattened.len() * 8) as i64);
                let result_drop = context
                    .tasks
                    .result_drops
                    .get(task_function)
                    .copied()
                    .map(|drop| builder.ins().func_addr(pointer_type, drop))
                    .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
                let thunk = builder.ins().func_addr(pointer_type, thunk);
                let call = builder.ins().call(
                    context.tasks.refs.spawn_heap,
                    &[
                        group,
                        thunk,
                        capture_pointer,
                        capture_size,
                        result_size,
                        result_drop,
                    ],
                );
                let id = builder.inst_results(call)[0];
                let call = builder
                    .ins()
                    .call(context.tasks.refs.result_pointer, &[group, id]);
                let result_pointer = builder.inst_results(call)[0];
                environment.bind_task_handle(*task, id);
                context.tasks.tasks.insert(
                    *task,
                    CompiledTask {
                        scope: *scope,
                        id,
                        result_pointer,
                        result_type: function_type.return_type,
                    },
                );
                return Ok(());
            }
            let (_result_slot, result_pointer) =
                create_task_slot(builder, pointer_type, result_components.len())?;
            let context_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                TASK_CONTEXT_SIZE,
                3,
            ));
            let context_pointer = builder.ins().stack_addr(pointer_type, context_slot, 0);
            builder.ins().stack_store(
                pointer_type,
                capture_pointer,
                context_slot,
                TASK_CONTEXT_CAPTURES_OFFSET,
            );
            builder.ins().stack_store(
                pointer_type,
                result_pointer,
                context_slot,
                TASK_CONTEXT_RESULT_OFFSET,
            );
            let result_size = builder
                .ins()
                .iconst(pointer_type, (result_components.len() * 8) as i64);
            builder.ins().stack_store(
                pointer_type,
                result_size,
                context_slot,
                TASK_CONTEXT_RESULT_SIZE_OFFSET,
            );
            let no_cancellation = builder.ins().iconst(pointer_type, 0);
            builder.ins().stack_store(
                pointer_type,
                no_cancellation,
                context_slot,
                TASK_CONTEXT_CANCELLATION_OFFSET,
            );
            builder.ins().stack_store(
                pointer_type,
                no_cancellation,
                context_slot,
                TASK_CONTEXT_CONTINUATION_OFFSET,
            );
            let result_drop = context
                .tasks
                .result_drops
                .get(task_function)
                .copied()
                .map(|drop| builder.ins().func_addr(pointer_type, drop))
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            builder.ins().stack_store(
                pointer_type,
                result_drop,
                context_slot,
                TASK_CONTEXT_DROP_RESULT_OFFSET,
            );
            let not_cancelled = builder.ins().iconst(types::I8, 0);
            builder.ins().stack_store(
                pointer_type,
                not_cancelled,
                context_slot,
                TASK_CONTEXT_CANCELLED_RESULT_OFFSET,
            );
            builder.ins().stack_store(
                pointer_type,
                not_cancelled,
                context_slot,
                TASK_CONTEXT_RESULT_INITIALIZED_OFFSET,
            );
            let thunk = builder.ins().func_addr(pointer_type, thunk);
            let call = builder
                .ins()
                .call(context.tasks.refs.spawn, &[group, thunk, context_pointer]);
            environment.bind_task_handle(*task, builder.inst_results(call)[0]);
            context.tasks.tasks.insert(
                *task,
                CompiledTask {
                    scope: *scope,
                    id: builder.inst_results(call)[0],
                    result_pointer,
                    result_type: function_type.return_type,
                },
            );
        }
        MirStatement::TaskJoin {
            destination,
            scope,
            task,
        } => {
            let group = task_group(&context.tasks, *scope)?;
            let task = context
                .tasks
                .tasks
                .get(task)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "task join references a task that was not created".to_owned(),
                })?;
            if task.scope != *scope {
                return Err(CodegenError::RuntimeError {
                    message: "task join crosses task scopes".to_owned(),
                });
            }
            builder
                .ins()
                .call(context.tasks.refs.join, &[group, task.id]);
            let value = task_result(builder, task, pointer_type, types)?;
            values.insert(*destination, value);
        }
        MirStatement::TaskClaimResult { scope, task } => {
            let group = task_group(&context.tasks, *scope)?;
            let task = context
                .tasks
                .tasks
                .get(task)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "task result claim references a task that was not created".to_owned(),
                })?;
            if task.scope != *scope {
                return Err(CodegenError::RuntimeError {
                    message: "task result claim crosses task scopes".to_owned(),
                });
            }
            builder
                .ins()
                .call(context.tasks.refs.claim_result, &[group, task.id]);
        }
        MirStatement::TaskCancel { scope, task } => {
            let group = task_group(&context.tasks, *scope)?;
            let task = context
                .tasks
                .tasks
                .get(task)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "task cancellation references a task that was not created".to_owned(),
                })?;
            builder
                .ins()
                .call(context.tasks.refs.cancel, &[group, task.id]);
        }
        MirStatement::RaceStart { .. } => {}
        MirStatement::RaceSelect {
            destination,
            scope,
            tasks,
        } => {
            let group = task_group(&context.tasks, *scope)?;
            let first = tasks.first().ok_or_else(|| CodegenError::RuntimeError {
                message: "race selection has no tasks".to_owned(),
            })?;
            let result_type = context
                .tasks
                .tasks
                .get(first)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "race references a task that was not created".to_owned(),
                })?
                .result_type;
            let mut ids = Vec::with_capacity(tasks.len());
            for id in tasks {
                let task =
                    context
                        .tasks
                        .tasks
                        .get(id)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "race references a task that was not created".to_owned(),
                        })?;
                if task.scope != *scope || task.result_type != result_type {
                    return Err(CodegenError::RuntimeError {
                        message: "race tasks do not share a scope and result type".to_owned(),
                    });
                }
                ids.push(task.id);
            }
            let (ids_slot, ids_pointer) = create_task_slot(builder, pointer_type, ids.len())?;
            for (index, id) in ids.iter().enumerate() {
                builder
                    .ins()
                    .stack_store(types::I64, *id, ids_slot, (index * 8) as i32);
            }
            let component_count = abi_types(result_type, pointer_type, types).len();
            let (result_slot, result_pointer) =
                create_task_slot(builder, pointer_type, component_count)?;
            let zero_result = super::super::super::constructors::zero_compiled_value(
                builder,
                result_type,
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            for (index, value) in value_arguments(zero_result).iter().enumerate() {
                builder
                    .ins()
                    .stack_store(pointer_type, *value, result_slot, (index * 8) as i32);
            }
            let task_count = builder.ins().iconst(pointer_type, ids.len() as i64);
            let result_size = builder
                .ins()
                .iconst(pointer_type, (component_count * 8) as i64);
            builder.ins().call(
                context.tasks.refs.race,
                &[group, ids_pointer, task_count, result_pointer, result_size],
            );
            let result = CompiledTask {
                scope: *scope,
                id: builder.ins().iconst(types::I64, 0),
                result_pointer,
                result_type,
            };
            values.insert(
                *destination,
                task_result(builder, &result, pointer_type, types)?,
            );
        }
        _ => {
            return Err(CodegenError::RuntimeError {
                message: "non-lifecycle statement passed to task lifecycle compiler".to_owned(),
            });
        }
    }
    Ok(())
}
