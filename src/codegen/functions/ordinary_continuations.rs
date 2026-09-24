use std::collections::HashMap;

use cranelift_codegen::ir::{types, FuncRef, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;

use crate::codegen::abi::abi_types;
use crate::codegen::functions::continuation::{
    continuation_cleanup_callback, continuation_cleanup_components, continuation_cleanup_regions,
};
use crate::diagnostic::CodegenError;
use crate::mir::{MirContinuationId, MirFunction, MirOwnership};
use crate::sema::{Type, TypeTable};
use joky_runtime_abi::{
    CONTINUATION_FRAME_STORAGE, CONTINUATION_RESULT_STORAGE, CONTINUATION_SPILL_STORAGE,
};

pub(super) struct ContinuationResources {
    pub(super) handles: HashMap<MirContinuationId, Value>,
    pub(super) spills: HashMap<MirContinuationId, Value>,
    pub(super) frames: HashMap<MirContinuationId, Value>,
    pub(super) results: HashMap<MirContinuationId, Value>,
}

#[derive(Clone, Copy)]
pub(super) struct ContinuationAllocationRefs {
    pub(super) new: FuncRef,
    pub(super) set_resume_entry: FuncRef,
    pub(super) alloc_frame: FuncRef,
    pub(super) alloc_result: FuncRef,
    pub(super) alloc_spill: FuncRef,
    pub(super) register_cleanup: FuncRef,
    pub(super) register_cleanup_region: FuncRef,
    pub(super) group_cancel_free: FuncRef,
    pub(super) managed_drop: FuncRef,
}

impl ContinuationAllocationRefs {
    pub(super) fn declare(
        module: &mut impl cranelift_module::Module,
        function: &mut cranelift_codegen::ir::Function,
        managed_drop: FuncRef,
    ) -> Result<Self, CodegenError> {
        use joky_runtime_abi::symbols::*;
        let pointer = module.target_config().pointer_type();
        let mut import = |name,
                          params: Vec<cranelift_codegen::ir::Type>,
                          result: Option<cranelift_codegen::ir::Type>| {
            let mut signature = module.make_signature();
            signature.params = params
                .into_iter()
                .map(cranelift_codegen::ir::AbiParam::new)
                .collect();
            signature.returns = result
                .into_iter()
                .map(cranelift_codegen::ir::AbiParam::new)
                .collect();
            let id = module
                .declare_function(name, cranelift_module::Linkage::Import, &signature)
                .map_err(crate::codegen::helpers::codegen_error)?;
            Ok::<_, CodegenError>(module.declare_func_in_func(id, function))
        };
        Ok(Self {
            new: import(CONTINUATION_NEW_SYMBOL, vec![types::I64], Some(pointer))?,
            set_resume_entry: import(
                CONTINUATION_SET_RESUME_ENTRY_SYMBOL,
                vec![pointer, pointer],
                Some(types::I8),
            )?,
            alloc_frame: import(
                CONTINUATION_ALLOC_FRAME_SYMBOL,
                vec![pointer, pointer],
                Some(pointer),
            )?,
            alloc_result: import(
                CONTINUATION_ALLOC_RESULT_SYMBOL,
                vec![pointer, pointer],
                Some(pointer),
            )?,
            alloc_spill: import(
                CONTINUATION_ALLOC_SPILL_SYMBOL,
                vec![pointer, pointer],
                Some(pointer),
            )?,
            register_cleanup: import(
                CONTINUATION_REGISTER_CLEANUP_SYMBOL,
                vec![pointer, types::I8, pointer, pointer],
                Some(types::I8),
            )?,
            register_cleanup_region: import(
                CONTINUATION_REGISTER_CLEANUP_REGION_SYMBOL,
                vec![pointer, types::I8, pointer, pointer],
                Some(types::I8),
            )?,
            group_cancel_free: import(
                joky_runtime_abi::symbols::TASK_GROUP_CANCEL_FREE_SYMBOL,
                vec![pointer],
                None,
            )?,
            managed_drop,
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Continuation allocation requires entry identities, typed cleanup tables and a caller-specific selection predicate."
)]
pub(super) fn initialize_continuations(
    builder: &mut FunctionBuilder<'_>,
    function: &MirFunction,
    continuation_entry_keys: &HashMap<MirContinuationId, usize>,
    refs: ContinuationAllocationRefs,
    class_drop_refs: &HashMap<usize, FuncRef>,
    continuation_cleanup_drop_refs: &HashMap<Type, FuncRef>,
    types: &TypeTable,
    pointer_type: cranelift_codegen::ir::Type,
    selected: impl Fn(&crate::mir::MirContinuation) -> bool,
) -> Result<ContinuationResources, CodegenError> {
    let ContinuationAllocationRefs {
        new: continuation_new_ref,
        set_resume_entry: continuation_set_resume_entry_ref,
        alloc_frame: continuation_alloc_frame_ref,
        alloc_result: continuation_alloc_result_ref,
        alloc_spill: continuation_alloc_spill_ref,
        register_cleanup: continuation_register_cleanup_ref,
        register_cleanup_region: continuation_register_cleanup_region_ref,
        group_cancel_free: task_group_cancel_free_ref,
        managed_drop: managed_drop_ref,
    } = refs;
    let mut handles = HashMap::new();
    let mut spills = HashMap::new();
    let mut frames = HashMap::new();
    let mut results = HashMap::new();
    // A machine entry may reach a later Suspend and reuse its current
    // continuation handle. Reserve all typed local slots up front so later
    // suspensions use the same offsets and cleanup descriptors.
    let task_layout = super::task_frame::TaskFrameLayout::new(function, pointer_type, types);
    let continuation_frame_size = task_layout.size();
    let spill_layout = super::spills::SpillLayout::new(function, pointer_type, types);
    for continuation in function
        .continuations
        .iter()
        .filter(|metadata| selected(metadata))
    {
        let generation = builder
            .ins()
            .iconst(types::I64, continuation.generation as i64);
        let handle_call = builder.ins().call(continuation_new_ref, &[generation]);
        let handle = builder.inst_results(handle_call)[0];
        handles.insert(continuation.id, handle);
        let resume_entry = builder.ins().iconst(
            pointer_type,
            *continuation_entry_keys
                .get(&continuation.id)
                .expect("continuation key was allocated") as i64,
        );
        builder
            .ins()
            .call(continuation_set_resume_entry_ref, &[handle, resume_entry]);
        let frame_size = if !continuation.kind.uses_heap_storage() {
            0
        } else {
            continuation_frame_size
        };
        if frame_size != 0 {
            let size = builder.ins().iconst(pointer_type, frame_size as i64);
            let frame_call = builder
                .ins()
                .call(continuation_alloc_frame_ref, &[handle, size]);
            frames.insert(continuation.id, builder.inst_results(frame_call)[0]);
        }
        let result_size = if !continuation.kind.uses_heap_storage() {
            0
        } else {
            abi_types(function.return_type, pointer_type, types)
                .len()
                .saturating_mul(8)
        };
        if result_size != 0 {
            let size = builder.ins().iconst(pointer_type, result_size as i64);
            let result_call = builder
                .ins()
                .call(continuation_alloc_result_ref, &[handle, size]);
            results.insert(continuation.id, builder.inst_results(result_call)[0]);
        }
        let spill_size = if !continuation.kind.uses_heap_storage() {
            0
        } else {
            spill_layout.size
        };
        if !continuation.kind.uses_heap_storage() {
            continue;
        }
        // Scope slots stay armed across resuspension. ScopeExit zeros them;
        // cancellation drains remaining groups from innermost to outermost.
        for scope in task_layout.scopes.iter().rev() {
            let storage = builder
                .ins()
                .iconst(types::I8, CONTINUATION_FRAME_STORAGE as i64);
            let offset = builder.ins().iconst(
                pointer_type,
                task_layout.scope_offset(*scope).unwrap() as i64,
            );
            let callback = builder
                .ins()
                .func_addr(pointer_type, task_group_cancel_free_ref);
            builder.ins().call(
                continuation_register_cleanup_ref,
                &[handle, storage, offset, callback],
            );
        }
        if spill_size != 0 {
            let size = builder.ins().iconst(pointer_type, spill_size as i64);
            let spill_call = builder
                .ins()
                .call(continuation_alloc_spill_ref, &[handle, size]);
            spills.insert(continuation.id, builder.inst_results(spill_call)[0]);
        }

        // Register every independently droppable component. Tuples
        // and structs have a fixed flattened layout, so their direct
        // managed leaves can be cleaned without runtime type tags.
        // Cleanups run in registration order; later locals drop first,
        // matching the explicit drop_local order of cancellation checkpoints.
        let frame_layout = super::task_frame::LocalFrameLayout::new(function, pointer_type, types);
        for (local, frame_offset) in frame_layout.slots.iter().rev() {
            let local = &function.locals[local.0];
            if local.ownership == MirOwnership::Borrowed {
                continue;
            }
            register_cleanup_components(
                builder,
                handle,
                CONTINUATION_FRAME_STORAGE,
                local.ty,
                *frame_offset,
                pointer_type,
                types,
                continuation_register_cleanup_ref,
                continuation_register_cleanup_region_ref,
                managed_drop_ref,
                class_drop_refs,
                continuation_cleanup_drop_refs,
            )?;
        }
        for (value, spill_offset) in &spill_layout.slots {
            let ty = function.value_types[value.0];
            if function.value_ownership[value.0] == MirOwnership::Borrowed {
                continue;
            }
            register_cleanup_components(
                builder,
                handle,
                CONTINUATION_SPILL_STORAGE,
                ty,
                *spill_offset,
                pointer_type,
                types,
                continuation_register_cleanup_ref,
                continuation_register_cleanup_region_ref,
                managed_drop_ref,
                class_drop_refs,
                continuation_cleanup_drop_refs,
            )?;
        }

        // A suspended call may have already materialized an owned
        // return value before cancellation. Keep cleanup metadata for
        // the result buffer as well; normal completion deliberately
        // leaves this buffer for the task join/claim path.
        register_cleanup_components(
            builder,
            handle,
            CONTINUATION_RESULT_STORAGE,
            function.return_type,
            0,
            pointer_type,
            types,
            continuation_register_cleanup_ref,
            continuation_register_cleanup_region_ref,
            managed_drop_ref,
            class_drop_refs,
            continuation_cleanup_drop_refs,
        )?;
    }
    Ok(ContinuationResources {
        handles,
        spills,
        frames,
        results,
    })
}

#[allow(clippy::too_many_arguments)]
fn register_cleanup_components(
    builder: &mut FunctionBuilder<'_>,
    handle: Value,
    storage_kind: u8,
    ty: Type,
    base_offset: usize,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
    register_cleanup_ref: FuncRef,
    register_cleanup_region_ref: FuncRef,
    managed_drop_ref: FuncRef,
    class_drop_refs: &HashMap<usize, FuncRef>,
    continuation_cleanup_drop_refs: &HashMap<Type, FuncRef>,
) -> Result<(), CodegenError> {
    for (offset, leaf_type) in continuation_cleanup_components(ty, base_offset, types) {
        let callback = continuation_cleanup_callback(
            builder,
            leaf_type,
            pointer_type,
            managed_drop_ref,
            class_drop_refs,
        )?;
        let storage = builder.ins().iconst(types::I8, storage_kind as i64);
        let offset = builder.ins().iconst(pointer_type, offset as i64);
        builder
            .ins()
            .call(register_cleanup_ref, &[handle, storage, offset, callback]);
    }
    for (offset, tagged_type) in continuation_cleanup_regions(ty, base_offset, types) {
        let drop = continuation_cleanup_drop_refs
            .get(&tagged_type)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "missing tagged continuation cleanup glue".to_owned(),
            })?;
        let callback = builder.ins().func_addr(pointer_type, *drop);
        let storage = builder.ins().iconst(types::I8, storage_kind as i64);
        let offset = builder.ins().iconst(pointer_type, offset as i64);
        builder.ins().call(
            register_cleanup_region_ref,
            &[handle, storage, offset, callback],
        );
    }
    Ok(())
}
