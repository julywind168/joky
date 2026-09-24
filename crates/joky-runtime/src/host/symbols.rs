//! Function addresses registered with Cranelift by an embedding host.

use crate::runtime;

macro_rules! visit_symbols {
    ($visitor:ident; $($name:literal => $function:path),+ $(,)?) => {
        $(
            $visitor($name, $function as *const u8);
        )+
    };
}

pub fn visit_control_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    macro_rules! symbol {
        ($name:literal, $function:path) => {
            visitor($name, $function as *const u8);
        };
    }

    symbol!("jk_main_result", runtime::scope::jk_main_result);
    symbol!(
        "jk_continuation_new",
        runtime::continuation::jk_continuation_new
    );
    symbol!(
        "jk_task_function_status",
        runtime::continuation::jk_task_function_status
    );
    symbol!(
        "jk_continuation_discard_ready_call",
        runtime::continuation::jk_continuation_discard_ready_call
    );
    symbol!(
        "jk_continuation_retire_function_activation",
        runtime::continuation::jk_continuation_retire_function_activation
    );
    symbol!(
        "jk_continuation_fail_function_chain",
        runtime::continuation::jk_continuation_fail_function_chain
    );
    symbol!(
        "jk_continuation_function_parent",
        runtime::continuation::jk_continuation_function_parent
    );
    symbol!(
        "jk_continuation_cancel",
        runtime::continuation::jk_continuation_cancel
    );
    symbol!(
        "jk_continuation_begin_function_pending_with_parent",
        runtime::continuation::jk_continuation_begin_function_pending_with_parent
    );
    symbol!(
        "jk_continuation_spill_pointer",
        runtime::continuation::jk_continuation_spill_pointer
    );
    symbol!(
        "jk_continuation_take_function_pending_result",
        runtime::continuation::jk_continuation_take_function_pending_result
    );
    symbol!(
        "jk_continuation_own_handler",
        runtime::continuation::jk_continuation_own_handler
    );
    symbol!(
        "jk_continuation_exit_handler",
        runtime::continuation::jk_continuation_exit_handler
    );
    symbol!(
        "jk_cown_try_acquire_many",
        runtime::managed::jk_cown_try_acquire_many
    );
    symbol!(
        "jk_continuation_start_cown_wait",
        runtime::continuation::jk_continuation_start_cown_wait
    );
    symbol!(
        "jk_continuation_start_cown_acquire",
        runtime::continuation::jk_continuation_start_cown_acquire
    );
    symbol!(
        "jk_continuation_start_task_wait",
        runtime::continuation::jk_continuation_start_task_wait
    );
    symbol!(
        "jk_continuation_start_pending_timer",
        runtime::continuation::jk_continuation_start_pending_timer
    );
    symbol!(
        "jk_continuation_start_pending_provider",
        runtime::continuation::jk_continuation_start_pending_provider
    );
    symbol!(
        "jk_continuation_begin_function_pending",
        runtime::continuation::jk_continuation_begin_function_pending
    );
    symbol!(
        "jk_continuation_complete_function_pending",
        runtime::continuation::jk_continuation_complete_function_pending
    );
    symbol!(
        "jk_continuation_poll_function_pending",
        runtime::continuation::jk_continuation_poll_function_pending
    );
    symbol!(
        "jk_continuation_poll_function_pending_resume",
        runtime::continuation::jk_continuation_poll_function_pending_resume
    );
    symbol!(
        "jk_continuation_publish_function_pending",
        runtime::continuation::jk_continuation_publish_function_pending
    );
    symbol!(
        "jk_continuation_alloc_frame",
        runtime::continuation::jk_continuation_alloc_frame
    );
    symbol!(
        "jk_continuation_alloc_result",
        runtime::continuation::jk_continuation_alloc_result
    );
    symbol!(
        "jk_continuation_alloc_suspend_result",
        runtime::continuation::jk_continuation_alloc_suspend_result
    );
    symbol!(
        "jk_continuation_resuspend_suspend",
        runtime::continuation::jk_continuation_resuspend_suspend
    );
    symbol!(
        "jk_continuation_resuspend_suspend_payload",
        runtime::continuation::jk_continuation_resuspend_suspend_payload
    );
    symbol!(
        "jk_continuation_release_frame",
        runtime::continuation::jk_continuation_release_frame
    );
    symbol!(
        "jk_continuation_release_result",
        runtime::continuation::jk_continuation_release_result
    );
    symbol!(
        "jk_continuation_release_suspend_result",
        runtime::continuation::jk_continuation_release_suspend_result
    );
    symbol!(
        "jk_continuation_result_pointer",
        runtime::continuation::jk_continuation_result_pointer
    );
    symbol!(
        "jk_continuation_result_size",
        runtime::continuation::jk_continuation_result_size
    );
    symbol!(
        "jk_continuation_suspend_result_pointer",
        runtime::continuation::jk_continuation_suspend_result_pointer
    );
    symbol!(
        "jk_continuation_suspend_result_size",
        runtime::continuation::jk_continuation_suspend_result_size
    );
    symbol!(
        "jk_continuation_suspend_arguments_pointer",
        runtime::continuation::jk_continuation_suspend_arguments_pointer
    );
    symbol!(
        "jk_continuation_suspend_arguments_size",
        runtime::continuation::jk_continuation_suspend_arguments_size
    );
    symbol!(
        "jk_continuation_frame_pointer",
        runtime::continuation::jk_continuation_frame_pointer
    );
    symbol!(
        "jk_continuation_frame_size",
        runtime::continuation::jk_continuation_frame_size
    );
    symbol!(
        "jk_continuation_set_program_counter",
        runtime::continuation::jk_continuation_set_program_counter
    );
    symbol!(
        "jk_continuation_program_counter",
        runtime::continuation::jk_continuation_program_counter
    );
    symbol!(
        "jk_continuation_alloc_spill",
        runtime::continuation::jk_continuation_alloc_spill
    );
    symbol!(
        "jk_continuation_release_spill",
        runtime::continuation::jk_continuation_release_spill
    );
    symbol!(
        "jk_continuation_free",
        runtime::continuation::jk_continuation_free
    );
    symbol!(
        "jk_continuation_set_resume_entry",
        runtime::continuation::jk_continuation_set_resume_entry
    );
    symbol!(
        "jk_continuation_set_root_group",
        runtime::continuation::jk_continuation_set_root_group
    );
    symbol!(
        "jk_continuation_root_group",
        runtime::continuation::jk_continuation_root_group
    );
    symbol!(
        "jk_continuation_resume_at",
        runtime::continuation::jk_continuation_resume_at
    );
    symbol!(
        "jk_continuation_resume_entry",
        runtime::continuation::jk_continuation_resume_entry
    );
    symbol!(
        "jk_continuation_is_cancelled",
        runtime::continuation::jk_continuation_is_cancelled
    );
    symbol!(
        "jk_continuation_set_resume_callback",
        runtime::continuation::jk_continuation_set_resume_callback
    );
    symbol!(
        "jk_continuation_dispatch",
        runtime::continuation::jk_continuation_dispatch
    );
    symbol!(
        "jk_continuation_set_machine_entry",
        runtime::continuation::jk_continuation_set_machine_entry
    );
    symbol!(
        "jk_continuation_machine_trampoline",
        runtime::continuation::jk_continuation_machine_trampoline
    );
    symbol!(
        "jk_continuation_register_machine_entry",
        runtime::continuation::jk_continuation_register_machine_entry
    );
    symbol!(
        "jk_continuation_unregister_machine_entry",
        runtime::continuation::jk_continuation_unregister_machine_entry
    );
    symbol!(
        "jk_continuation_register_suspend_provider",
        runtime::continuation::jk_continuation_register_suspend_provider
    );
    symbol!(
        "jk_continuation_unregister_suspend_provider",
        runtime::continuation::jk_continuation_unregister_suspend_provider
    );
    symbol!(
        "jk_continuation_register_cleanup",
        runtime::continuation::jk_continuation_register_cleanup
    );
    symbol!(
        "jk_continuation_register_cleanup_region",
        runtime::continuation::jk_continuation_register_cleanup_region
    );
    symbol!(
        "jk_continuation_clear_suspend_cleanups",
        runtime::continuation::jk_continuation_clear_suspend_cleanups
    );
    symbol!(
        "jk_continuation_complete",
        runtime::continuation::jk_continuation_complete
    );
    symbol!(
        "jk_continuation_complete_suspend",
        runtime::continuation::jk_continuation_complete_suspend
    );
    symbol!(
        "jk_continuation_complete_suspend_with_payload",
        runtime::continuation::jk_continuation_complete_suspend_with_payload
    );
    symbol!("jk_callback_new", runtime::callback::jk_callback_new);
    symbol!("jk_callback_invoke", runtime::callback::jk_callback_invoke);
    symbol!(
        "jk_callback_function",
        runtime::callback::jk_callback_function
    );
    symbol!(
        "jk_callback_context",
        runtime::callback::jk_callback_context
    );
    symbol!("jk_callback_failed", runtime::callback::jk_callback_failed);
}

pub fn visit_value_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    visit_symbols!(visitor;
        "jk_println" => runtime::io::jk_println,
        "jk_print" => runtime::io::jk_print,
        "jk_panic" => runtime::io::jk_panic,
        "jk_user_drop_enter" => runtime::destruction::jk_user_drop_enter,
        "jk_user_drop_exit" => runtime::destruction::jk_user_drop_exit,
        "jk_allocate" => runtime::io::jk_allocate,
        "jk_alloc_object" => runtime::managed::jk_alloc_object,
        "jk_cown_new" => runtime::managed::jk_cown_new,
        "jk_cown_acquire" => runtime::managed::jk_cown_acquire,
        "jk_cown_acquire_many" => runtime::managed::jk_cown_acquire_many,
        "jk_cown_payload" => runtime::managed::jk_cown_payload,
        "jk_cown_release" => runtime::managed::jk_cown_release,
        "jk_alloc_closure_environment" => runtime::managed::jk_alloc_closure_environment,
        "jk_dup" => runtime::managed::jk_dup,
        "jk_show_i64" => runtime::string::jk_show_i64,
        "jk_debug_string" => runtime::string::jk_debug_string,
        "jk_debug_bytes" => runtime::debug::jk_debug_bytes,
        "jk_debug_duration" => runtime::debug::jk_debug_duration,
        "jk_debug_path" => runtime::debug::jk_debug_path,
        "jk_debug_path_status" => runtime::debug::jk_debug_path_status,
        "jk_debug_native_id" => runtime::debug::jk_debug_native_id,
        "jk_echo" => runtime::io::jk_echo,
        "jk_show_u64" => runtime::string::jk_show_u64,
        "jk_show_f64" => runtime::string::jk_show_f64,
        "jk_show_bool" => runtime::string::jk_show_bool,
        "jk_drop" => runtime::managed::jk_drop,
        "jk_string_from_utf8" => runtime::string::jk_string_from_utf8,
        "jk_string_c_string_check" => runtime::string::jk_string_c_string_check,
        "jk_string_from_cstr" => runtime::string::jk_string_from_cstr,
        "jk_c_alloc" => runtime::c_memory::jk_c_alloc,
        "jk_c_free" => runtime::c_memory::jk_c_free,
        "jk_string_len" => runtime::string::jk_string_len,
        "jk_bytes_new" => runtime::bytes::jk_bytes_new,
        "jk_bytes_from_string" => runtime::bytes::jk_bytes_from_string,
        "jk_bytes_eq" => runtime::bytes::jk_bytes_eq,
        "jk_bytes_compare" => runtime::bytes::jk_bytes_compare,
        "jk_bytes_length" => runtime::bytes::jk_bytes_length,
        "jk_mut_bytes_new" => runtime::bytes::jk_mut_bytes_new,
        "jk_mut_bytes_with_capacity" => runtime::bytes::jk_mut_bytes_with_capacity,
        "jk_mut_bytes_length" => runtime::bytes::jk_mut_bytes_length,
        "jk_mut_bytes_capacity" => runtime::bytes::jk_mut_bytes_capacity,
        "jk_mut_bytes_push" => runtime::bytes::jk_mut_bytes_push,
        "jk_bytes_is_empty" => runtime::bytes::jk_bytes_is_empty,
        "jk_bytes_get" => runtime::bytes::jk_bytes_get,
        "jk_bytes_slice" => runtime::bytes::jk_bytes_slice,
        "jk_bytes_concat" => runtime::bytes::jk_bytes_concat,
        "jk_bytes_cursor_new" => runtime::bytes::jk_bytes_cursor_new,
        "jk_bytes_cursor_step" => runtime::bytes::jk_bytes_cursor_step,
        "jk_bytes_to_string" => runtime::bytes::jk_bytes_to_string,
        "jk_mut_bytes_from_bytes" => runtime::bytes::jk_mut_bytes_from_bytes,
        "jk_mut_bytes_get" => runtime::bytes::jk_mut_bytes_get,
        "jk_mut_bytes_set" => runtime::bytes::jk_mut_bytes_set,
        "jk_mut_bytes_pop" => runtime::bytes::jk_mut_bytes_pop,
        "jk_mut_bytes_clear" => runtime::bytes::jk_mut_bytes_clear,
        "jk_mut_bytes_reserve" => runtime::bytes::jk_mut_bytes_reserve,
        "jk_mut_bytes_to_bytes" => runtime::bytes::jk_mut_bytes_to_bytes,
        "jk_mut_bytes_extend" => runtime::bytes::jk_mut_bytes_extend,
        "jk_string_concat" => runtime::string::jk_string_concat,
        "jk_string_eq" => runtime::string::jk_string_eq,
        "jk_string_compare" => runtime::string::jk_string_compare,
        "jk_string_starts_with" => runtime::string::jk_string_starts_with,
        "jk_string_ends_with" => runtime::string::jk_string_ends_with,
        "jk_string_contains" => runtime::string::jk_string_contains,
        "jk_string_scalar_count" => runtime::string::jk_string_scalar_count,
        "jk_string_grapheme_count" => runtime::string::jk_string_grapheme_count,
        "jk_string_is_ascii" => runtime::string::jk_string_is_ascii,
        "jk_string_trim" => runtime::string::jk_string_trim,
        "jk_string_to_upper" => runtime::string::jk_string_to_upper,
        "jk_string_to_lower" => runtime::string::jk_string_to_lower,
        "jk_path_call" => runtime::path::jk_path_call,
        "jk_string_split" => runtime::string::jk_string_split,
        "jk_string_replace" => runtime::string::jk_string_replace,
        "jk_string_get_byte" => runtime::string::jk_string_get_byte,
        "jk_string_slice" => runtime::string::jk_string_slice,
        "jk_string_parse_i8" => runtime::string::jk_string_parse_i8,
        "jk_string_parse_i16" => runtime::string::jk_string_parse_i16,
        "jk_string_parse_i64" => runtime::string::jk_string_parse_i64,
        "jk_string_parse_u8" => runtime::string::jk_string_parse_u8,
        "jk_string_parse_u16" => runtime::string::jk_string_parse_u16,
        "jk_string_parse_u32" => runtime::string::jk_string_parse_u32,
        "jk_string_parse_u64" => runtime::string::jk_string_parse_u64,
        "jk_string_parse_f32" => runtime::string::jk_string_parse_f32,
        "jk_string_parse_bool" => runtime::string::jk_string_parse_bool,
        "jk_string_parse_i32" => runtime::string::jk_string_parse_i32,
        "jk_string_parse_f64" => runtime::string::jk_string_parse_f64,
        "jk_list_cons" => runtime::list::jk_list_cons,
        "jk_list_head" => runtime::list::jk_list_head,
        "jk_list_tail" => runtime::list::jk_list_tail,
        "jk_list_length" => runtime::list::jk_list_length,
        "jk_list_reverse" => runtime::list::jk_list_reverse,
        "jk_mut_list_new" => runtime::mut_list::jk_mut_list_new,
        "jk_mut_list_push" => runtime::mut_list::jk_mut_list_push,
        "jk_mut_list_get" => runtime::mut_list::jk_mut_list_get,
        "jk_mut_list_set" => runtime::mut_list::jk_mut_list_set,
        "jk_mut_list_pop" => runtime::mut_list::jk_mut_list_pop,
        "jk_mut_list_length" => runtime::mut_list::jk_mut_list_length,
        "jk_mut_list_capacity" => runtime::mut_list::jk_mut_list_capacity,
        "jk_mut_list_cursor_new" => runtime::mut_list::jk_mut_list_cursor_new,
        "jk_mut_list_cursor_step" => runtime::mut_list::jk_mut_list_cursor_step,
        "jk_mut_list_to_list" => runtime::mut_list::jk_mut_list_to_list,
        "jk_map_insert" => runtime::map::jk_map_insert,
        "jk_hasher_new" => runtime::hashing::jk_hasher_new,
        "jk_hasher_word" => runtime::hashing::jk_hasher_word,
        "jk_hasher_string" => runtime::hashing::jk_hasher_string,
        "jk_hasher_bytes" => runtime::hashing::jk_hasher_bytes,
        "jk_hasher_finish" => runtime::hashing::jk_hasher_finish,
        "jk_map_get" => runtime::map::jk_map_get,
        "jk_map_remove" => runtime::map::jk_map_remove,
        "jk_map_contains_key" => runtime::map::jk_map_contains_key,
        "jk_map_cursor_new" => runtime::map::jk_map_cursor_new,
        "jk_map_cursor_step" => runtime::map::jk_map_cursor_step,
        "jk_seq_new" => runtime::list::jk_seq_new,
        "jk_seq_push" => runtime::list::jk_seq_push,
        "jk_seq_finish" => runtime::list::jk_seq_finish,
        "jk_map_length" => runtime::map::jk_map_length,
        "jk_mut_map_new" => runtime::mut_map::jk_mut_map_new,
        "jk_mut_map_insert" => runtime::mut_map::jk_mut_map_insert,
        "jk_mut_map_get" => runtime::mut_map::jk_mut_map_get,
        "jk_mut_map_remove" => runtime::mut_map::jk_mut_map_remove,
        "jk_mut_map_contains_key" => runtime::mut_map::jk_mut_map_contains_key,
        "jk_mut_map_length" => runtime::mut_map::jk_mut_map_length,
        "jk_mut_map_capacity" => runtime::mut_map::jk_mut_map_capacity,
        "jk_mut_map_is_empty" => runtime::mut_map::jk_mut_map_is_empty,
        "jk_mut_map_cursor_new" => runtime::mut_map::jk_mut_map_cursor_new,
        "jk_mut_map_cursor_step" => runtime::mut_map::jk_mut_map_cursor_step,
        "jk_mut_map_to_list" => runtime::mut_map::jk_mut_map_to_list,
    );
}

pub fn visit_task_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    visit_symbols!(visitor;
        "jk_task_group_new" => runtime::task::jk_task_group_new,
        "jk_task_group_free" => runtime::task::jk_task_group_free,
        "jk_task_spawn" => runtime::task::jk_task_spawn,
        "jk_task_spawn_heap" => runtime::task::jk_task_spawn_heap,
        "jk_task_join" => runtime::task::jk_task_join,
        "jk_task_cancel" => runtime::task::jk_task_cancel,
        "jk_task_race" => runtime::task::jk_task_race,
        "jk_task_claim_result" => runtime::task::jk_task_claim_result,
        "jk_task_group_cancel_free" => runtime::task::jk_task_group_cancel_free,
        "jk_task_result_pointer" => runtime::task::jk_task_result_pointer,
        "jk_task_is_cancelled" => runtime::task::jk_task_is_cancelled,
        "jk_task_mark_cancelled_result" => runtime::task::jk_task_mark_cancelled_result,
        "jk_task_abort" => runtime::task::jk_task_abort,
        "jk_task_abort_payload" => runtime::task::jk_task_abort_payload,
        "jk_task_group_failure_operation" => runtime::task::jk_task_group_failure_operation,
        "jk_task_group_failure_payload" => runtime::task::jk_task_group_failure_payload,
        "jk_task_group_claim_failure" => runtime::task::jk_task_group_claim_failure,
        "jk_task_rethrow_failure" => runtime::task::jk_task_rethrow_failure,
    );
}

pub fn visit_handler_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    visit_symbols!(visitor;
        "jk_handler_frame_new" => runtime::handler::jk_handler_frame_new,
        "jk_handler_frame_enter" => runtime::handler::jk_handler_frame_enter,
        "jk_handler_frame_exit" => runtime::handler::jk_handler_frame_exit,
        "jk_handler_frame_free" => runtime::handler::jk_handler_frame_free,
        "jk_handler_frame_begin_resumption_handle" => runtime::handler::jk_handler_frame_begin_resumption_handle,
        "jk_handler_frame_begin_resumption_handle_with_payload" => runtime::handler::jk_handler_frame_begin_resumption_handle_with_payload,
        "jk_handler_frame_begin_resumption_handle_with_payload_env" => runtime::handler::jk_handler_frame_begin_resumption_handle_with_payload_env,
        "jk_handler_frame_resume_handle" => runtime::handler::jk_handler_frame_resume_handle,
        "jk_handler_frame_free_resumption_handle" => runtime::handler::jk_handler_frame_free_resumption_handle,
        "jk_handler_frame_resumption_payload_copy" => runtime::handler::jk_handler_frame_resumption_payload_copy,
        "jk_handler_frame_resumption_payload_consume" => runtime::handler::jk_handler_frame_resumption_payload_consume,
        "jk_handler_frame_invoke_thunk" => runtime::handler::jk_handler_frame_invoke_thunk,
        "jk_handler_frame_set_resumption_thunk" => runtime::handler::jk_handler_frame_set_resumption_thunk,
        "jk_handler_frame_set_resumption_thunk_with_env" => runtime::handler::jk_handler_frame_set_resumption_thunk_with_env,
        "jk_handler_frame_set_resumption_thunk_with_result_env" => runtime::handler::jk_handler_frame_set_resumption_thunk_with_result_env,
        "jk_handler_frame_static_thunk" => runtime::handler::jk_handler_frame_static_thunk,
        "jk_handler_frame_forward_thunk" => runtime::handler::jk_handler_frame_forward_thunk,
        "jk_handler_frame_transform_i64_thunk" => runtime::handler::jk_handler_frame_transform_i64_thunk,
        "jk_handler_frame_bind_resumption_handle" => runtime::handler::jk_handler_frame_bind_resumption_handle,
        "jk_handler_frame_set_resumption_payload_thunk" => runtime::handler::jk_handler_frame_set_resumption_payload_thunk,
        "jk_handler_frame_dispatch_resumption_handle" => runtime::handler::jk_handler_frame_dispatch_resumption_handle,
        "jk_handler_frame_request_payload_copy" => runtime::handler::jk_handler_frame_request_payload_copy,
        "jk_handler_frame_request_payload_size" => runtime::handler::jk_handler_frame_request_payload_size,
        "jk_handler_frame_resumption_handle_operation" => runtime::handler::jk_handler_frame_resumption_handle_operation,
        "jk_handler_frame_resumption_payload_size" => runtime::handler::jk_handler_frame_resumption_payload_size,
        "jk_handler_frame_set_resumption_string" => runtime::handler::jk_handler_frame_set_resumption_string,
        "jk_handler_frame_set_resumption_payload" => runtime::handler::jk_handler_frame_set_resumption_payload,
        "jk_handler_frame_set_resumption_managed_payload" => runtime::handler::jk_handler_frame_set_resumption_managed_payload,
    );
}

pub fn visit_batch_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    visit_symbols!(visitor;
        "jk_batch_new" => runtime::batch::jk_batch_new,
        "jk_batch_worker" => runtime::batch::jk_batch_worker,
        "jk_batch_next" => runtime::batch::jk_batch_next,
        "jk_batch_push" => runtime::batch::jk_batch_push,
        "jk_batch_finish" => runtime::batch::jk_batch_finish,
    );
}

pub fn visit_jit_symbols(mut visitor: impl FnMut(&'static str, *const u8)) {
    visit_control_symbols(&mut visitor);
    visit_value_symbols(&mut visitor);
    visit_task_symbols(&mut visitor);
    visit_handler_symbols(&mut visitor);
    visit_batch_symbols(visitor);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn control_symbol_table_has_unique_non_null_entries() {
        let mut names = BTreeSet::new();
        visit_control_symbols(|name, address| {
            assert!(!address.is_null(), "{name}");
            assert!(names.insert(name), "duplicate JIT symbol: {name}");
        });
        assert_eq!(names.len(), 70);
    }

    #[test]
    fn complete_jit_symbol_table_has_unique_non_null_entries() {
        let mut names = BTreeSet::new();
        visit_jit_symbols(|name, address| {
            assert!(!address.is_null(), "{name}");
            assert!(names.insert(name), "duplicate JIT symbol: {name}");
        });
        assert_eq!(names.len(), 247);
        for name in joky_runtime_abi::symbols::ALL {
            assert!(
                names.contains(name),
                "ABI symbol missing from JIT table: {name}"
            );
        }
    }
}
