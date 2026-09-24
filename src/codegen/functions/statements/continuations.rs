//! Continuation-related MIR statement compilation for the Cranelift backend.

use super::*;

/// Explicit inputs for emitting a Normal/Resumable effect request. Shared by
/// the ordinary statement path and by continuation machine entries, whose
/// resume tails may issue further synchronous Normal handler requests.
pub(crate) struct HandlerRequestRefs<'a> {
    pub(crate) function: &'a crate::mir::MirFunction,
    pub(crate) types: &'a TypeTable,
    pub(crate) pointer_type: cranelift_codegen::ir::Type,
    pub(crate) dup: cranelift_codegen::ir::FuncRef,
    pub(crate) managed_drop: cranelift_codegen::ir::FuncRef,
    pub(crate) begin_resumption: cranelift_codegen::ir::FuncRef,
    pub(crate) begin_resumption_env: cranelift_codegen::ir::FuncRef,
    pub(crate) dispatch: cranelift_codegen::ir::FuncRef,
    pub(crate) payload_copy: cranelift_codegen::ir::FuncRef,
    pub(crate) payload_consume: cranelift_codegen::ir::FuncRef,
    pub(crate) free_resumption: cranelift_codegen::ir::FuncRef,
}

/// Compile a resumable or handler request and materialize its result payload.
pub(crate) fn compile_resumable_request(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let (destination, operation, arguments) = match statement {
        MirStatement::ResumableRequest {
            destination,
            operation,
            arguments,
            ..
        }
        | MirStatement::HandlerRequest {
            destination,
            operation,
            arguments,
            ..
        } => (destination, operation, arguments),
        _ => {
            return Err(CodegenError::RuntimeError {
                message: "non-request statement passed to resumable request compiler".to_owned(),
            });
        }
    };
    emit_handler_request(
        builder,
        &HandlerRequestRefs {
            function: context.function,
            types: context.types,
            pointer_type: context.pointer_type,
            dup: context.refs.calls.dup,
            managed_drop: context.refs.calls.managed_drop,
            begin_resumption: context
                .tasks
                .refs
                .handler_frame_begin_resumption_with_payload,
            begin_resumption_env: context
                .tasks
                .refs
                .handler_frame_begin_resumption_with_payload_env,
            dispatch: context.tasks.refs.handler_frame_dispatch_resumption,
            payload_copy: context.tasks.refs.handler_frame_resumption_payload_copy,
            payload_consume: context.tasks.refs.handler_frame_resumption_payload_consume,
            free_resumption: context.tasks.refs.handler_frame_free_resumption,
        },
        *destination,
        *operation,
        arguments,
        values,
        environment,
    )
}

/// Emit the request token round trip: publish the operation payload, dispatch
/// it to the active handler frame synchronously, and materialize the result
/// into `destination`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_handler_request(
    builder: &mut FunctionBuilder<'_>,
    refs: &HandlerRequestRefs<'_>,
    destination: MirValueId,
    operation: crate::sema::EffectOperationId,
    arguments: &[crate::mir::MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let function = refs.function;
    let types = refs.types;
    let pointer_type = refs.pointer_type;
    let ty = function.value_types[destination.0];
    let operation_id = ((operation.effect.0 as u64) << 32 | operation.operation as u64) as i64;
    let operation_value = builder.ins().iconst(types::I64, operation_id);
    let operation_info = refs
        .types
        .effects()
        .operation_info(operation)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "resumable request operation metadata is missing".to_owned(),
        })?;
    let mut payload_components = Vec::new();
    let mut request_managed = Vec::new();
    let mut request_shared = Vec::new();
    let mut consumed_arguments = Vec::new();
    let mut ordered_arguments = arguments.to_vec();
    ordered_arguments.sort_by_key(|argument| argument.parameter);
    for argument in &ordered_arguments {
        let value =
            values
                .get(&argument.value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumable request argument was not compiled".to_owned(),
                })?;
        let parameter_type = operation_info
            .parameters
            .get(argument.parameter)
            .copied()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "resumable request argument index is invalid".to_owned(),
            })?;
        // Shared values are borrowed by the generated handler body, so keep
        // the caller's reference alive and send a duplicated reference in the
        // request payload. The runtime tracks that duplicate separately and
        // either transfers it to the result or drops it after the thunk returns.
        let payload_value = if types.is_shared(parameter_type) {
            consumed_arguments.push(value.clone());
            duplicate_shared_value(builder, value.clone(), refs.dup)?
        } else {
            value.clone()
        };
        let components = value_arguments(payload_value);
        let component_types = abi_types(parameter_type, pointer_type, types);
        let component_count = component_types.len();
        if components.len() != component_types.len() {
            return Err(CodegenError::RuntimeError {
                message: "resumable request argument ABI shape mismatch".to_owned(),
            });
        }
        payload_components.extend(
            components
                .into_iter()
                .zip(component_types)
                .map(|(value, ty)| payload_word(builder, value, ty)),
        );
        let argument_offset = payload_components
            .len()
            .saturating_sub(component_count)
            .saturating_mul(8);
        if types.is_owned(parameter_type)
            && operation_info.parameter_borrows.get(argument.parameter) != Some(&true)
        {
            request_managed.extend(managed_pointer_types(
                parameter_type,
                argument_offset,
                types,
            ));
        } else if types.is_shared(parameter_type) {
            request_shared.extend(managed_pointer_types(
                parameter_type,
                argument_offset,
                types,
            ));
        }
    }
    let payload_word_count = payload_components.len();
    let (payload_slot, payload_pointer) =
        create_task_slot(builder, pointer_type, payload_word_count)?;
    for (index, value) in payload_components.into_iter().enumerate() {
        builder
            .ins()
            .stack_store(pointer_type, value, payload_slot, (index * 8) as i32);
    }
    let payload_size = builder
        .ins()
        .iconst(pointer_type, payload_word_count.saturating_mul(8) as i64);
    let token = if request_managed.is_empty() && request_shared.is_empty() {
        builder.ins().call(
            refs.begin_resumption,
            &[operation_value, payload_pointer, payload_size],
        )
    } else {
        let request_slots =
            request_managed
                .iter()
                .map(|(offset, ty)| (*offset, *ty, joky_runtime_abi::HandlerEnvOwnership::Owned))
                .chain(request_shared.iter().map(|(offset, ty)| {
                    (*offset, *ty, joky_runtime_abi::HandlerEnvOwnership::Shared)
                }))
                .collect::<Vec<_>>();
        let (descriptor_slot, descriptor_pointer) = create_task_slot(
            builder,
            pointer_type,
            request_slots.len() * (joky_runtime_abi::HANDLER_ENV_SLOT_SIZE as usize / 8),
        )?;
        for (index, (offset, leaf_type, ownership_kind)) in request_slots.iter().enumerate() {
            let base = (index as i32) * joky_runtime_abi::HANDLER_ENV_SLOT_SIZE;
            let offset_value = builder.ins().iconst(pointer_type, *offset as i64);
            let ownership = builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I8, *ownership_kind as i64);
            let drop_callback = match leaf_type {
                Type::Class(id) => {
                    let drop = environment.class_drop_refs.get(id).ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: format!("class {id} has no handler request drop glue"),
                        }
                    })?;
                    builder.ins().func_addr(pointer_type, *drop)
                }
                _ => builder.ins().func_addr(pointer_type, refs.managed_drop),
            };
            let offset_address = builder.ins().stack_addr(
                pointer_type,
                descriptor_slot,
                base + joky_runtime_abi::HANDLER_ENV_SLOT_OFFSET_OFFSET,
            );
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                offset_value,
                offset_address,
                0,
            );
            let ownership_address = builder.ins().stack_addr(
                pointer_type,
                descriptor_slot,
                base + joky_runtime_abi::HANDLER_ENV_SLOT_OWNERSHIP_OFFSET,
            );
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                ownership,
                ownership_address,
                0,
            );
            let drop_address = builder.ins().stack_addr(
                pointer_type,
                descriptor_slot,
                base + joky_runtime_abi::HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET,
            );
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                drop_callback,
                drop_address,
                0,
            );
        }
        let count = builder
            .ins()
            .iconst(pointer_type, request_slots.len() as i64);
        builder.ins().call(
            refs.begin_resumption_env,
            &[
                operation_value,
                payload_pointer,
                payload_size,
                descriptor_pointer,
                count,
            ],
        )
    };
    let token = builder.inst_results(token)[0];
    builder.ins().call(refs.dispatch, &[token]);
    for value in consumed_arguments {
        compile_drop_value(builder, value, environment, pointer_type, types)?;
    }
    let component_types = abi_types(ty, pointer_type, types);
    let (slot, pointer) = create_task_slot(builder, pointer_type, component_types.len())?;
    // An unhandled Normal request reports the failure through the runtime and
    // returns a null handle. Keep the subsequent cleanup path well-defined by
    // zero-initializing the result words before the best-effort payload copy.
    for index in 0..component_types.len() {
        let zero = builder.ins().iconst(pointer_type, 0);
        builder
            .ins()
            .stack_store(pointer_type, zero, slot, (index * 8) as i32);
    }
    let payload_size = component_types.len().saturating_mul(8);
    let size = builder.ins().iconst(pointer_type, payload_size as i64);
    let copied = builder
        .ins()
        .call(refs.payload_copy, &[token, pointer, size]);
    let _ = builder.inst_results(copied)[0];
    let components = component_types
        .into_iter()
        .enumerate()
        .map(|(index, component_type)| {
            builder
                .ins()
                .stack_load(pointer_type, component_type, slot, (index * 8) as i32)
        })
        .collect::<Vec<_>>();
    let value =
        value_from_params(&components, ty, types).map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
    builder.ins().call(refs.payload_consume, &[token]);
    builder.ins().call(refs.free_resumption, &[token]);
    values.insert(destination, value);
    Ok(())
}

/// Save the native environment at either an operation or a function call.
#[expect(
    clippy::too_many_arguments,
    reason = "The save boundary keeps SSA values, local bindings and optional frame/spill storage explicit."
)]
pub(super) fn save_continuation_frame(
    builder: &mut FunctionBuilder<'_>,
    context: &StatementContext<'_>,
    continuation: &crate::mir::MirContinuationId,
    values: &HashMap<MirValueId, CompiledValue>,
    environment: &Environment,
    handle: Value,
    frame: Option<Value>,
    spill: Option<Value>,
) -> Result<(), CodegenError> {
    for frame in &context.tasks.handler_frames {
        builder
            .ins()
            .call(context.pending_refs.own_handler, &[handle, *frame]);
    }
    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;
    let metadata = function
        .continuations
        .iter()
        .find(|item| item.id == *continuation)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "MIR suspend continuation metadata was not compiled".to_owned(),
        })?;
    let task_layout = super::super::task_frame::TaskFrameLayout::new(function, pointer_type, types);
    if let Some(frame) = &frame {
        for scope in &task_layout.scopes {
            let group = environment
                .task_group(*scope)
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                group,
                *frame,
                task_layout.scope_offset(*scope).unwrap(),
            );
        }
        for task in &task_layout.tasks {
            let id = environment
                .task_handle(*task)
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                id,
                *frame,
                task_layout.task_offset(*task).unwrap(),
            );
        }
    }
    if !metadata.frame_slots.is_empty() {
        let frame_pointer = frame.as_ref().ok_or_else(|| CodegenError::RuntimeError {
            message: format!("MIR continuation c{} has no frame area", continuation.0),
        })?;
        let local_layout =
            super::super::task_frame::LocalFrameLayout::new(function, pointer_type, types);
        for slot in &metadata.frame_slots {
            let mut frame_offset = local_layout.offset(slot.local) as i32;
            let value = slot
                .value
                .and_then(|value| values.get(&value).cloned())
                .or_else(|| environment.lookup_local(slot.local))
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!(
                        "MIR continuation frame local {} was not compiled",
                        slot.local.0
                    ),
                })?;
            let components = value_arguments(value);
            let component_types = abi_types(slot.ty, pointer_type, types);
            if components.len() != component_types.len() {
                return Err(CodegenError::RuntimeError {
                    message: "MIR continuation frame ABI shape mismatch".to_owned(),
                });
            }
            for component in components {
                builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    component,
                    *frame_pointer,
                    frame_offset,
                );
                frame_offset += 8;
            }
        }
    }
    let program_counter = builder
        .ins()
        .iconst(pointer_type, metadata.resume_block.0 as i64);
    builder.ins().call(
        context.refs.continuation_set_program_counter,
        &[handle, program_counter],
    );
    if let Some(pointer) = &spill {
        super::super::spills::SpillLayout::new(function, pointer_type, types)
            .save(builder, metadata, *pointer, values)?;
    }
    Ok(())
}

/// Compile a suspension point and publish its pending continuation result.
pub(crate) fn compile_suspend(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let MirStatement::Suspend {
        destination,
        operation,
        continuation,
        arguments,
        ..
    } = statement
    else {
        return Err(CodegenError::RuntimeError {
            message: "non-suspend statement passed to suspend compiler".to_owned(),
        });
    };
    save_continuation_frame(
        builder,
        context,
        continuation,
        values,
        environment,
        context.continuation_handles[continuation],
        context.continuation_frames.get(continuation).copied(),
        context.continuation_spills.get(continuation).copied(),
    )?;
    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;
    let handle = &context.continuation_handles[continuation];
    let info =
        types
            .effects()
            .operation_info(*operation)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "MIR suspend operation was not resolved".to_owned(),
            })?;
    if super::super::continuation::continuation_protocol(function, *continuation)?
        != crate::mir::MirContinuationKind::Suspending
    {
        return Err(CodegenError::RuntimeError {
            message: "MIR Suspend requires a suspending continuation".to_owned(),
        });
    }
    // A continuation handle may be reused by a machine entry for a later
    // suspension. Reset the operation-owned cleanup list and allocate the
    // exact result layout before the provider starts; this keeps differently
    // typed successive operations independent.
    builder
        .ins()
        .call(context.refs.continuation_clear_suspend_cleanups, &[*handle]);
    let result_size = abi_types(info.return_type, pointer_type, types)
        .len()
        .saturating_mul(8);
    if result_size != 0 {
        let size = builder.ins().iconst(pointer_type, result_size as i64);
        builder.ins().call(
            context.refs.continuation_alloc_suspend_result,
            &[*handle, size],
        );
    }
    register_continuation_cleanups(
        builder,
        *handle,
        CONTINUATION_SUSPEND_RESULT_STORAGE,
        info.return_type,
        0,
        pointer_type,
        types,
        context.refs.continuation_register_cleanup,
        context.refs.continuation_register_cleanup_region,
        context.refs.calls.managed_drop,
        &environment.class_drop_refs,
        context.continuation_cleanup_drop_refs,
    )?;
    let mut argument_offset = 0_usize;
    for (parameter_type, borrowed) in info.parameters.iter().zip(&info.parameter_borrows) {
        if *borrowed {
            argument_offset += abi_types(*parameter_type, pointer_type, types).len() * 8;
            continue;
        }
        register_continuation_cleanups(
            builder,
            *handle,
            CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
            *parameter_type,
            argument_offset,
            pointer_type,
            types,
            context.refs.continuation_register_cleanup,
            context.refs.continuation_register_cleanup_region,
            context.refs.calls.managed_drop,
            &environment.class_drop_refs,
            context.continuation_cleanup_drop_refs,
        )?;
        argument_offset += abi_types(*parameter_type, pointer_type, types).len() * 8;
    }
    let is_time_sleep = is_time_sleep_operation(context.types, *operation);
    let operation = builder.ins().iconst(
        cranelift_codegen::ir::types::I64,
        ((operation.effect.0 as u64) << 32 | operation.operation as u64) as i64,
    );
    let pending =
        if is_time_sleep {
            let [argument] = arguments.as_slice() else {
                return Err(CodegenError::RuntimeError {
                    message: "suspending operation expects one Duration argument".to_owned(),
                });
            };
            let CompiledValue::Numeric { value, ty } = values
                .get(&argument.value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR suspend duration was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR suspend duration is not numeric".to_owned(),
                });
            };
            if ty != Type::Duration {
                return Err(CodegenError::RuntimeError {
                    message: "MIR suspend duration has invalid type".to_owned(),
                });
            }
            let parent = context
                .pending_parent
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            let call = builder.ins().call(
                context.refs.continuation_start_pending_timer,
                &[*handle, operation, value, parent],
            );
            builder.inst_results(call)[0]
        } else {
            if arguments.len() != info.parameters.len() {
                return Err(CodegenError::RuntimeError {
                    message: "suspending operation argument count mismatch".to_owned(),
                });
            }
            let mut flattened = Vec::new();
            for (argument, parameter_type) in arguments.iter().zip(&info.parameters) {
                let value = values.get(&argument.value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MIR suspend argument was not compiled".to_owned(),
                    }
                })?;
                // MIR already gives each argument its own ownership share.
                // Transfer it to payload storage, as the machine entry does;
                // duplicating it again would leave the original share live.
                let components = value_arguments(value);
                let expected = abi_types(*parameter_type, pointer_type, types);
                if components.len() != expected.len() {
                    return Err(CodegenError::RuntimeError {
                        message: "MIR suspend argument ABI shape mismatch".to_owned(),
                    });
                }
                flattened.extend(components);
            }
            let (slot, pointer) = if flattened.is_empty() {
                (None, builder.ins().iconst(pointer_type, 0))
            } else {
                let (slot, pointer) = create_task_slot(builder, pointer_type, flattened.len())?;
                for (index, value) in flattened.iter().enumerate() {
                    builder.ins().store(
                        cranelift_codegen::ir::MemFlagsData::new(),
                        *value,
                        pointer,
                        (index * 8) as i32,
                    );
                }
                (Some(slot), pointer)
            };
            // Keep the explicit stack slot alive until the call has read the
            // argument buffer. Providers must copy arguments before returning if
            // they need them after this frame suspends.
            let _ = slot;
            let size = builder
                .ins()
                .iconst(pointer_type, (flattened.len() * 8) as i64);
            let parent = context
                .pending_parent
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            let call = builder.ins().call(
                context.refs.continuation_start_pending_provider,
                &[*handle, operation, pointer, size, parent],
            );
            builder.inst_results(call)[0]
        };
    context.suspend_pending = Some(pending);
    if info.return_type == Type::Unit {
        values.insert(*destination, CompiledValue::Unit);
    }
    Ok(())
}

/// Compile the resume side of a continuation protocol.
pub(crate) fn compile_resume(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    statement: &MirStatement,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let MirStatement::Resume { continuation } = statement else {
        return Err(CodegenError::RuntimeError {
            message: "non-resume statement passed to resume compiler".to_owned(),
        });
    };

    let function = context.function;
    let types = context.types;
    let pointer_type = context.pointer_type;
    let metadata = function
        .continuations
        .iter()
        .find(|item| item.id == *continuation)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "MIR resume continuation metadata was not compiled".to_owned(),
        })?;
    if metadata.kind.is_normal() {
        // Normal handler requests are synchronous: the runtime has already
        // materialized the request result before this block. There is no
        // machine entry to dispatch for this kind, and completing the
        // continuation here would eagerly run its cleanup list before a
        // managed value can be returned from the enclosing function. The
        // handle is released at the function return boundary instead.
        return Ok(());
    }
    if metadata.kind == crate::mir::MirContinuationKind::CownAcquire {
        // Native execution reaches this marker only through the successful
        // fast attempt; no continuation storage was populated or transferred.
        if let Some(destination) = metadata.resume_destination {
            values.insert(destination, CompiledValue::Unit);
        }
        return Ok(());
    }
    if metadata.is_function_call() {
        let slot = context.call_handle_slots[continuation];
        let handle = builder
            .ins()
            .stack_load(pointer_type, pointer_type, slot, 0);
        let destination = metadata
            .resume_destination
            .expect("verified call destination");
        let (status, value) = super::super::pending::take_result(
            builder,
            context.pending_refs.take,
            handle,
            function.value_types[destination.0],
            pointer_type,
            types,
        )?;
        super::super::pending::return_status_if_error(builder, status, |builder| {
            super::free_native_reservations(builder, context)
        });
        values.insert(destination, value);
        // Ready keeps the native environment as the owner. Saved copies were
        // needed only if this call had returned Pending; disarm their words
        // before the activation is freed at the native return.
        {
            let call = builder.ins().call(context.pending_refs.frame, &[handle]);
            let frame_value = builder.inst_results(call)[0];
            let frame = &frame_value;
            let local_layout =
                super::super::task_frame::LocalFrameLayout::new(function, pointer_type, types);
            for slot in &metadata.frame_slots {
                let offset = local_layout.offset(slot.local);
                for pointer_offset in managed_pointer_offsets(slot.ty, offset, types) {
                    let zero = builder.ins().iconst(pointer_type, 0);
                    builder.ins().store(
                        cranelift_codegen::ir::MemFlagsData::new(),
                        zero,
                        *frame,
                        pointer_offset as i32,
                    );
                }
            }
            let layout =
                super::super::task_frame::TaskFrameLayout::new(function, pointer_type, types);
            for scope in &layout.scopes {
                let zero = builder.ins().iconst(pointer_type, 0);
                builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    zero,
                    *frame,
                    layout.scope_offset(*scope).unwrap(),
                );
            }
        }
        {
            let call = builder.ins().call(context.pending_refs.spill, &[handle]);
            let spill_value = builder.inst_results(call)[0];
            let spill = &spill_value;
            let layout = super::super::spills::SpillLayout::new(function, pointer_type, types);
            for (value, offset) in &layout.slots {
                for pointer_offset in
                    managed_pointer_offsets(function.value_types[value.0], *offset, types)
                {
                    let zero = builder.ins().iconst(pointer_type, 0);
                    builder.ins().store(
                        cranelift_codegen::ir::MemFlagsData::new(),
                        zero,
                        *spill,
                        pointer_offset as i32,
                    );
                }
            }
        }
        builder
            .ins()
            .call(context.pending_refs.discard_ready, &[handle]);
        let zero = builder.ins().iconst(pointer_type, 0);
        builder.ins().stack_store(pointer_type, zero, slot, 0);
        return Ok(());
    }
    let handle = context
        .continuation_handles
        .get(continuation)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "MIR resume continuation handle was not compiled".to_owned(),
        })?;
    // This path still owns a native frame. Its ScopeExit statements retain
    // responsibility for the groups when a provider completes synchronously.
    if let Some(frame) = context.continuation_frames.get(continuation) {
        let layout = super::super::task_frame::TaskFrameLayout::new(function, pointer_type, types);
        for scope in &layout.scopes {
            let zero = builder.ins().iconst(pointer_type, 0);
            builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                zero,
                *frame,
                layout.scope_offset(*scope).unwrap(),
            );
        }
    }
    let generation = builder
        .ins()
        .iconst(pointer_type, metadata.generation as i64);
    builder
        .ins()
        .call(context.refs.continuation_resume_at, &[*handle, generation]);
    if metadata.kind.is_suspending() {
        let destination =
            metadata
                .resume_destination
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR suspend continuation has no result destination".to_owned(),
                })?;
        let result_type = function.value_types[destination.0];
        let component_types = abi_types(result_type, pointer_type, types);
        if component_types.is_empty() {
            values.insert(destination, CompiledValue::Unit);
        } else {
            let result_call = builder
                .ins()
                .call(context.refs.continuation_suspend_result_pointer, &[*handle]);
            let result_pointer = builder.inst_results(result_call)[0];
            let mut offset = 0;
            let components = component_types
                .into_iter()
                .map(|component_type| {
                    let value = builder.ins().load(
                        component_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        result_pointer,
                        offset,
                    );
                    offset += 8;
                    value
                })
                .collect::<Vec<_>>();
            let value = value_from_params(&components, result_type, types).map_err(|error| {
                CodegenError::RuntimeError {
                    message: format!("MIR suspend result restore failed: {error}"),
                }
            })?;
            // Ownership moves from the provider result slot into the resumed
            // SSA value. Clear every managed pointer word so cancellation
            // cleanup cannot release it a second time.
            for pointer_offset in managed_pointer_offsets(result_type, 0, types) {
                let zero = builder.ins().iconst(pointer_type, 0);
                builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    zero,
                    result_pointer,
                    pointer_offset as i32,
                );
            }
            values.insert(destination, value);
        }
        // The payload has been materialized into the SSA value. Its
        // continuation-owned byte buffer can now be released; managed pointer
        // words were cleared above before this call.
        builder
            .ins()
            .call(context.refs.continuation_release_suspend_result, &[*handle]);
    }
    // The native invocation of a pending call may still return Ready, in
    // which case this frame keeps owning the locals. A stackless machine
    // entry arrives without them, so materialize any missing SSA value from
    // the frame and rebind its local before compiling the resume tail.
    if !metadata.frame_slots.is_empty() {
        let frame_pointer = context
            .continuation_frames
            .get(continuation)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!("MIR continuation c{} has no frame area", continuation.0),
            })?;
        let local_layout =
            super::super::task_frame::LocalFrameLayout::new(function, pointer_type, types);
        for slot in &metadata.frame_slots {
            let mut frame_offset = local_layout.offset(slot.local) as i32;
            let component_types = abi_types(slot.ty, pointer_type, types);
            let should_restore = slot.value.is_some_and(|value| !values.contains_key(&value));
            let mut components = Vec::with_capacity(component_types.len());
            for component_type in component_types {
                let component = builder.ins().load(
                    component_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    *frame_pointer,
                    frame_offset,
                );
                frame_offset += 8;
                components.push(component);
            }
            if should_restore {
                let value = value_from_params(&components, slot.ty, types).map_err(|error| {
                    CodegenError::RuntimeError {
                        message: format!("MIR continuation frame restore failed: {error}"),
                    }
                })?;
                if let Some(value_id) = slot.value {
                    values.insert(value_id, value.clone());
                    environment.bind_local(slot.local, value);
                }
            }
        }
    }
    if let Some(pointer) = context.continuation_spills.get(continuation) {
        super::super::spills::SpillLayout::new(function, pointer_type, types).restore(
            builder,
            function,
            metadata,
            *pointer,
            values,
            pointer_type,
            types,
        )?;
    }
    if metadata.kind.is_suspending() && metadata.operation.is_some() {
        let operation = builder.ins().iconst(
            cranelift_codegen::ir::types::I64,
            ({
                let operation = metadata
                    .operation
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "suspending continuation has no effect operation".to_owned(),
                    })?;
                (operation.effect.0 as u64) << 32 | operation.operation as u64
            }) as i64,
        );
        builder.ins().call(
            context.refs.continuation_complete_suspend,
            &[*handle, operation],
        );
    } else {
        builder
            .ins()
            .call(context.refs.continuation_complete, &[*handle]);
    }
    Ok(())
}
