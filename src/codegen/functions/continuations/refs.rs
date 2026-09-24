use std::collections::HashMap;

use cranelift_codegen::ir::{FuncRef, Function};
use cranelift_module::Module;

use crate::codegen::environment::UserFunctionRef;
use crate::mir::MirFunctionId;
use crate::sema::Type;

use super::super::runtime_ids::{declare_runtime_call_refs, RuntimeCallIds};
use super::super::statements::RuntimeCallRefs;

#[derive(Clone, Copy)]
pub(super) struct ContinuationEntryIds {
    pub(super) complete: cranelift_module::FuncId,
    pub(super) is_cancelled: cranelift_module::FuncId,
    pub(super) frame_pointer: cranelift_module::FuncId,
    pub(super) spill_pointer: cranelift_module::FuncId,
    pub(super) result_pointer: cranelift_module::FuncId,
    pub(super) suspend_result_pointer: cranelift_module::FuncId,
    pub(super) alloc_suspend_result: cranelift_module::FuncId,
    pub(super) release_suspend_result: cranelift_module::FuncId,
    pub(super) clear_suspend_cleanups: cranelift_module::FuncId,
    pub(super) register_cleanup: cranelift_module::FuncId,
    pub(super) register_cleanup_region: cranelift_module::FuncId,
    pub(super) resuspend_suspend: cranelift_module::FuncId,
    pub(super) resuspend_suspend_payload: cranelift_module::FuncId,
    pub(super) set_resume_entry: cranelift_module::FuncId,
    pub(super) set_program_counter: cranelift_module::FuncId,
    pub(super) root_group: cranelift_module::FuncId,
    pub(super) group_new: cranelift_module::FuncId,
    pub(super) spawn_heap: cranelift_module::FuncId,
    pub(super) group_free: cranelift_module::FuncId,
    pub(super) failure_operation: cranelift_module::FuncId,
    pub(super) failure_claim: cranelift_module::FuncId,
    pub(super) failure_rethrow: cranelift_module::FuncId,
    pub(super) task_join: cranelift_module::FuncId,
    pub(super) task_claim_result: cranelift_module::FuncId,
    pub(super) task_cancel: cranelift_module::FuncId,
    pub(super) task_result_pointer: cranelift_module::FuncId,
    pub(super) race: cranelift_module::FuncId,
    pub(super) failure_payload: cranelift_module::FuncId,
    pub(super) handler_resumption: HandlerResumptionIds,
    pub(super) abort: cranelift_module::FuncId,
    pub(super) abort_payload: cranelift_module::FuncId,
}

/// Runtime functions a machine entry needs to issue further synchronous
/// Normal handler requests from its resume tail.
#[derive(Clone, Copy)]
pub(super) struct HandlerResumptionIds {
    pub(super) begin_payload: cranelift_module::FuncId,
    pub(super) begin_payload_env: cranelift_module::FuncId,
    pub(super) dispatch: cranelift_module::FuncId,
    pub(super) payload_copy: cranelift_module::FuncId,
    pub(super) payload_consume: cranelift_module::FuncId,
    pub(super) free: cranelift_module::FuncId,
}

#[derive(Clone, Copy)]
pub(super) struct ContinuationEntryRefs {
    pub(super) complete: FuncRef,
    pub(super) is_cancelled: FuncRef,
    pub(super) frame_pointer: FuncRef,
    pub(super) spill_pointer: FuncRef,
    pub(super) result_pointer: FuncRef,
    pub(super) suspend_result_pointer: FuncRef,
    pub(super) alloc_suspend_result: FuncRef,
    pub(super) release_suspend_result: FuncRef,
    pub(super) clear_suspend_cleanups: FuncRef,
    pub(super) register_cleanup: FuncRef,
    pub(super) register_cleanup_region: FuncRef,
    pub(super) resuspend_suspend: FuncRef,
    pub(super) resuspend_suspend_payload: FuncRef,
    pub(super) set_resume_entry: FuncRef,
    pub(super) set_program_counter: FuncRef,
    pub(super) root_group: FuncRef,
    pub(super) group_new: FuncRef,
    pub(super) spawn_heap: FuncRef,
    pub(super) group_free: FuncRef,
    pub(super) failure_operation: FuncRef,
    pub(super) failure_claim: FuncRef,
    pub(super) failure_rethrow: FuncRef,
    pub(super) task_join: FuncRef,
    pub(super) task_claim_result: FuncRef,
    pub(super) task_cancel: FuncRef,
    pub(super) task_result_pointer: FuncRef,
    pub(super) race: FuncRef,
    pub(super) failure_payload: FuncRef,
    pub(super) handler_begin_payload: FuncRef,
    pub(super) handler_begin_payload_env: FuncRef,
    pub(super) handler_dispatch: FuncRef,
    pub(super) handler_payload_copy: FuncRef,
    pub(super) handler_payload_consume: FuncRef,
    pub(super) handler_free_resumption: FuncRef,
    pub(super) abort: FuncRef,
    pub(super) abort_payload: FuncRef,
    pub(super) calls: RuntimeCallRefs,
}

pub(super) struct ContinuationCodegenRefs {
    pub(super) class_drop_refs: HashMap<usize, FuncRef>,
    pub(super) continuation_cleanup_drop_refs: HashMap<Type, FuncRef>,
    pub(super) function_refs: HashMap<MirFunctionId, UserFunctionRef>,
    pub(super) closure_call_refs: HashMap<MirFunctionId, FuncRef>,
    pub(super) closure_drop_refs: HashMap<MirFunctionId, FuncRef>,
    pub(super) callback_trampoline_refs: HashMap<crate::codegen::environment::CallbackKey, FuncRef>,
    pub(super) task_thunk_refs: HashMap<MirFunctionId, FuncRef>,
    pub(super) task_result_drop_refs: HashMap<MirFunctionId, FuncRef>,
}

pub(super) fn declare_continuation_entry_refs(
    module: &mut impl Module,
    function: &mut Function,
    ids: ContinuationEntryIds,
    runtime_call_ids: RuntimeCallIds,
) -> ContinuationEntryRefs {
    macro_rules! reference {
        ($field:ident) => {
            module.declare_func_in_func(ids.$field, function)
        };
    }

    let calls = declare_runtime_call_refs(module, function, runtime_call_ids);
    ContinuationEntryRefs {
        complete: reference!(complete),
        is_cancelled: reference!(is_cancelled),
        frame_pointer: reference!(frame_pointer),
        spill_pointer: reference!(spill_pointer),
        result_pointer: reference!(result_pointer),
        suspend_result_pointer: reference!(suspend_result_pointer),
        alloc_suspend_result: reference!(alloc_suspend_result),
        release_suspend_result: reference!(release_suspend_result),
        clear_suspend_cleanups: reference!(clear_suspend_cleanups),
        register_cleanup: reference!(register_cleanup),
        register_cleanup_region: reference!(register_cleanup_region),
        resuspend_suspend: reference!(resuspend_suspend),
        resuspend_suspend_payload: reference!(resuspend_suspend_payload),
        set_resume_entry: reference!(set_resume_entry),
        set_program_counter: reference!(set_program_counter),
        root_group: reference!(root_group),
        group_new: reference!(group_new),
        spawn_heap: reference!(spawn_heap),
        group_free: reference!(group_free),
        failure_operation: reference!(failure_operation),
        failure_claim: reference!(failure_claim),
        failure_rethrow: reference!(failure_rethrow),
        task_join: reference!(task_join),
        task_claim_result: reference!(task_claim_result),
        task_cancel: reference!(task_cancel),
        task_result_pointer: reference!(task_result_pointer),
        race: reference!(race),
        failure_payload: reference!(failure_payload),
        handler_begin_payload: module
            .declare_func_in_func(ids.handler_resumption.begin_payload, function),
        handler_begin_payload_env: module
            .declare_func_in_func(ids.handler_resumption.begin_payload_env, function),
        handler_dispatch: module.declare_func_in_func(ids.handler_resumption.dispatch, function),
        handler_payload_copy: module
            .declare_func_in_func(ids.handler_resumption.payload_copy, function),
        handler_payload_consume: module
            .declare_func_in_func(ids.handler_resumption.payload_consume, function),
        handler_free_resumption: module.declare_func_in_func(ids.handler_resumption.free, function),
        abort: module.declare_func_in_func(ids.abort, function),
        abort_payload: module.declare_func_in_func(ids.abort_payload, function),
        calls,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "This declaration boundary consumes separate symbol tables to build ContinuationCodegenRefs."
)]
pub(super) fn declare_continuation_codegen_refs(
    module: &mut impl Module,
    function: &mut Function,
    class_drop_ids: &HashMap<usize, cranelift_module::FuncId>,
    continuation_cleanup_drop_ids: &HashMap<Type, cranelift_module::FuncId>,
    func_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
    closure_call_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
    closure_drop_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
    callback_trampoline_ids: &HashMap<
        crate::codegen::environment::CallbackKey,
        cranelift_module::FuncId,
    >,
    task_thunk_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
    task_result_drop_ids: &HashMap<MirFunctionId, cranelift_module::FuncId>,
) -> ContinuationCodegenRefs {
    let class_drop_refs = class_drop_ids
        .iter()
        .map(|(id, function_id)| (*id, module.declare_func_in_func(*function_id, function)))
        .collect::<HashMap<_, _>>();
    let continuation_cleanup_drop_refs = continuation_cleanup_drop_ids
        .iter()
        .map(|(ty, drop)| (*ty, module.declare_func_in_func(*drop, function)))
        .collect::<HashMap<_, _>>();
    let function_refs = func_ids
        .iter()
        .map(|(id, function_id)| {
            (
                *id,
                UserFunctionRef {
                    reference: module.declare_func_in_func(*function_id, function),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let closure_call_refs = closure_call_ids
        .iter()
        .map(|(id, function_id)| (*id, module.declare_func_in_func(*function_id, function)))
        .collect::<HashMap<_, _>>();
    let closure_drop_refs = closure_drop_ids
        .iter()
        .map(|(id, function_id)| (*id, module.declare_func_in_func(*function_id, function)))
        .collect::<HashMap<_, _>>();
    let callback_trampoline_refs = callback_trampoline_ids
        .iter()
        .map(|(key, trampoline)| (*key, module.declare_func_in_func(*trampoline, function)))
        .collect::<HashMap<_, _>>();
    let task_thunk_refs = task_thunk_ids
        .iter()
        .map(|(id, thunk)| (*id, module.declare_func_in_func(*thunk, function)))
        .collect::<HashMap<_, _>>();
    let task_result_drop_refs = task_result_drop_ids
        .iter()
        .map(|(id, drop)| (*id, module.declare_func_in_func(*drop, function)))
        .collect::<HashMap<_, _>>();

    ContinuationCodegenRefs {
        class_drop_refs,
        continuation_cleanup_drop_refs,
        function_refs,
        closure_call_refs,
        closure_drop_refs,
        callback_trampoline_refs,
        task_thunk_refs,
        task_result_drop_refs,
    }
}
