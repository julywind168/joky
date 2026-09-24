//! Handler statement compilation for the Cranelift backend.

use super::*;

pub(crate) fn compile_handler_enter(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    handlers: &[crate::mir::MirHandlerArm],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let frame = emit_handler_enter(
        builder,
        context.function,
        context.types,
        context.pointer_type,
        context.string_values,
        &context.tasks,
        context.refs.calls,
        handlers,
        values,
        environment,
    )?;
    context.tasks.handler_frames.push(frame);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::functions) fn emit_handler_enter(
    builder: &mut FunctionBuilder<'_>,
    mir_function: &crate::mir::MirFunction,
    types: &TypeTable,
    pointer_type: cranelift_codegen::ir::Type,
    string_values: &mut dyn Iterator<Item = (super::super::context::StringValue, usize)>,
    tasks: &TaskCodegenContext<'_>,
    calls: RuntimeCallRefs,
    handlers: &[crate::mir::MirHandlerArm],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<Value, CodegenError> {
    let (slot, operations) = create_task_slot(builder, pointer_type, handlers.len())?;
    for (index, handler) in handlers.iter().enumerate() {
        let operation =
            ((handler.operation.effect.0 as u64) << 32) | handler.operation.operation as u64;
        let encoded = builder.ins().iconst(types::I64, operation as i64);
        builder
            .ins()
            .stack_store(types::I64, encoded, slot, (index * 8) as i32);
    }
    let count = builder.ins().iconst(pointer_type, handlers.len() as i64);
    let call = builder
        .ins()
        .call(tasks.refs.handler_frame_new, &[operations, count]);
    let frame = builder.inst_results(call)[0];
    builder.ins().call(tasks.refs.handler_frame_enter, &[frame]);
    for handler in handlers {
        let operation_id =
            ((handler.operation.effect.0 as u64) << 32 | handler.operation.operation as u64) as i64;
        let operation_value = builder.ins().iconst(types::I64, operation_id);
        if let Some(value) = &handler.resumable_value {
            if let MirConstant::String(_) = value {
                let compiled = compile_payload_constant(
                    builder,
                    value,
                    handler.result_type,
                    types,
                    pointer_type,
                    calls,
                    environment,
                    string_values,
                )?;
                let CompiledValue::String { pointer, length } = compiled else {
                    return Err(CodegenError::RuntimeError {
                        message: "string resumable payload did not compile as String".to_owned(),
                    });
                };
                builder.ins().call(
                    tasks.refs.handler_frame_set_resumption_string,
                    &[frame, operation_value, pointer, length],
                );
                let thunk = builder
                    .ins()
                    .func_addr(pointer_type, tasks.refs.handler_frame_static_thunk);
                let result_capacity = builder.ins().iconst(pointer_type, 16);
                builder.ins().call(
                    tasks.refs.handler_frame_set_resumption_payload_thunk,
                    &[frame, operation_value, thunk, result_capacity],
                );
                continue;
            }
            let compiled = compile_payload_constant(
                builder,
                value,
                handler.result_type,
                types,
                pointer_type,
                calls,
                environment,
                string_values,
            )?;
            let components = value_arguments(compiled);
            let component_types = abi_types(handler.result_type, pointer_type, types);
            if components.len() != component_types.len() {
                return Err(CodegenError::RuntimeError {
                    message: "resumable payload ABI shape mismatch".to_owned(),
                });
            }
            let component_count = component_types.len();
            let (payload_slot, payload_pointer) =
                create_task_slot(builder, pointer_type, components.len())?;
            for (index, (component, component_type)) in
                components.into_iter().zip(component_types).enumerate()
            {
                let component = payload_word(builder, component, component_type);
                builder.ins().stack_store(
                    pointer_type,
                    component,
                    payload_slot,
                    (index * 8) as i32,
                );
            }
            let size = builder
                .ins()
                .iconst(pointer_type, (component_count * 8) as i64);
            let managed_offsets = managed_pointer_offsets(handler.result_type, 0, types);
            if managed_offsets.is_empty() {
                let thunk = builder
                    .ins()
                    .func_addr(pointer_type, tasks.refs.handler_frame_static_thunk);
                let result_capacity = builder
                    .ins()
                    .iconst(pointer_type, (component_count * 8).max(1) as i64);
                builder.ins().call(
                    tasks.refs.handler_frame_set_resumption_thunk,
                    &[
                        frame,
                        operation_value,
                        thunk,
                        payload_pointer,
                        size,
                        result_capacity,
                    ],
                );
                continue;
            }
            let (offset_slot, offset_pointer) =
                create_task_slot(builder, pointer_type, managed_offsets.len())?;
            for (index, offset) in managed_offsets.iter().enumerate() {
                let value = builder.ins().iconst(pointer_type, *offset as i64);
                builder
                    .ins()
                    .stack_store(pointer_type, value, offset_slot, (index * 8) as i32);
            }
            let managed_count = builder
                .ins()
                .iconst(pointer_type, managed_offsets.len() as i64);
            builder.ins().call(
                tasks.refs.handler_frame_set_resumption_managed_payload,
                &[
                    frame,
                    operation_value,
                    payload_pointer,
                    size,
                    offset_pointer,
                    managed_count,
                ],
            );
            let result_capacity = builder
                .ins()
                .iconst(pointer_type, (component_count * 8).max(1) as i64);
            let thunk = builder
                .ins()
                .func_addr(pointer_type, tasks.refs.handler_frame_static_thunk);
            builder.ins().call(
                tasks.refs.handler_frame_set_resumption_thunk,
                &[
                    frame,
                    operation_value,
                    thunk,
                    payload_pointer,
                    size,
                    result_capacity,
                ],
            );
        } else if let Some(parameter) = handler.resumable_parameter {
            let operation_info = types
                .effects()
                .operation_info(handler.operation)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumable handler operation metadata is missing".to_owned(),
                })?;
            let offset_words = operation_info
                .parameters
                .iter()
                .take(parameter)
                .map(|ty| abi_types(*ty, pointer_type, types).len())
                .sum::<usize>();
            let length_words = operation_info
                .parameters
                .get(parameter)
                .map(|ty| abi_types(*ty, pointer_type, types).len())
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumable handler parameter index is invalid".to_owned(),
                })?;
            let packed = (u64::try_from(length_words.saturating_mul(8)).unwrap_or(u64::MAX) << 32)
                | u64::try_from(offset_words.saturating_mul(8)).unwrap_or(u64::MAX);
            let thunk = builder
                .ins()
                .func_addr(pointer_type, tasks.refs.handler_frame_forward_thunk);
            let context_slot = builder.ins().iconst(pointer_type, packed as i64);
            let (context_storage, context_pointer) = create_task_slot(builder, pointer_type, 1)?;
            builder
                .ins()
                .stack_store(pointer_type, context_slot, context_storage, 0);
            let result_capacity = builder
                .ins()
                .iconst(pointer_type, length_words.saturating_mul(8) as i64);
            let captures_size = builder.ins().iconst(pointer_type, 8);
            builder.ins().call(
                tasks.refs.handler_frame_set_resumption_thunk,
                &[
                    frame,
                    operation_value,
                    thunk,
                    context_pointer,
                    captures_size,
                    result_capacity,
                ],
            );
        } else if let Some(transform) = handler.resumable_transform {
            let operation_info = types
                .effects()
                .operation_info(handler.operation)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumable handler operation metadata is missing".to_owned(),
                })?;
            let parameter_type = operation_info
                .parameters
                .get(transform.parameter)
                .copied()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumable handler transform parameter index is invalid".to_owned(),
                })?;
            if parameter_type != handler.result_type
                || !parameter_type.is_integer()
                || abi_types(parameter_type, pointer_type, types).len() != 1
            {
                return Err(CodegenError::RuntimeError {
                    message: "resumable handler transform requires one integer word".to_owned(),
                });
            }
            let offset_words = operation_info
                .parameters
                .iter()
                .take(transform.parameter)
                .map(|ty| abi_types(*ty, pointer_type, types).len())
                .sum::<usize>();
            let offset_bytes = offset_words.saturating_mul(8);
            if offset_bytes > u16::MAX as usize {
                return Err(CodegenError::RuntimeError {
                    message: "resumable handler transform payload offset is too large".to_owned(),
                });
            }
            let operator = match transform.operator {
                crate::syntax::BinaryOp::Add => 0_u64,
                crate::syntax::BinaryOp::Subtract => 1_u64,
                crate::syntax::BinaryOp::Multiply => 2_u64,
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "unsupported resumable handler transform operator".to_owned(),
                    })
                }
            };
            let immediate = transform.immediate as u32 as u64;
            let packed = (immediate << 32)
                | (operator << 16)
                | u64::try_from(offset_bytes).unwrap_or(u64::MAX);
            let thunk = builder
                .ins()
                .func_addr(pointer_type, tasks.refs.handler_frame_transform_i64_thunk);
            let context_slot = builder.ins().iconst(pointer_type, packed as i64);
            let (context_storage, context_pointer) = create_task_slot(builder, pointer_type, 1)?;
            builder
                .ins()
                .stack_store(pointer_type, context_slot, context_storage, 0);
            let result_capacity = builder.ins().iconst(pointer_type, 8);
            let captures_size = builder.ins().iconst(pointer_type, 8);
            builder.ins().call(
                tasks.refs.handler_frame_set_resumption_thunk,
                &[
                    frame,
                    operation_value,
                    thunk,
                    context_pointer,
                    captures_size,
                    result_capacity,
                ],
            );
        } else if let Some(function) = handler.resumable_function {
            let canonical_captures = handler.resumable_captures.iter().all(|capture| {
                values
                    .get(capture)
                    .cloned()
                    .map(value_type)
                    .is_some_and(|ty| {
                        matches!(
                            ty,
                            Type::I32 | Type::I64 | Type::F64 | Type::Bool | Type::Class(_)
                        ) || types.is_shared(ty)
                            || types.is_owned(ty)
                    })
            });
            if canonical_captures {
                let handler_type = tasks.function_types.get(&function).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "synthetic handler function type was not compiled".to_owned(),
                    }
                })?;
                let operation_arity = types
                    .effects()
                    .operation_info(handler.operation)
                    .map(|info| info.parameters.len())
                    .unwrap_or_default();
                if handler_type.parameters.len()
                    != handler.resumable_captures.len() + operation_arity
                    || handler_type.parameters[operation_arity..]
                        .iter()
                        .zip(
                            handler
                                .resumable_captures
                                .iter()
                                .map(|capture| values.get(capture).cloned().map(value_type)),
                        )
                        .any(|(expected, actual)| Some(*expected) != actual)
                {
                    return Err(CodegenError::RuntimeError {
                        message: "synthetic handler capture ABI does not match its function"
                            .to_owned(),
                    });
                }
                let thunk = tasks
                    .handler_thunks
                    .get(&function)
                    .copied()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "capture-free synthetic handler thunk was not compiled".to_owned(),
                    })?;
                let thunk = builder.ins().func_addr(pointer_type, thunk);
                // Unit has no payload words, but the runtime still
                // needs a non-null result allocation for the
                // canonical thunk call record. The thunk reports a
                // zero-byte result for Unit.
                let result_capacity = abi_types(handler.result_type, pointer_type, types)
                    .len()
                    .saturating_mul(8)
                    .max(8);
                let result_capacity = builder.ins().iconst(pointer_type, result_capacity as i64);
                let mut capture_words = Vec::new();
                let mut capture_types = Vec::new();
                let mut capture_slots = Vec::new();
                let mut capture_offset = 0_usize;
                for capture in &handler.resumable_captures {
                    let mut value =
                        values
                            .get(capture)
                            .cloned()
                            .ok_or_else(|| CodegenError::RuntimeError {
                                message: "synthetic handler capture was not compiled".to_owned(),
                            })?;
                    let capture_type = value_type(value.clone());
                    let value_ownership = mir_function
                        .value_ownership
                        .get(capture.0)
                        .copied()
                        .unwrap_or(crate::mir::MirOwnership::Copy);
                    let environment_ownership = if types.is_shared(capture_type) {
                        value = duplicate_shared_value(builder, value, calls.dup)?;
                        Some(joky_runtime_abi::HandlerEnvOwnership::Shared)
                    } else if types.is_owned(capture_type) {
                        match value_ownership {
                            crate::mir::MirOwnership::Owned => {
                                Some(joky_runtime_abi::HandlerEnvOwnership::Owned)
                            }
                            crate::mir::MirOwnership::Borrowed => {
                                Some(joky_runtime_abi::HandlerEnvOwnership::Borrowed)
                            }
                            _ => {
                                return Err(CodegenError::RuntimeError {
                                    message: "owned handler capture has invalid ownership"
                                        .to_owned(),
                                })
                            }
                        }
                    } else {
                        None
                    };
                    let components = value_arguments(value);
                    let component_types = abi_types(capture_type, pointer_type, types);
                    if components.len() != component_types.len() {
                        return Err(CodegenError::RuntimeError {
                            message: "canonical handler capture ABI shape mismatch".to_owned(),
                        });
                    }
                    for (component, component_type) in components.into_iter().zip(component_types) {
                        capture_words.push(payload_word(builder, component, component_type));
                    }
                    let Some(ownership) = environment_ownership else {
                        capture_types.push(capture_type);
                        capture_offset += abi_types(capture_type, pointer_type, types).len() * 8;
                        continue;
                    };
                    for (offset, leaf_type) in
                        managed_pointer_types(capture_type, capture_offset, types)
                    {
                        capture_slots.push((offset, ownership, leaf_type));
                    }
                    capture_types.push(capture_type);
                    capture_offset += abi_types(capture_type, pointer_type, types).len() * 8;
                }
                let (capture_slot, captures) =
                    create_task_slot(builder, pointer_type, capture_words.len())?;
                for (index, word) in capture_words.into_iter().enumerate() {
                    builder
                        .ins()
                        .stack_store(pointer_type, word, capture_slot, (index * 8) as i32);
                }
                let captures_size = builder.ins().iconst(pointer_type, capture_offset as i64);
                let (environment_slots, environment_slot_count) = if capture_slots.is_empty() {
                    (
                        builder.ins().iconst(pointer_type, 0),
                        builder.ins().iconst(pointer_type, 0),
                    )
                } else {
                    let (slot, pointer) = create_task_slot(
                        builder,
                        pointer_type,
                        capture_slots.len()
                            * (joky_runtime_abi::HANDLER_ENV_SLOT_SIZE as usize / 8),
                    )?;
                    let null = builder.ins().iconst(pointer_type, 0);
                    let mut descriptor_index = 0;
                    for (capture_offset, ownership_kind, leaf_type) in capture_slots {
                        let descriptor_offset =
                            descriptor_index * joky_runtime_abi::HANDLER_ENV_SLOT_SIZE;
                        let capture_offset =
                            builder.ins().iconst(pointer_type, capture_offset as i64);
                        let ownership = builder
                            .ins()
                            .iconst(cranelift_codegen::ir::types::I8, ownership_kind as i64);
                        let drop_callback = if matches!(
                            ownership_kind,
                            joky_runtime_abi::HandlerEnvOwnership::Owned
                                | joky_runtime_abi::HandlerEnvOwnership::Shared
                        ) {
                            match leaf_type {
                                Type::Class(id) => {
                                    let drop =
                                        environment.class_drop_refs.get(&id).ok_or_else(|| {
                                            CodegenError::RuntimeError {
                                                message: format!(
                                                    "class {id} has no handler capture drop glue"
                                                ),
                                            }
                                        })?;
                                    builder.ins().func_addr(pointer_type, *drop)
                                }
                                _ => builder.ins().func_addr(pointer_type, calls.managed_drop),
                            }
                        } else {
                            null
                        };
                        let offset_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset + joky_runtime_abi::HANDLER_ENV_SLOT_OFFSET_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            capture_offset,
                            offset_address,
                            0,
                        );
                        let ownership_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset + joky_runtime_abi::HANDLER_ENV_SLOT_OWNERSHIP_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ownership,
                            ownership_address,
                            0,
                        );
                        let drop_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset
                                + joky_runtime_abi::HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            drop_callback,
                            drop_address,
                            0,
                        );
                        descriptor_index += 1;
                    }
                    (
                        pointer,
                        builder.ins().iconst(pointer_type, descriptor_index as i64),
                    )
                };
                let result_managed = managed_pointer_types(handler.result_type, 0, types);
                let (result_slots, result_slot_count) = if result_managed.is_empty() {
                    (
                        builder.ins().iconst(pointer_type, 0),
                        builder.ins().iconst(pointer_type, 0),
                    )
                } else {
                    let (slot, pointer) = create_task_slot(
                        builder,
                        pointer_type,
                        result_managed.len()
                            * (joky_runtime_abi::HANDLER_ENV_SLOT_SIZE as usize / 8),
                    )?;
                    let mut descriptor_index = 0;
                    for (result_offset, leaf_type) in result_managed {
                        let descriptor_offset =
                            descriptor_index * joky_runtime_abi::HANDLER_ENV_SLOT_SIZE;
                        let result_offset =
                            builder.ins().iconst(pointer_type, result_offset as i64);
                        let ownership = builder.ins().iconst(
                            cranelift_codegen::ir::types::I8,
                            joky_runtime_abi::HandlerEnvOwnership::Owned as i64,
                        );
                        let drop_callback = match leaf_type {
                            Type::Class(id) => {
                                let drop =
                                    environment.class_drop_refs.get(&id).ok_or_else(|| {
                                        CodegenError::RuntimeError {
                                            message: format!(
                                                "class {id} has no handler result drop glue"
                                            ),
                                        }
                                    })?;
                                builder.ins().func_addr(pointer_type, *drop)
                            }
                            _ => builder.ins().func_addr(pointer_type, calls.managed_drop),
                        };
                        let offset_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset + joky_runtime_abi::HANDLER_ENV_SLOT_OFFSET_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            result_offset,
                            offset_address,
                            0,
                        );
                        let ownership_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset + joky_runtime_abi::HANDLER_ENV_SLOT_OWNERSHIP_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ownership,
                            ownership_address,
                            0,
                        );
                        let drop_address = builder.ins().stack_addr(
                            pointer_type,
                            slot,
                            descriptor_offset
                                + joky_runtime_abi::HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET,
                        );
                        builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            drop_callback,
                            drop_address,
                            0,
                        );
                        descriptor_index += 1;
                    }
                    (
                        pointer,
                        builder.ins().iconst(pointer_type, descriptor_index as i64),
                    )
                };
                builder.ins().call(
                    tasks
                        .refs
                        .handler_frame_set_resumption_thunk_with_result_env,
                    &[
                        frame,
                        operation_value,
                        thunk,
                        captures,
                        captures_size,
                        result_capacity,
                        environment_slots,
                        environment_slot_count,
                        result_slots,
                        result_slot_count,
                    ],
                );
                continue;
            }
            return Err(CodegenError::RuntimeError {
                message: "synthetic resumable handler must use the canonical thunk ABI".to_owned(),
            });
        }
    }
    Ok(frame)
}
