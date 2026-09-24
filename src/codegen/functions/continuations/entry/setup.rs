use std::collections::HashMap;

use cranelift_codegen::ir::{Block, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;

use crate::codegen::abi::{
    abi_types, append_result_params, managed_pointer_offsets, value_from_params,
};
use crate::codegen::environment::{CompiledValue, Environment, FunctionType};
use crate::codegen::functions::continuation::continuation_machine_blocks;
use crate::codegen::functions::continuations::refs::{
    ContinuationCodegenRefs, ContinuationEntryRefs,
};
use crate::diagnostic::CodegenError;
use crate::mir::{MirContinuation, MirFunction, MirFunctionId, MirStatement, MirValueId};
use crate::sema::TypeTable;

pub(super) struct ContinuationEntrySetup {
    pub(super) entry: Block,
    pub(super) resume_blocks: Vec<crate::mir::MirBlockId>,
    pub(super) blocks: Vec<Option<Block>>,
    pub(super) phi_values: Vec<HashMap<MirValueId, CompiledValue>>,
    pub(super) continuation_handle: Value,
    pub(super) frame: Value,
    pub(super) spill: Value,
    pub(super) values: HashMap<MirValueId, CompiledValue>,
    pub(super) environments: Vec<Option<Environment>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "Entry setup needs the MIR frame, callee signatures and declared runtime references together."
)]
pub(super) fn setup_continuation_entry(
    builder: &mut FunctionBuilder<'_>,
    function: &MirFunction,
    continuation: &MirContinuation,
    function_types: &HashMap<MirFunctionId, FunctionType>,
    support_refs: &ContinuationCodegenRefs,
    entry_refs: ContinuationEntryRefs,
    types: &TypeTable,
    pointer_type: cranelift_codegen::ir::Type,
    take_function_result: cranelift_codegen::ir::FuncRef,
) -> Result<ContinuationEntrySetup, CodegenError> {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    let resume_blocks = continuation_machine_blocks(function, continuation.resume_block);
    if resume_blocks.is_empty() {
        return Err(CodegenError::RuntimeError {
            message: "continuation resume block is out of bounds".to_owned(),
        });
    }
    let mut blocks = vec![None; function.blocks.len()];
    for block_id in &resume_blocks {
        blocks[block_id.0] = Some(builder.create_block());
    }
    let mut phi_values = vec![HashMap::new(); function.blocks.len()];
    for block_id in &resume_blocks {
        let source = &function.blocks[block_id.0];
        let block = blocks[block_id.0].expect("created continuation CFG block");
        for statement in &source.statements {
            let MirStatement::Phi { destination, .. } = &statement else {
                break;
            };
            let ty = function.value_types[destination.0];
            let params = append_result_params(&mut *builder, block, ty, pointer_type, types);
            let value = value_from_params(&params, ty, types).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            phi_values[block_id.0].insert(*destination, value);
        }
    }

    builder.switch_to_block(entry);
    let continuation_handle = builder.block_params(entry)[0];
    let root_group_call = builder
        .ins()
        .call(entry_refs.root_group, &[continuation_handle]);
    let root_group = builder.inst_results(root_group_call)[0];
    let frame_call = builder
        .ins()
        .call(entry_refs.frame_pointer, &[continuation_handle]);
    let frame = builder.inst_results(frame_call)[0];
    let spill_call = builder
        .ins()
        .call(entry_refs.spill_pointer, &[continuation_handle]);
    let spill = builder.inst_results(spill_call)[0];
    let mut values = HashMap::new();
    let mut resume_environment = Environment::new();
    resume_environment.function_refs = support_refs.function_refs.clone();
    resume_environment.closure_call_refs = support_refs.closure_call_refs.clone();
    resume_environment.closure_drop_refs = support_refs.closure_drop_refs.clone();
    resume_environment.callback_trampoline_refs = support_refs.callback_trampoline_refs.clone();
    resume_environment.function_types = function_types.clone();
    resume_environment.class_drop_refs = support_refs.class_drop_refs.clone();
    resume_environment.managed_drop_ref = Some(entry_refs.calls.managed_drop);
    resume_environment.allocate_ref = Some(entry_refs.calls.allocate);
    resume_environment.closure_allocate_ref = Some(entry_refs.calls.closure_allocate);
    resume_environment.bind_task_group(crate::mir::MirScopeId(0), root_group);
    let task_layout =
        crate::codegen::functions::task_frame::TaskFrameLayout::new(function, pointer_type, types);
    for scope in &task_layout.scopes {
        let group = builder.ins().load(
            pointer_type,
            MemFlagsData::new(),
            frame,
            task_layout.scope_offset(*scope).unwrap(),
        );
        resume_environment.bind_task_group(*scope, group);
    }
    for task in &task_layout.tasks {
        let id = builder.ins().load(
            pointer_type,
            MemFlagsData::new(),
            frame,
            task_layout.task_offset(*task).unwrap(),
        );
        resume_environment.bind_task_handle(*task, id);
    }

    restore_suspend_result(
        builder,
        function,
        continuation,
        &mut values,
        continuation_handle,
        entry_refs.suspend_result_pointer,
        entry_refs.release_suspend_result,
        pointer_type,
        types,
        take_function_result,
        entry_refs.complete,
    )?;
    restore_frame_slots(
        builder,
        function,
        continuation,
        &mut values,
        &mut resume_environment,
        frame,
        pointer_type,
        types,
    )?;
    crate::codegen::functions::spills::SpillLayout::new(function, pointer_type, types).restore(
        builder,
        function,
        continuation,
        spill,
        &mut values,
        pointer_type,
        types,
    )?;

    let resume_block =
        blocks[continuation.resume_block.0].expect("checked continuation resume block");
    builder.ins().jump(resume_block, &[]);

    let mut environments = vec![None; function.blocks.len()];
    environments[continuation.resume_block.0] = Some(resume_environment);
    Ok(ContinuationEntrySetup {
        entry,
        resume_blocks,
        blocks,
        phi_values,
        continuation_handle,
        frame,
        spill,
        values,
        environments,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Frame restoration updates both SSA values and local bindings using the function's typed layout."
)]
fn restore_frame_slots(
    builder: &mut FunctionBuilder<'_>,
    function: &MirFunction,
    continuation: &MirContinuation,
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &mut Environment,
    frame: Value,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(), CodegenError> {
    let layout =
        crate::codegen::functions::task_frame::LocalFrameLayout::new(function, pointer_type, types);
    for slot in &continuation.frame_slots {
        let mut frame_offset = layout.offset(slot.local);
        let slot_base = frame_offset;
        let components = abi_types(slot.ty, pointer_type, types)
            .into_iter()
            .map(|ty| {
                let value = builder
                    .ins()
                    .load(ty, MemFlagsData::new(), frame, frame_offset as i32);
                frame_offset += 8;
                value
            })
            .collect::<Vec<_>>();
        let value = value_from_params(&components, slot.ty, types).map_err(|error| {
            CodegenError::RuntimeError {
                message: error.to_string(),
            }
        })?;
        // Parameters and receiver locals do not have a defining MIR SSA
        // value yet. They are still materialized in the frame at Suspend,
        // so restore them directly into the resume environment.
        if let Some(value_id) = slot.value {
            values.insert(value_id, value.clone());
        }
        // Ownership moves from the continuation frame into the resumed
        // environment. Clear managed words in the frame so its cleanup
        // descriptors cannot drop the same native handle a second time.
        for pointer_offset in managed_pointer_offsets(slot.ty, slot_base, types) {
            let zero = builder.ins().iconst(pointer_type, 0);
            builder
                .ins()
                .store(MemFlagsData::new(), zero, frame, pointer_offset as i32);
        }
        environment.bind_local(slot.local, value);
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Result restoration coordinates typed storage and distinct runtime transfer/completion callbacks."
)]
fn restore_suspend_result(
    builder: &mut FunctionBuilder<'_>,
    function: &MirFunction,
    continuation: &MirContinuation,
    values: &mut HashMap<MirValueId, CompiledValue>,
    continuation_handle: Value,
    suspend_result_pointer_ref: cranelift_codegen::ir::FuncRef,
    release_suspend_result_ref: cranelift_codegen::ir::FuncRef,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
    take_function_result: cranelift_codegen::ir::FuncRef,
    complete: cranelift_codegen::ir::FuncRef,
) -> Result<(), CodegenError> {
    if !continuation.kind.is_suspending() {
        return Ok(());
    }
    let destination =
        continuation
            .resume_destination
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "MIR suspend continuation has no result destination".to_owned(),
            })?;
    let result_type = function.value_types[destination.0];
    if continuation.is_function_call() {
        let (status, value) = crate::codegen::functions::pending::take_result(
            builder,
            take_function_result,
            continuation_handle,
            result_type,
            pointer_type,
            types,
        )?;
        let failed = builder.create_block();
        let ready = builder.create_block();
        builder.ins().brif(status, failed, &[], ready, &[]);
        builder.switch_to_block(failed);
        builder.ins().call(complete, &[continuation_handle]);
        builder.ins().return_(&[]);
        builder.switch_to_block(ready);
        builder.seal_block(failed);
        builder.seal_block(ready);
        values.insert(destination, value);
        // The claimed payload has moved into SSA values. A later operation
        // can reuse this activation with a different result layout.
        builder
            .ins()
            .call(release_suspend_result_ref, &[continuation_handle]);
        return Ok(());
    }
    let component_types = abi_types(result_type, pointer_type, types);
    if component_types.is_empty() {
        values.insert(destination, CompiledValue::Unit);
    } else {
        let result_call = builder
            .ins()
            .call(suspend_result_pointer_ref, &[continuation_handle]);
        let result_pointer = builder.inst_results(result_call)[0];
        let mut offset = 0;
        let components = component_types
            .into_iter()
            .map(|component_type| {
                let value =
                    builder
                        .ins()
                        .load(component_type, MemFlagsData::new(), result_pointer, offset);
                offset += 8;
                value
            })
            .collect::<Vec<_>>();
        let value = value_from_params(&components, result_type, types).map_err(|error| {
            CodegenError::RuntimeError {
                message: format!("MIR suspend result restore failed: {error}"),
            }
        })?;
        for pointer_offset in managed_pointer_offsets(result_type, 0, types) {
            let zero = builder.ins().iconst(pointer_type, 0);
            builder.ins().store(
                MemFlagsData::new(),
                zero,
                result_pointer,
                pointer_offset as i32,
            );
        }
        values.insert(destination, value);
    }
    // The result now lives in the resumed SSA value. Clear managed
    // words before releasing the operation-owned buffer so completion
    // cleanup cannot observe a moved pointer.
    builder
        .ins()
        .call(release_suspend_result_ref, &[continuation_handle]);
    Ok(())
}
