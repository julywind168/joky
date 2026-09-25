//! MIR statement compilation for the Cranelift backend.

use super::*;

pub(crate) fn compile_mir_statement(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
    base_environment: &mut Environment,
) -> Result<(), CodegenError> {
    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;
    let string_values = &mut *context.string_values;
    let RuntimeCallRefs {
        string_from: string_from_ref,
        allocate: allocate_ref,
        dup: dup_ref,
        ..
    } = context.refs.calls;

    match statement {
        MirStatement::Unit { destination } => {
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::Const { destination, value } => {
            let value = compile_mir_constant(
                builder,
                value,
                function.value_types[destination.0],
                pointer_type,
                types,
                string_values,
                string_from_ref,
                context.refs.calls.list_cons,
                context.refs.calls.map_insert,
                context.refs.calls.allocate,
                environment,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::Read { destination, local } => {
            let value = compile_mir_read(builder, *local, pointer_type, environment, types)?;
            values.insert(*destination, value);
        }
        MirStatement::BorrowLocal { destination, local } => {
            let value = compile_mir_read(builder, *local, pointer_type, environment, types)?;
            values.insert(*destination, value);
        }
        MirStatement::TakeLocal { destination, local } => {
            let value =
                environment
                    .take_local(*local)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!("MIR local {} was moved before codegen", local.0),
                    })?;
            values.insert(*destination, value);
        }
        MirStatement::Unary {
            destination,
            op,
            operand,
        } => {
            let operand =
                values
                    .get(operand)
                    .cloned()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "MIR unary operand was not compiled".to_owned(),
                    })?;
            let value = compile_unary_value(
                builder,
                *op,
                operand,
                function.value_types[destination.0],
                pointer_type,
                context.refs.calls.panic,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
        }
        MirStatement::Binary {
            destination,
            op,
            left,
            right,
        } => {
            let left = values
                .get(left)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR binary left operand was not compiled".to_owned(),
                })?;
            let right = values
                .get(right)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR binary right operand was not compiled".to_owned(),
                })?;
            let value = compile_binary_value(
                builder,
                *op,
                left,
                right,
                function.value_types[destination.0],
                pointer_type,
                context.refs.calls.panic,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
        }
        MirStatement::Numeric {
            destination,
            method,
            arguments,
        } => {
            let compiled = arguments
                .iter()
                .map(|argument| {
                    values
                        .get(argument)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR numeric operand was not compiled".to_owned(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let value = compile_numeric_method(
                builder,
                *method,
                &compiled,
                function.value_types[destination.0],
                pointer_type,
                context.refs.calls.panic,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            values.insert(*destination, value);
        }
        MirStatement::Call {
            destination,
            function: function_name,
            arguments,
            continuation,
        } => {
            if let Some(continuation) = continuation {
                return pending_calls::compile_pending_call(
                    builder,
                    context,
                    *destination,
                    *function_name,
                    None,
                    arguments,
                    *continuation,
                    values,
                    environment,
                );
            }
            // Native callbacks: a function-typed argument to an extern call
            // passes the C trampoline address instead of the two-word closure
            // value. The closure environment is released immediately because
            // C never observes it (zero-capture contract).
            if let Some(callee_type) = environment.function_types.get(function_name) {
                if callee_type.foreign {
                    for (index, parameter_type) in callee_type.parameters.iter().enumerate() {
                        if !matches!(parameter_type, crate::sema::Type::Function(_)) {
                            continue;
                        }
                        let Some(argument) = arguments.iter().find(|a| a.parameter == index) else {
                            continue;
                        };
                        let Some(trampoline) = environment.callback_trampoline_refs.get(
                            &crate::codegen::environment::CallbackKey::Literal(
                                *function_name,
                                argument.value,
                            ),
                        ) else {
                            continue;
                        };
                        let Some(CompiledValue::Function {
                            environment: closure_env,
                            ..
                        }) = values.get(&argument.value)
                        else {
                            continue;
                        };
                        let closure_env = *closure_env;
                        let addr = builder.ins().func_addr(pointer_type, *trampoline);
                        values.insert(
                            argument.value,
                            CompiledValue::NativeHandle {
                                pointer: addr,
                                ty: *parameter_type,
                            },
                        );
                        builder
                            .ins()
                            .call(context.refs.calls.managed_drop, &[closure_env]);
                    }
                }
            }
            let value = compile_mir_call(
                builder,
                function_name,
                arguments,
                values,
                environment,
                types,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::DynamicUpcast { destination, value } => {
            let result = super::super::values::compile_dynamic_upcast(
                builder,
                values[value].clone(),
                function.value_types[destination.0],
                pointer_type,
                types,
            )?;
            values.insert(*destination, result);
        }
        MirStatement::DynamicValue {
            destination,
            methods,
            value,
        } => {
            let result = super::super::values::compile_dynamic_environment(
                builder,
                function.value_types[destination.0],
                values[value].clone(),
                function.value_types[value.0],
                methods,
                environment,
                context.refs.calls.closure_allocate,
                pointer_type,
                types,
            )?;
            values.insert(*destination, result);
        }
        MirStatement::FunctionValue {
            destination,
            function,
            captures,
        } => {
            let reference = environment
                .closure_call_refs
                .get(function)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "closure call glue was not registered".to_owned(),
                })?
                .to_owned();
            let drop_reference = environment
                .closure_drop_refs
                .get(function)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "closure drop glue was not registered".to_owned(),
                })?
                .to_owned();
            let closure_type = context.function.value_types[destination.0];
            let Type::Function(signature_id) = closure_type else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR closure destination is not a function type".to_owned(),
                });
            };
            let user_parameter_count = types.function_type(signature_id).parameters.len();
            let target_type = environment.function_types.get(function).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "closure target function type was not registered".to_owned(),
                }
            })?;
            if target_type.parameters.len() < user_parameter_count
                || target_type.parameters.len() - user_parameter_count != captures.len()
            {
                return Err(CodegenError::RuntimeError {
                    message: "closure capture count does not match target signature".to_owned(),
                });
            }
            let capture_types = target_type.parameters[..captures.len()].to_vec();
            let captures = captures
                .iter()
                .map(|capture| {
                    values
                        .get(capture)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR closure capture was not compiled".to_owned(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let code = builder.ins().func_addr(pointer_type, reference);
            let drop_callback = builder.ins().func_addr(pointer_type, drop_reference);
            let value = compile_closure_environment(
                builder,
                code,
                closure_type,
                captures,
                capture_types,
                drop_callback,
                context.refs.calls.closure_allocate,
                pointer_type,
                types,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::CallIndirect {
            destination,
            callee,
            arguments,
            continuation,
            ..
        } => {
            if let Some(continuation) = continuation {
                let callee =
                    values
                        .get(callee)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR indirect callee was not compiled".to_owned(),
                        })?;
                return pending_calls::compile_pending_indirect_call(
                    builder,
                    context,
                    *destination,
                    callee,
                    arguments,
                    *continuation,
                    values,
                    environment,
                );
            }
            let callee = values
                .get(callee)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR indirect callee was not compiled".to_owned(),
                })?;
            let value =
                compile_mir_indirect_call(builder, callee, arguments, values, pointer_type, types)?;
            values.insert(*destination, value);
        }
        MirStatement::Project {
            destination,
            base,
            access,
        } => {
            let base = values
                .get(base)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR projection base was not compiled".to_owned(),
                })?;
            let value = compile_mir_project(builder, base, access, pointer_type, types)?;
            values.insert(*destination, value);
        }
        MirStatement::EnumTag { destination, value } => {
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR enum tag value was not compiled".to_owned(),
                })?;
            let CompiledValue::Enum { tag, .. } = value else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR enum tag reads a non-enum value".to_owned(),
                });
            };
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: tag,
                    ty: crate::sema::Type::I32,
                },
            );
        }
        MirStatement::EnumProject {
            destination,
            value,
            variant,
            field,
        } => {
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR enum projection value was not compiled".to_owned(),
                })?;
            let value = compile_mir_enum_project(value, *variant, *field, types)?;
            values.insert(*destination, value);
        }
        MirStatement::Tuple {
            destination,
            elements,
        } => {
            let tuple_values = elements
                .iter()
                .map(|element| {
                    values
                        .get(element)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR tuple element was not compiled".to_owned(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            values.insert(
                *destination,
                CompiledValue::Tuple {
                    elements: tuple_values,
                    ty: function.value_types[destination.0],
                },
            );
        }
        MirStatement::EnumConstruct {
            destination,
            enum_id,
            variant,
            arguments,
        } => {
            let value = compile_mir_enum_construct(
                builder,
                *enum_id,
                *variant,
                arguments,
                values,
                pointer_type,
                types,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::Construct {
            destination,
            type_id,
            fields,
        } => {
            let value = compile_mir_construct(
                builder,
                *type_id,
                fields,
                values,
                function.value_types[destination.0],
                allocate_ref,
                pointer_type,
                types,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::Store {
            destination,
            receiver,
            access,
            value,
        } => {
            let receiver =
                values
                    .get(receiver)
                    .cloned()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "MIR store receiver was not compiled".to_owned(),
                    })?;
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR store value was not compiled".to_owned(),
                })?;
            compile_mir_store(
                builder,
                receiver,
                access,
                value,
                pointer_type,
                types,
                environment,
            )?;
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::MethodCall {
            destination,
            receiver,
            receiver_type,
            method,
            arguments,
            continuation,
        } => {
            if let Some(continuation) = continuation {
                return pending_calls::compile_pending_call(
                    builder,
                    context,
                    *destination,
                    *method,
                    Some(*receiver),
                    arguments,
                    *continuation,
                    values,
                    environment,
                );
            }
            let receiver =
                values
                    .get(receiver)
                    .cloned()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "MIR method receiver was not compiled".to_owned(),
                    })?;
            let value = compile_mir_method_call(
                builder,
                receiver,
                *receiver_type,
                *method,
                arguments,
                values,
                environment,
                types,
            )?;
            values.insert(*destination, value);
        }
        MirStatement::RuntimeCall {
            destination,
            intrinsic,
            arguments,
        } => {
            compile_runtime_call(
                builder,
                &mut RuntimeCallContext {
                    types: context.types,
                    pointer_type: context.pointer_type,
                    refs: context.refs.calls,
                },
                destination,
                intrinsic,
                arguments,
                values,
                environment,
            )?;
        }
        MirStatement::ResumableRequest { .. } | MirStatement::HandlerRequest { .. } => {
            compile_resumable_request(builder, context, statement, values, environment)?;
        }
        MirStatement::CownAcquire {
            wait_for_change,
            destination,
            arguments,
            continuation,
        } => {
            let (handles, count) =
                super::super::pending::cown_handle_array(builder, pointer_type, arguments, values)?;
            let status = if *wait_for_change {
                builder.ins().iconst(cranelift_codegen::ir::types::I8, 1)
            } else {
                let attempt = builder
                    .ins()
                    .call(context.pending_refs.cown_try, &[handles, count]);
                builder.inst_results(attempt)[0]
            };
            let slow = builder.create_block();
            let done = builder.create_block();
            builder.append_block_param(done, cranelift_codegen::ir::types::I8);
            builder
                .ins()
                .brif(status, slow, &[], done, &[status.into()]);
            builder.switch_to_block(slow);
            super::continuations::save_continuation_frame(
                builder,
                context,
                continuation,
                values,
                environment,
                context.continuation_handles[continuation],
                context.continuation_frames.get(continuation).copied(),
                context.continuation_spills.get(continuation).copied(),
            )?;
            // A cancelled/invalid fast attempt must also transfer owned locals
            // into cleanup storage before the native error return.
            let start = builder.create_block();
            let contended =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, status, 1);
            builder
                .ins()
                .brif(contended, start, &[], done, &[status.into()]);
            builder.switch_to_block(start);
            let parent = context
                .pending_parent
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            let call = builder.ins().call(
                if *wait_for_change {
                    context.pending_refs.cown_wait
                } else {
                    context.pending_refs.cown_acquire
                },
                &[
                    context.continuation_handles[continuation],
                    handles,
                    count,
                    parent,
                ],
            );
            let pending = builder.inst_results(call)[0];
            builder.ins().jump(done, &[pending.into()]);
            builder.switch_to_block(done);
            builder.seal_block(slow);
            builder.seal_block(start);
            builder.seal_block(done);
            context.suspend_pending = Some(builder.block_params(done)[0]);
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::TaskWait {
            destination,
            scope,
            race,
            continuation,
        } => {
            super::continuations::save_continuation_frame(
                builder,
                context,
                continuation,
                values,
                environment,
                context.continuation_handles[continuation],
                context.continuation_frames.get(continuation).copied(),
                context.continuation_spills.get(continuation).copied(),
            )?;
            let group = environment.task_group(*scope).expect("verified wait scope");
            let parent = context
                .pending_parent
                .unwrap_or_else(|| builder.ins().iconst(context.pointer_type, 0));
            let mode = builder.ins().iconst(context.pointer_type, i64::from(*race));
            let call = builder.ins().call(
                context.pending_refs.task_wait,
                &[
                    context.continuation_handles[continuation],
                    group,
                    mode,
                    parent,
                ],
            );
            context.suspend_pending = Some(builder.inst_results(call)[0]);
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::Suspend { .. } => {
            compile_suspend(builder, context, statement, values, environment)?;
        }
        MirStatement::Resume { .. } => {
            compile_resume(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskPoll { .. } | MirStatement::TaskCancelled { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskAbort { .. }
        | MirStatement::TaskFailureOperation { .. }
        | MirStatement::TaskFailurePayload { .. }
        | MirStatement::TaskFailureClaim { .. }
        | MirStatement::TaskFailureRethrow { .. } => {
            compile_task_failure(builder, context, statement, values)?;
        }
        MirStatement::HandlerEnter { handlers } => {
            compile_handler_enter(builder, context, handlers, values, environment)?;
        }
        MirStatement::HandlerExit => {
            let frame =
                context
                    .tasks
                    .handler_frames
                    .pop()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "MIR handler frame exits before entering".to_owned(),
                    })?;
            builder
                .ins()
                .call(context.tasks.refs.handler_frame_exit, &[frame]);
            builder
                .ins()
                .call(context.tasks.refs.handler_frame_free, &[frame]);
        }
        MirStatement::ScopeEnter { .. } | MirStatement::ScopeExit { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskCreate { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskJoin { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskClaimResult { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::TaskCancel { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::RaceStart { .. } | MirStatement::RaceSelect { .. } => {
            compile_task_lifecycle(builder, context, statement, values, environment)?;
        }
        MirStatement::Dup { destination, value } => {
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR dup value was not compiled".to_owned(),
                })?;
            values.insert(
                *destination,
                duplicate_shared_value(builder, value, dup_ref)?,
            );
        }
        MirStatement::Move { destination, value } => {
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR move value was not compiled".to_owned(),
                })?;
            values.insert(*destination, value);
        }
        MirStatement::Drop { destination, value } => {
            let value = values
                .get(value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR drop value was not compiled".to_owned(),
                })?;
            compile_drop_value(builder, value, environment, pointer_type, types)?;
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::Deinit { destination, .. } => {
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::DropLocal { destination, local } => {
            let value =
                environment
                    .take_local(*local)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!(
                            "MIR local {} was dropped after move in {}",
                            local.0, context.function.name
                        ),
                    })?;
            compile_drop_value(builder, value, environment, pointer_type, types)?;
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::Bind {
            local,
            value,
            destination,
        } => {
            let value = value
                .map(|value| {
                    values
                        .get(&value)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR binding value was not compiled".to_owned(),
                        })
                })
                .transpose()?;
            environment.bind_local(*local, value.clone().unwrap_or(CompiledValue::Unit));
            if function.locals[local.0].scope_depth == 0 {
                base_environment.bind_local(*local, value.unwrap_or(CompiledValue::Unit));
            }
            values.insert(*destination, CompiledValue::Unit);
        }
        MirStatement::Phi { .. } => {}
    }

    Ok(())
}
