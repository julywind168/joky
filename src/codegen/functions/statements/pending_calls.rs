use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "Direct Pending calls carry MIR call operands alongside mutable SSA and ownership environments."
)]
pub(super) fn compile_pending_call(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    destination: MirValueId,
    callee: crate::mir::MirFunctionId,
    receiver: Option<MirValueId>,
    arguments: &[crate::mir::MirCallArgument],
    continuation: crate::mir::MirContinuationId,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    if !context.pending_abi {
        return Err(CodegenError::RuntimeError {
            message: "call continuation requires a Pending caller".to_owned(),
        });
    }
    let pointer_type = context.pointer_type;
    let resources = super::super::ordinary_continuations::initialize_continuations(
        builder,
        context.function,
        context.continuation_entry_keys,
        context.continuation_allocation_refs,
        &environment.class_drop_refs,
        context.continuation_cleanup_drop_refs,
        context.types,
        pointer_type,
        |metadata| metadata.id == continuation,
    )?;
    let handle = resources.handles[&continuation];
    let slot = context.call_handle_slots[&continuation];
    builder.ins().stack_store(pointer_type, handle, slot, 0);
    continuations::save_continuation_frame(
        builder,
        context,
        &continuation,
        values,
        environment,
        handle,
        resources.frames.get(&continuation).copied(),
        resources.spills.get(&continuation).copied(),
    )?;
    let ty = context.function.value_types[destination.0];
    let size = abi_types(ty, pointer_type, context.types).len() * 8;
    if size != 0 {
        let size = builder.ins().iconst(pointer_type, size as i64);
        builder.ins().call(
            context.refs.continuation_alloc_suspend_result,
            &[handle, size],
        );
    }
    register_continuation_cleanups(
        builder,
        handle,
        CONTINUATION_SUSPEND_RESULT_STORAGE,
        ty,
        0,
        pointer_type,
        context.types,
        context.refs.continuation_register_cleanup,
        context.refs.continuation_register_cleanup_region,
        context.refs.calls.managed_drop,
        &environment.class_drop_refs,
        context.continuation_cleanup_drop_refs,
    )?;
    let parent = context
        .pending_parent
        .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
    let begun = builder
        .ins()
        .call(context.pending_refs.begin, &[handle, parent]);
    // A rejected activation must never invoke a callee with dead frame storage.
    let begun = builder.inst_results(begun)[0];
    let cancelled = builder.ins().iconst(types::I8, 3);
    let success = builder.ins().iconst(types::I8, 0);
    let status = builder.ins().select(begun, success, cancelled);
    super::super::pending::return_status_if_error(builder, status, |builder| {
        free_native_reservations(builder, context)
    });
    let receiver = receiver
        .map(|value| {
            values
                .get(&value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Pending method receiver was not compiled".to_owned(),
                })
        })
        .transpose()?;
    let (status, value) = compile_mir_call_abi(
        builder,
        &callee,
        arguments,
        values,
        environment,
        context.types,
        Some(handle),
        receiver,
    )?;
    let status = status.expect("Pending signature returns a status");
    let failed = builder.ins().icmp_imm_u(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
        status,
        1,
    );
    let failed_block = builder.create_block();
    let valid = builder.create_block();
    builder.ins().brif(failed, failed_block, &[], valid, &[]);
    builder.switch_to_block(failed_block);
    builder.ins().call(context.pending_refs.cancel, &[handle]);
    free_native_reservations(builder, context);
    super::super::pending::return_status(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(failed_block);
    builder.seal_block(valid);
    let ready = builder.create_block();
    let after = builder.create_block();
    builder.ins().brif(status, after, &[], ready, &[]);
    builder.switch_to_block(ready);
    let components = value_arguments(value.clone());
    let (_, payload) = create_task_slot(builder, pointer_type, components.len())?;
    for (index, value) in components.iter().enumerate() {
        builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            *value,
            payload,
            (index * 8) as i32,
        );
    }
    let size = builder.ins().iconst(pointer_type, size as i64);
    let accepted = builder.ins().call(
        context.refs.continuation_complete_function_pending,
        &[handle, payload, size],
    );
    let accepted = builder.inst_results(accepted)[0];
    let rejected = builder.create_block();
    let accepted_block = builder.create_block();
    builder
        .ins()
        .brif(accepted, accepted_block, &[], rejected, &[]);
    builder.switch_to_block(rejected);
    compile_drop_value(builder, value, environment, pointer_type, context.types)?;
    builder.ins().call(context.pending_refs.cancel, &[handle]);
    let cancelled = builder.ins().iconst(types::I8, 3);
    free_native_reservations(builder, context);
    super::super::pending::return_status(builder, cancelled);
    builder.switch_to_block(accepted_block);
    builder.seal_block(rejected);
    builder.seal_block(accepted_block);
    builder.ins().jump(after, &[]);
    builder.switch_to_block(after);
    builder.seal_block(ready);
    builder.seal_block(after);
    let polled = builder.ins().call(context.pending_refs.poll, &[handle]);
    context.suspend_pending = Some(builder.inst_results(polled)[0]);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Indirect Pending calls carry the closure operands and continuation alongside mutable codegen state."
)]
pub(super) fn compile_pending_indirect_call(
    builder: &mut FunctionBuilder<'_>,
    context: &mut StatementContext<'_>,
    destination: MirValueId,
    callee: CompiledValue,
    arguments: &[crate::mir::MirCallArgument],
    continuation: crate::mir::MirContinuationId,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
) -> Result<(), CodegenError> {
    let pointer_type = context.pointer_type;
    let resources = super::super::ordinary_continuations::initialize_continuations(
        builder,
        context.function,
        context.continuation_entry_keys,
        context.continuation_allocation_refs,
        &environment.class_drop_refs,
        context.continuation_cleanup_drop_refs,
        context.types,
        pointer_type,
        |metadata| metadata.id == continuation,
    )?;
    let handle = resources.handles[&continuation];
    let slot = context.call_handle_slots[&continuation];
    builder.ins().stack_store(pointer_type, handle, slot, 0);
    continuations::save_continuation_frame(
        builder,
        context,
        &continuation,
        values,
        environment,
        handle,
        resources.frames.get(&continuation).copied(),
        resources.spills.get(&continuation).copied(),
    )?;
    let ty = context.function.value_types[destination.0];
    let size_bytes = abi_types(ty, pointer_type, context.types).len() * 8;
    if size_bytes != 0 {
        let size = builder.ins().iconst(pointer_type, size_bytes as i64);
        builder.ins().call(
            context.refs.continuation_alloc_suspend_result,
            &[handle, size],
        );
    }
    register_continuation_cleanups(
        builder,
        handle,
        CONTINUATION_SUSPEND_RESULT_STORAGE,
        ty,
        0,
        pointer_type,
        context.types,
        context.refs.continuation_register_cleanup,
        context.refs.continuation_register_cleanup_region,
        context.refs.calls.managed_drop,
        &environment.class_drop_refs,
        context.continuation_cleanup_drop_refs,
    )?;
    let parent = context
        .pending_parent
        .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
    let begun = builder
        .ins()
        .call(context.pending_refs.begin, &[handle, parent]);
    let begun = builder.inst_results(begun)[0];
    let cancelled = builder.ins().iconst(types::I8, 3);
    let success = builder.ins().iconst(types::I8, 0);
    let status = builder.ins().select(begun, success, cancelled);
    super::super::pending::return_status_if_error(builder, status, |builder| {
        free_native_reservations(builder, context)
    });
    let (status, value) = super::super::values::compile_mir_indirect_call_abi(
        builder,
        callee,
        arguments,
        values,
        pointer_type,
        context.types,
        Some(handle),
    )?;
    let status = status.expect("indirect Pending signature returns a status");
    let failed = builder.ins().icmp_imm_u(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
        status,
        1,
    );
    let failed_block = builder.create_block();
    let valid = builder.create_block();
    builder.ins().brif(failed, failed_block, &[], valid, &[]);
    builder.switch_to_block(failed_block);
    builder.ins().call(context.pending_refs.cancel, &[handle]);
    free_native_reservations(builder, context);
    super::super::pending::return_status(builder, status);
    builder.switch_to_block(valid);
    builder.seal_block(failed_block);
    builder.seal_block(valid);
    let ready = builder.create_block();
    let after = builder.create_block();
    builder.ins().brif(status, after, &[], ready, &[]);
    builder.switch_to_block(ready);
    let components = value_arguments(value.clone());
    let (_, payload) = create_task_slot(builder, pointer_type, components.len())?;
    for (index, component) in components.iter().enumerate() {
        builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            *component,
            payload,
            (index * 8) as i32,
        );
    }
    let size = builder.ins().iconst(pointer_type, size_bytes as i64);
    let accepted = builder.ins().call(
        context.refs.continuation_complete_function_pending,
        &[handle, payload, size],
    );
    let accepted = builder.inst_results(accepted)[0];
    let rejected = builder.create_block();
    let accepted_block = builder.create_block();
    builder
        .ins()
        .brif(accepted, accepted_block, &[], rejected, &[]);
    builder.switch_to_block(rejected);
    compile_drop_value(builder, value, environment, pointer_type, context.types)?;
    builder.ins().call(context.pending_refs.cancel, &[handle]);
    free_native_reservations(builder, context);
    super::super::pending::return_status(builder, cancelled);
    builder.switch_to_block(accepted_block);
    builder.seal_block(rejected);
    builder.seal_block(accepted_block);
    builder.ins().jump(after, &[]);
    builder.switch_to_block(after);
    builder.seal_block(ready);
    builder.seal_block(after);
    let polled = builder.ins().call(context.pending_refs.poll, &[handle]);
    context.suspend_pending = Some(builder.inst_results(polled)[0]);
    Ok(())
}

/// Native error returns have no machine entry to retire these reservations.
/// Dynamic call slots may contain already-retired tokens; free is idempotent.
pub(in crate::codegen::functions) fn free_native_reservations(
    builder: &mut FunctionBuilder<'_>,
    context: &StatementContext<'_>,
) {
    for handle in context.continuation_handles.values() {
        builder.ins().call(context.pending_refs.free, &[*handle]);
    }
    for slot in context.call_handle_slots.values() {
        let handle = builder
            .ins()
            .stack_load(context.pointer_type, context.pointer_type, *slot, 0);
        builder.ins().call(context.pending_refs.free, &[handle]);
    }
}
