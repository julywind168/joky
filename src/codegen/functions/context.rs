use std::collections::HashMap;

use crate::mir::MirFunctionId;
use crate::sema::{Type, TypeTable};
use cranelift_codegen::ir::GlobalValue;

#[derive(Clone, Copy)]
pub(crate) enum StringValue {
    Pointer(*const u8),
    Global(GlobalValue),
}

use super::super::cranelift::TaskRuntimeIds;
use super::super::environment::FunctionType;

/// Shared inputs for compiling one MIR function.
///
/// Keeping these together makes the function compiler independent from the
/// backend's runtime-symbol declaration order and avoids a call site with
/// dozens of positional `FuncId` arguments.
pub(crate) struct CompileContext<'a> {
    pub(crate) types: &'a TypeTable,
    pub(crate) func_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    pub(crate) closure_call_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    pub(crate) closure_drop_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    /// C trampolines for native callback arguments, keyed by
    /// (extern callee, argument value).
    pub(crate) callback_trampoline_ids:
        &'a HashMap<crate::codegen::environment::CallbackKey, cranelift_module::FuncId>,
    pub(crate) continuation_entry_ids:
        &'a HashMap<(MirFunctionId, crate::mir::MirContinuationId), cranelift_module::FuncId>,
    pub(crate) println_id: cranelift_module::FuncId,
    pub(crate) print_id: cranelift_module::FuncId,
    pub(crate) panic_id: cranelift_module::FuncId,
    pub(crate) continuation_resuspend_suspend_id: cranelift_module::FuncId,
    pub(crate) continuation_resuspend_suspend_payload_id: cranelift_module::FuncId,
    pub(crate) continuation_new_id: cranelift_module::FuncId,
    pub(crate) continuation_complete_function_pending_id: cranelift_module::FuncId,
    pub(crate) continuation_start_pending_timer_id: cranelift_module::FuncId,
    pub(crate) continuation_start_pending_provider_id: cranelift_module::FuncId,
    pub(crate) continuation_register_cleanup_id: cranelift_module::FuncId,
    pub(crate) continuation_register_cleanup_region_id: cranelift_module::FuncId,
    pub(crate) continuation_alloc_frame_id: cranelift_module::FuncId,
    pub(crate) continuation_alloc_result_id: cranelift_module::FuncId,
    pub(crate) continuation_alloc_suspend_result_id: cranelift_module::FuncId,
    pub(crate) continuation_release_suspend_result_id: cranelift_module::FuncId,
    pub(crate) continuation_clear_suspend_cleanups_id: cranelift_module::FuncId,
    pub(crate) continuation_alloc_spill_id: cranelift_module::FuncId,
    pub(crate) continuation_free_id: cranelift_module::FuncId,
    pub(crate) continuation_set_resume_entry_id: cranelift_module::FuncId,
    pub(crate) continuation_set_program_counter_id: cranelift_module::FuncId,
    pub(crate) continuation_set_root_group_id: cranelift_module::FuncId,
    pub(crate) continuation_root_group_id: cranelift_module::FuncId,
    pub(crate) continuation_resume_at_id: cranelift_module::FuncId,
    pub(crate) continuation_complete_id: cranelift_module::FuncId,
    pub(crate) continuation_complete_suspend_id: cranelift_module::FuncId,
    pub(crate) continuation_is_cancelled_id: cranelift_module::FuncId,
    pub(crate) continuation_frame_pointer_id: cranelift_module::FuncId,
    pub(crate) continuation_spill_pointer_id: cranelift_module::FuncId,
    pub(crate) continuation_result_pointer_id: cranelift_module::FuncId,
    pub(crate) continuation_suspend_result_pointer_id: cranelift_module::FuncId,
    pub(crate) allocate_id: cranelift_module::FuncId,
    pub(crate) closure_allocate_id: cranelift_module::FuncId,
    pub(crate) dup_id: cranelift_module::FuncId,
    pub(crate) drop_id: cranelift_module::FuncId,
    pub(crate) string_from_id: cranelift_module::FuncId,
    pub(crate) string_len_id: cranelift_module::FuncId,
    pub(crate) string_c_string_check_id: cranelift_module::FuncId,
    pub(crate) string_from_cstr_id: cranelift_module::FuncId,
    pub(crate) c_alloc_id: cranelift_module::FuncId,
    pub(crate) c_free_id: cranelift_module::FuncId,
    pub(crate) show_i64_id: cranelift_module::FuncId,
    pub(crate) debug_path_id: cranelift_module::FuncId,
    pub(crate) debug_path_status_id: cranelift_module::FuncId,
    pub(crate) debug_native_id_id: cranelift_module::FuncId,
    pub(crate) debug_string_id: cranelift_module::FuncId,
    pub(crate) debug_bytes_id: cranelift_module::FuncId,
    pub(crate) debug_duration_id: cranelift_module::FuncId,
    pub(crate) echo_id: cranelift_module::FuncId,
    pub(crate) show_u64_id: cranelift_module::FuncId,
    pub(crate) show_f64_id: cranelift_module::FuncId,
    pub(crate) show_bool_id: cranelift_module::FuncId,
    pub(crate) string_concat_id: cranelift_module::FuncId,
    pub(crate) string_eq_id: cranelift_module::FuncId,
    pub(crate) string_compare_id: cranelift_module::FuncId,
    pub(crate) string_starts_with_id: cranelift_module::FuncId,
    pub(crate) string_ends_with_id: cranelift_module::FuncId,
    pub(crate) string_contains_id: cranelift_module::FuncId,
    pub(crate) string_scalar_count_id: cranelift_module::FuncId,
    pub(crate) string_grapheme_count_id: cranelift_module::FuncId,
    pub(crate) string_is_ascii_id: cranelift_module::FuncId,
    pub(crate) string_trim_id: cranelift_module::FuncId,
    pub(crate) string_to_upper_id: cranelift_module::FuncId,
    pub(crate) string_to_lower_id: cranelift_module::FuncId,
    pub(crate) string_split_id: cranelift_module::FuncId,
    pub(crate) path_call_id: cranelift_module::FuncId,
    pub(crate) string_replace_id: cranelift_module::FuncId,
    pub(crate) string_get_byte_id: cranelift_module::FuncId,
    pub(crate) string_slice_id: cranelift_module::FuncId,
    pub(crate) class_drop_ids: &'a HashMap<usize, cranelift_module::FuncId>,
    pub(crate) continuation_cleanup_drop_ids: &'a HashMap<Type, cranelift_module::FuncId>,
    pub(crate) pointer_type: cranelift_codegen::ir::Type,
    pub(crate) string_values: &'a mut dyn Iterator<Item = (StringValue, usize)>,
    pub(crate) function_types: &'a HashMap<MirFunctionId, FunctionType>,
    pub(crate) task_thunk_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    pub(crate) handler_thunk_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    pub(crate) task_result_drop_ids: &'a HashMap<MirFunctionId, cranelift_module::FuncId>,
    pub(crate) task_failure_drop_ids:
        &'a HashMap<crate::sema::EffectOperationId, cranelift_module::FuncId>,
    pub(crate) task_runtime_ids: TaskRuntimeIds,
    pub(crate) function_id: MirFunctionId,
    pub(crate) is_main: bool,
    pub(crate) machine_entry_literals: &'a [Box<[u8]>],
}
