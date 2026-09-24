use super::*;
use crate::codegen::environment::Environment;
use crate::codegen::functions::ordinary_continuations::{
    initialize_continuations, ContinuationAllocationRefs,
};
use crate::codegen::functions::pending::PendingRefs;
use crate::mir::{MirContinuationId, MirValueId};

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_resumed_call(
    builder: &mut FunctionBuilder<'_>,
    function: &MirFunction,
    continuation: MirContinuationId,
    callee: Option<MirFunctionId>,
    indirect_callee: Option<CompiledValue>,
    receiver: Option<MirValueId>,
    arguments: &[crate::mir::MirCallArgument],
    destination: MirValueId,
    current: cranelift_codegen::ir::Value,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
    keys: &HashMap<MirContinuationId, usize>,
    allocation: ContinuationAllocationRefs,
    pending: PendingRefs,
    entry_refs: super::super::refs::ContinuationEntryRefs,
    cleanup: &HashMap<Type, cranelift_codegen::ir::FuncRef>,
    types: &TypeTable,
    pointer_type: cranelift_codegen::ir::Type,
) -> Result<(), CodegenError> {
    let metadata = &function.continuations[continuation.0];
    let resources = initialize_continuations(
        builder,
        function,
        keys,
        allocation,
        &environment.class_drop_refs,
        cleanup,
        types,
        pointer_type,
        |item| item.id == continuation,
    )?;
    let handle = resources.handles[&continuation];
    if let Some(frame) = resources.frames.get(&continuation) {
        let layout = crate::codegen::functions::task_frame::TaskFrameLayout::new(
            function,
            pointer_type,
            types,
        );
        for scope in &layout.scopes {
            let group = environment
                .task_group(*scope)
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            builder.ins().store(
                MemFlagsData::new(),
                group,
                *frame,
                layout.scope_offset(*scope).unwrap(),
            );
        }
        for task in &layout.tasks {
            let task_handle = environment
                .task_handle(*task)
                .unwrap_or_else(|| builder.ins().iconst(pointer_type, 0));
            builder.ins().store(
                MemFlagsData::new(),
                task_handle,
                *frame,
                layout.task_offset(*task).unwrap(),
            );
        }
        let local_layout = crate::codegen::functions::task_frame::LocalFrameLayout::new(
            function,
            pointer_type,
            types,
        );
        for slot in &metadata.frame_slots {
            let mut offset = local_layout.offset(slot.local) as i32;
            let value = slot
                .value
                .and_then(|id| values.get(&id).cloned())
                .or_else(|| environment.lookup_local(slot.local))
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!("resumed call frame local {} is missing", slot.local.0),
                })?;
            for component in value_arguments(value) {
                builder
                    .ins()
                    .store(MemFlagsData::new(), component, *frame, offset);
                offset += 8;
            }
        }
    }
    if let Some(spill) = resources.spills.get(&continuation) {
        crate::codegen::functions::spills::SpillLayout::new(function, pointer_type, types)
            .save(builder, metadata, *spill, values)?;
    }
    let pc = builder
        .ins()
        .iconst(pointer_type, metadata.resume_block.0 as i64);
    builder
        .ins()
        .call(entry_refs.set_program_counter, &[handle, pc]);
    let ty = function.value_types[destination.0];
    let size = abi_types(ty, pointer_type, types).len() * 8;
    let size_value = builder.ins().iconst(pointer_type, size as i64);
    if size != 0 {
        builder
            .ins()
            .call(entry_refs.alloc_suspend_result, &[handle, size_value]);
    }
    register_continuation_cleanups(
        builder,
        handle,
        CONTINUATION_SUSPEND_RESULT_STORAGE,
        ty,
        0,
        pointer_type,
        types,
        allocation.register_cleanup,
        allocation.register_cleanup_region,
        allocation.managed_drop,
        &environment.class_drop_refs,
        cleanup,
    )?;
    let parent = builder.ins().call(pending.parent, &[current]);
    let parent = builder.inst_results(parent)[0];
    let begun = builder.ins().call(pending.begin, &[handle, parent]);
    let begun = builder.inst_results(begun)[0];
    let success = builder.ins().iconst(cranelift_codegen::ir::types::I8, 0);
    let cancelled = builder.ins().iconst(cranelift_codegen::ir::types::I8, 3);
    let status = builder.ins().select(begun, success, cancelled);
    fail_if_nonzero(builder, status, handle, current, pending);
    let receiver = receiver
        .map(|id| {
            values
                .get(&id)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "resumed method receiver is missing".to_owned(),
                })
        })
        .transpose()?;
    let (status, value) = if let Some(callee) = indirect_callee {
        compile_mir_indirect_call_abi(
            builder,
            callee,
            arguments,
            values,
            pointer_type,
            types,
            Some(handle),
        )?
    } else {
        let callee = callee.expect("resumed direct call is missing callee");
        compile_mir_call_abi(
            builder,
            &callee,
            arguments,
            values,
            environment,
            types,
            Some(handle),
            receiver,
        )?
    };
    let status = status.unwrap();
    let terminal = builder.ins().icmp_imm_u(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
        status,
        1,
    );
    let status_or_zero = builder.ins().select(terminal, status, success);
    fail_if_nonzero(builder, status_or_zero, handle, current, pending);
    let ready = builder.create_block();
    let poll = builder.create_block();
    builder.ins().brif(status, poll, &[], ready, &[]);
    builder.switch_to_block(ready);
    let components = value_arguments(value.clone());
    let (_, payload) = create_task_slot(builder, pointer_type, components.len())?;
    for (index, component) in components.into_iter().enumerate() {
        builder
            .ins()
            .store(MemFlagsData::new(), component, payload, (index * 8) as i32);
    }
    let accepted = builder
        .ins()
        .call(pending.complete, &[handle, payload, size_value]);
    let accepted = builder.inst_results(accepted)[0];
    let rejected = builder.create_block();
    builder.ins().brif(accepted, poll, &[], rejected, &[]);
    builder.switch_to_block(rejected);
    compile_drop_value(builder, value, environment, pointer_type, types)?;
    builder.ins().call(pending.retire, &[current, handle]);
    builder.ins().call(pending.fail_chain, &[handle, cancelled]);
    builder.ins().return_(&[]);
    builder.switch_to_block(poll);
    builder.seal_block(ready);
    builder.seal_block(rejected);
    builder.seal_block(poll);
    // Reenter through the saved continuation so loop backedges reload both
    // the result and frame instead of reusing this entry's restored SSA values.
    let polled = builder.ins().call(pending.poll_resume, &[handle]);
    let status = builder.inst_results(polled)[0];
    let ready = builder.create_block();
    let suspended = builder.create_block();
    builder.ins().brif(status, suspended, &[], ready, &[]);
    builder.switch_to_block(suspended);
    let terminal = builder.ins().icmp_imm_u(
        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
        status,
        1,
    );
    let status_or_zero = builder.ins().select(terminal, status, success);
    fail_if_nonzero(builder, status_or_zero, handle, current, pending);
    builder.ins().call(pending.retire, &[current, handle]);
    builder.ins().return_(&[]);
    builder.switch_to_block(ready);
    builder.seal_block(suspended);
    builder.seal_block(ready);
    let (status, value) = crate::codegen::functions::pending::take_result(
        builder,
        pending.take,
        handle,
        ty,
        pointer_type,
        types,
    )?;
    fail_if_nonzero(builder, status, handle, current, pending);
    builder.ins().call(pending.discard_ready, &[handle]);
    values.insert(destination, value);
    Ok(())
}

fn fail_if_nonzero(
    builder: &mut FunctionBuilder<'_>,
    status: cranelift_codegen::ir::Value,
    handle: cranelift_codegen::ir::Value,
    current: cranelift_codegen::ir::Value,
    pending: PendingRefs,
) {
    let failed = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(status, failed, &[], ready, &[]);
    builder.switch_to_block(failed);
    builder.ins().call(pending.retire, &[current, handle]);
    builder.ins().call(pending.fail_chain, &[handle, status]);
    builder.ins().return_(&[]);
    builder.switch_to_block(ready);
    builder.seal_block(failed);
    builder.seal_block(ready);
}
