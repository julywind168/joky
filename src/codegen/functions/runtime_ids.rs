#[derive(Clone, Copy)]
pub(super) struct RuntimeCallIds {
    pub(super) hasher_new: cranelift_module::FuncId,
    pub(super) hasher_word: cranelift_module::FuncId,
    pub(super) hasher_string: cranelift_module::FuncId,
    pub(super) hasher_bytes: cranelift_module::FuncId,
    pub(super) hasher_finish: cranelift_module::FuncId,
    pub(super) cown_new: cranelift_module::FuncId,
    pub(super) cown_acquire: cranelift_module::FuncId,
    pub(super) cown_acquire_many: cranelift_module::FuncId,
    pub(super) cown_payload: cranelift_module::FuncId,
    pub(super) cown_release: cranelift_module::FuncId,
    pub(super) print: cranelift_module::FuncId,
    pub(super) println: cranelift_module::FuncId,
    pub(super) panic: cranelift_module::FuncId,
    pub(super) allocate: cranelift_module::FuncId,
    pub(super) closure_allocate: cranelift_module::FuncId,
    pub(super) dup: cranelift_module::FuncId,
    pub(super) managed_drop: cranelift_module::FuncId,
    pub(super) string_from: cranelift_module::FuncId,
    pub(super) bytes_from_data: cranelift_module::FuncId,
    pub(super) string_len: cranelift_module::FuncId,
    pub(super) string_c_string_check: cranelift_module::FuncId,
    pub(super) string_from_cstr: cranelift_module::FuncId,
    pub(super) c_alloc: cranelift_module::FuncId,
    pub(super) c_free: cranelift_module::FuncId,
    pub(super) callback_function: cranelift_module::FuncId,
    pub(super) callback_context: cranelift_module::FuncId,
    pub(super) callback_failed: cranelift_module::FuncId,
    pub(super) show_i64: cranelift_module::FuncId,
    pub(super) debug_path: cranelift_module::FuncId,
    pub(super) debug_path_status: cranelift_module::FuncId,
    pub(super) debug_native_id: cranelift_module::FuncId,
    pub(super) debug_string: cranelift_module::FuncId,
    pub(super) debug_bytes: cranelift_module::FuncId,
    pub(super) debug_duration: cranelift_module::FuncId,
    pub(super) echo: cranelift_module::FuncId,
    pub(super) show_u64: cranelift_module::FuncId,
    pub(super) show_f64: cranelift_module::FuncId,
    pub(super) show_bool: cranelift_module::FuncId,
    pub(super) string_concat: cranelift_module::FuncId,
    pub(super) string_eq: cranelift_module::FuncId,
    pub(super) string_compare: cranelift_module::FuncId,
    pub(super) string_starts_with: cranelift_module::FuncId,
    pub(super) string_ends_with: cranelift_module::FuncId,
    pub(super) string_contains: cranelift_module::FuncId,
    pub(super) string_scalar_count: cranelift_module::FuncId,
    pub(super) string_grapheme_count: cranelift_module::FuncId,
    pub(super) string_is_ascii: cranelift_module::FuncId,
    pub(super) string_trim: cranelift_module::FuncId,
    pub(super) string_to_upper: cranelift_module::FuncId,
    pub(super) string_to_lower: cranelift_module::FuncId,
    pub(super) string_split: cranelift_module::FuncId,
    pub(super) path_call: cranelift_module::FuncId,
    pub(super) string_replace: cranelift_module::FuncId,
    pub(super) string_get_byte: cranelift_module::FuncId,
    pub(super) string_slice: cranelift_module::FuncId,
    pub(super) parse_i8: cranelift_module::FuncId,
    pub(super) parse_i16: cranelift_module::FuncId,
    pub(super) parse_i64: cranelift_module::FuncId,
    pub(super) parse_u8: cranelift_module::FuncId,
    pub(super) parse_u16: cranelift_module::FuncId,
    pub(super) parse_u32: cranelift_module::FuncId,
    pub(super) parse_u64: cranelift_module::FuncId,
    pub(super) parse_f32: cranelift_module::FuncId,
    pub(super) parse_bool: cranelift_module::FuncId,
    pub(super) parse_i32: cranelift_module::FuncId,
    pub(super) parse_f64: cranelift_module::FuncId,
    pub(super) batch_new: cranelift_module::FuncId,
    pub(super) batch_worker: cranelift_module::FuncId,
    pub(super) batch_next: cranelift_module::FuncId,
    pub(super) batch_push: cranelift_module::FuncId,
    pub(super) batch_finish: cranelift_module::FuncId,
    pub(super) seq_new: cranelift_module::FuncId,
    pub(super) seq_push: cranelift_module::FuncId,
    pub(super) seq_finish: cranelift_module::FuncId,
    pub(super) list_cons: cranelift_module::FuncId,
    pub(super) list_head: cranelift_module::FuncId,
    pub(super) list_tail: cranelift_module::FuncId,
    pub(super) list_length: cranelift_module::FuncId,
    pub(super) list_reverse: cranelift_module::FuncId,
    pub(super) mut_list_new: cranelift_module::FuncId,
    pub(super) mut_list_push: cranelift_module::FuncId,
    pub(super) mut_list_get: cranelift_module::FuncId,
    pub(super) mut_list_set: cranelift_module::FuncId,
    pub(super) mut_list_pop: cranelift_module::FuncId,
    pub(super) mut_list_length: cranelift_module::FuncId,
    pub(super) mut_list_capacity: cranelift_module::FuncId,
    pub(super) mut_list_cursor_new: cranelift_module::FuncId,
    pub(super) mut_list_cursor_step: cranelift_module::FuncId,
    pub(super) mut_list_to_list: cranelift_module::FuncId,
    pub(super) map_insert: cranelift_module::FuncId,
    pub(super) map_get: cranelift_module::FuncId,
    pub(super) map_remove: cranelift_module::FuncId,
    pub(super) map_contains_key: cranelift_module::FuncId,
    pub(super) map_length: cranelift_module::FuncId,
    pub(super) map_cursor_new: cranelift_module::FuncId,
    pub(super) map_cursor_step: cranelift_module::FuncId,
    pub(super) mut_map_new: cranelift_module::FuncId,
    pub(super) mut_map_insert: cranelift_module::FuncId,
    pub(super) mut_map_get: cranelift_module::FuncId,
    pub(super) mut_map_remove: cranelift_module::FuncId,
    pub(super) mut_map_contains_key: cranelift_module::FuncId,
    pub(super) mut_map_length: cranelift_module::FuncId,
    pub(super) mut_map_capacity: cranelift_module::FuncId,
    pub(super) mut_map_is_empty: cranelift_module::FuncId,
    pub(super) mut_map_cursor_new: cranelift_module::FuncId,
    pub(super) mut_map_cursor_step: cranelift_module::FuncId,
    pub(super) mut_map_to_list: cranelift_module::FuncId,
    pub(super) bytes_new: cranelift_module::FuncId,
    pub(super) bytes_from_string: cranelift_module::FuncId,
    pub(super) bytes_eq: cranelift_module::FuncId,
    pub(super) bytes_compare: cranelift_module::FuncId,
    pub(super) bytes_length: cranelift_module::FuncId,
    pub(super) bytes_cursor_new: cranelift_module::FuncId,
    pub(super) bytes_cursor_step: cranelift_module::FuncId,
    pub(super) mut_bytes_new: cranelift_module::FuncId,
    pub(super) mut_bytes_with_capacity: cranelift_module::FuncId,
    pub(super) mut_bytes_length: cranelift_module::FuncId,
    pub(super) mut_bytes_capacity: cranelift_module::FuncId,
    pub(super) mut_bytes_push: cranelift_module::FuncId,
    pub(super) bytes_is_empty: cranelift_module::FuncId,
    pub(super) bytes_get: cranelift_module::FuncId,
    pub(super) bytes_slice: cranelift_module::FuncId,
    pub(super) bytes_concat: cranelift_module::FuncId,
    pub(super) bytes_to_string: cranelift_module::FuncId,
    pub(super) mut_bytes_from_bytes: cranelift_module::FuncId,
    pub(super) mut_bytes_get: cranelift_module::FuncId,
    pub(super) mut_bytes_set: cranelift_module::FuncId,
    pub(super) mut_bytes_pop: cranelift_module::FuncId,
    pub(super) mut_bytes_clear: cranelift_module::FuncId,
    pub(super) mut_bytes_reserve: cranelift_module::FuncId,
    pub(super) mut_bytes_to_bytes: cranelift_module::FuncId,
    pub(super) mut_bytes_extend: cranelift_module::FuncId,
}

use cranelift_codegen::ir::{types, AbiParam, Signature};
use cranelift_module::{Linkage, Module};
use joky_runtime_abi::symbols::*;

use crate::diagnostic::CodegenError;

use super::super::cranelift::CraneliftBackend;
use super::super::cranelift::ModuleLifecycle;
use super::super::helpers::codegen_error;
use super::statements::RuntimeCallRefs;

pub(super) fn declare_runtime_call_refs(
    module: &mut impl Module,
    function: &mut cranelift_codegen::ir::Function,
    ids: RuntimeCallIds,
) -> RuntimeCallRefs {
    macro_rules! reference {
        ($field:ident) => {
            module.declare_func_in_func(ids.$field, function)
        };
    }

    RuntimeCallRefs {
        hasher_new: reference!(hasher_new),
        hasher_word: reference!(hasher_word),
        hasher_string: reference!(hasher_string),
        hasher_bytes: reference!(hasher_bytes),
        hasher_finish: reference!(hasher_finish),
        cown_new: reference!(cown_new),
        cown_acquire: reference!(cown_acquire),
        cown_acquire_many: reference!(cown_acquire_many),
        cown_payload: reference!(cown_payload),
        cown_release: reference!(cown_release),
        print: reference!(print),
        println: reference!(println),
        panic: reference!(panic),
        allocate: reference!(allocate),
        closure_allocate: reference!(closure_allocate),
        dup: reference!(dup),
        managed_drop: reference!(managed_drop),
        string_from: reference!(string_from),
        bytes_from_data: reference!(bytes_from_data),
        string_len: reference!(string_len),
        string_c_string_check: reference!(string_c_string_check),
        string_from_cstr: reference!(string_from_cstr),
        c_alloc: reference!(c_alloc),
        c_free: reference!(c_free),
        callback_function: reference!(callback_function),
        callback_context: reference!(callback_context),
        callback_failed: reference!(callback_failed),
        show_i64: reference!(show_i64),
        debug_path: reference!(debug_path),
        debug_path_status: reference!(debug_path_status),
        debug_native_id: reference!(debug_native_id),
        debug_string: reference!(debug_string),
        debug_bytes: reference!(debug_bytes),
        debug_duration: reference!(debug_duration),
        echo: reference!(echo),
        show_u64: reference!(show_u64),
        show_f64: reference!(show_f64),
        show_bool: reference!(show_bool),
        string_concat: reference!(string_concat),
        string_eq: reference!(string_eq),
        string_compare: reference!(string_compare),
        string_starts_with: reference!(string_starts_with),
        string_ends_with: reference!(string_ends_with),
        string_contains: reference!(string_contains),
        string_scalar_count: reference!(string_scalar_count),
        string_grapheme_count: reference!(string_grapheme_count),
        string_is_ascii: reference!(string_is_ascii),
        string_trim: reference!(string_trim),
        string_to_upper: reference!(string_to_upper),
        string_to_lower: reference!(string_to_lower),
        string_split: reference!(string_split),
        path_call: reference!(path_call),
        string_replace: reference!(string_replace),
        string_get_byte: reference!(string_get_byte),
        string_slice: reference!(string_slice),
        parse_i8: reference!(parse_i8),
        parse_i16: reference!(parse_i16),
        parse_i64: reference!(parse_i64),
        parse_u8: reference!(parse_u8),
        parse_u16: reference!(parse_u16),
        parse_u32: reference!(parse_u32),
        parse_u64: reference!(parse_u64),
        parse_f32: reference!(parse_f32),
        parse_bool: reference!(parse_bool),
        parse_i32: reference!(parse_i32),
        parse_f64: reference!(parse_f64),
        batch_new: reference!(batch_new),
        batch_worker: reference!(batch_worker),
        batch_next: reference!(batch_next),
        batch_push: reference!(batch_push),
        batch_finish: reference!(batch_finish),
        seq_new: reference!(seq_new),
        seq_push: reference!(seq_push),
        seq_finish: reference!(seq_finish),
        list_cons: reference!(list_cons),
        list_head: reference!(list_head),
        list_tail: reference!(list_tail),
        list_length: reference!(list_length),
        list_reverse: reference!(list_reverse),
        mut_list_new: reference!(mut_list_new),
        mut_list_push: reference!(mut_list_push),
        mut_list_get: reference!(mut_list_get),
        mut_list_set: reference!(mut_list_set),
        mut_list_pop: reference!(mut_list_pop),
        mut_list_length: reference!(mut_list_length),
        mut_list_capacity: reference!(mut_list_capacity),
        mut_list_cursor_new: reference!(mut_list_cursor_new),
        mut_list_cursor_step: reference!(mut_list_cursor_step),
        mut_list_to_list: reference!(mut_list_to_list),
        map_insert: reference!(map_insert),
        map_get: reference!(map_get),
        map_remove: reference!(map_remove),
        map_contains_key: reference!(map_contains_key),
        map_length: reference!(map_length),
        map_cursor_new: reference!(map_cursor_new),
        map_cursor_step: reference!(map_cursor_step),
        mut_map_new: reference!(mut_map_new),
        mut_map_insert: reference!(mut_map_insert),
        mut_map_get: reference!(mut_map_get),
        mut_map_remove: reference!(mut_map_remove),
        mut_map_contains_key: reference!(mut_map_contains_key),
        mut_map_length: reference!(mut_map_length),
        mut_map_capacity: reference!(mut_map_capacity),
        mut_map_is_empty: reference!(mut_map_is_empty),
        mut_map_cursor_new: reference!(mut_map_cursor_new),
        mut_map_cursor_step: reference!(mut_map_cursor_step),
        mut_map_to_list: reference!(mut_map_to_list),
        bytes_new: reference!(bytes_new),
        bytes_from_string: reference!(bytes_from_string),
        bytes_eq: reference!(bytes_eq),
        bytes_compare: reference!(bytes_compare),
        bytes_length: reference!(bytes_length),
        bytes_cursor_new: reference!(bytes_cursor_new),
        bytes_cursor_step: reference!(bytes_cursor_step),
        mut_bytes_new: reference!(mut_bytes_new),
        mut_bytes_with_capacity: reference!(mut_bytes_with_capacity),
        mut_bytes_length: reference!(mut_bytes_length),
        mut_bytes_capacity: reference!(mut_bytes_capacity),
        mut_bytes_push: reference!(mut_bytes_push),
        bytes_is_empty: reference!(bytes_is_empty),
        bytes_get: reference!(bytes_get),
        bytes_slice: reference!(bytes_slice),
        bytes_concat: reference!(bytes_concat),
        bytes_to_string: reference!(bytes_to_string),
        mut_bytes_from_bytes: reference!(mut_bytes_from_bytes),
        mut_bytes_get: reference!(mut_bytes_get),
        mut_bytes_set: reference!(mut_bytes_set),
        mut_bytes_pop: reference!(mut_bytes_pop),
        mut_bytes_clear: reference!(mut_bytes_clear),
        mut_bytes_reserve: reference!(mut_bytes_reserve),
        mut_bytes_to_bytes: reference!(mut_bytes_to_bytes),
        mut_bytes_extend: reference!(mut_bytes_extend),
    }
}

pub(super) struct RuntimeCallInputs {
    pub(crate) pointer_type: cranelift_codegen::ir::Type,
    pub(crate) print_id: cranelift_module::FuncId,
    pub(crate) println_id: cranelift_module::FuncId,
    pub(crate) panic_id: cranelift_module::FuncId,
    pub(crate) allocate_id: cranelift_module::FuncId,
    pub(crate) closure_allocate_id: cranelift_module::FuncId,
    pub(crate) dup_id: cranelift_module::FuncId,
    pub(crate) drop_id: cranelift_module::FuncId,
    pub(crate) string_from_id: cranelift_module::FuncId,
    pub(crate) bytes_from_data_id: cranelift_module::FuncId,
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
}

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(super) fn declare_runtime_call_ids(
        &mut self,
        inputs: RuntimeCallInputs,
    ) -> Result<RuntimeCallIds, CodegenError> {
        let call_conv = self.module.isa().default_call_conv();
        let RuntimeCallInputs {
            pointer_type,
            print_id,
            println_id,
            panic_id,
            allocate_id,
            closure_allocate_id,
            dup_id,
            drop_id,
            string_from_id,
            bytes_from_data_id,
            string_len_id,
            string_c_string_check_id,
            string_from_cstr_id,
            c_alloc_id,
            c_free_id,
            show_i64_id,
            debug_path_id,
            debug_path_status_id,
            debug_native_id_id,
            debug_string_id,
            debug_bytes_id,
            debug_duration_id,
            echo_id,
            show_u64_id,
            show_f64_id,
            show_bool_id,
            string_concat_id,
            string_eq_id,
            string_compare_id,
            string_starts_with_id,
            string_ends_with_id,
            string_contains_id,
            string_scalar_count_id,
            string_grapheme_count_id,
            string_is_ascii_id,
            string_trim_id,
            string_to_upper_id,
            string_to_lower_id,
            string_split_id,
            path_call_id,
            string_replace_id,
            string_get_byte_id,
            string_slice_id,
        } = inputs;
        let mut batch_signature = Signature {
            params: vec![],
            returns: vec![],
            call_conv,
        };
        batch_signature.params = vec![AbiParam::new(pointer_type); 2];
        batch_signature.returns = vec![AbiParam::new(pointer_type)];
        let batch_new_id = self
            .module
            .declare_function(BATCH_NEW_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 1];
        batch_signature.returns = vec![AbiParam::new(types::I8)];
        let batch_worker_id = self
            .module
            .declare_function(BATCH_WORKER_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 2];
        batch_signature.returns = vec![AbiParam::new(pointer_type)];
        let batch_next_id = self
            .module
            .declare_function(BATCH_NEXT_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 3];
        batch_signature.returns = vec![];
        let batch_push_id = self
            .module
            .declare_function(BATCH_PUSH_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 1];
        batch_signature.returns = vec![AbiParam::new(pointer_type)];
        let batch_finish_id = self
            .module
            .declare_function(BATCH_FINISH_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![];
        let seq_new_id = self
            .module
            .declare_function(SEQ_NEW_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 2];
        batch_signature.returns = vec![];
        let seq_push_id = self
            .module
            .declare_function(SEQ_PUSH_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        batch_signature.params = vec![AbiParam::new(pointer_type); 1];
        batch_signature.returns = vec![AbiParam::new(pointer_type)];
        let seq_finish_id = self
            .module
            .declare_function(SEQ_FINISH_SYMBOL, Linkage::Import, &batch_signature)
            .map_err(codegen_error)?;
        let parse_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let cown_new_signature = Signature {
            params: vec![AbiParam::new(pointer_type); 2],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let cown_handle_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let cown_new_id = self
            .module
            .declare_function(COWN_NEW_SYMBOL, Linkage::Import, &cown_new_signature)
            .map_err(codegen_error)?;
        let cown_acquire_id = self
            .module
            .declare_function(COWN_ACQUIRE_SYMBOL, Linkage::Import, &cown_handle_signature)
            .map_err(codegen_error)?;
        let cown_acquire_many_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let cown_acquire_many_id = self
            .module
            .declare_function(
                COWN_ACQUIRE_MANY_SYMBOL,
                Linkage::Import,
                &cown_acquire_many_signature,
            )
            .map_err(codegen_error)?;
        let cown_payload_id = self
            .module
            .declare_function(COWN_PAYLOAD_SYMBOL, Linkage::Import, &cown_handle_signature)
            .map_err(codegen_error)?;
        let cown_release_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        let cown_release_id = self
            .module
            .declare_function(
                COWN_RELEASE_SYMBOL,
                Linkage::Import,
                &cown_release_signature,
            )
            .map_err(codegen_error)?;
        let parse_i8_id = self
            .module
            .declare_function(STRING_PARSE_I8_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_i16_id = self
            .module
            .declare_function(STRING_PARSE_I16_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_i64_id = self
            .module
            .declare_function(STRING_PARSE_I64_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_u8_id = self
            .module
            .declare_function(STRING_PARSE_U8_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_u16_id = self
            .module
            .declare_function(STRING_PARSE_U16_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_u32_id = self
            .module
            .declare_function(STRING_PARSE_U32_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_u64_id = self
            .module
            .declare_function(STRING_PARSE_U64_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_f32_id = self
            .module
            .declare_function(STRING_PARSE_F32_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_bool_id = self
            .module
            .declare_function(STRING_PARSE_BOOL_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_i32_id = self
            .module
            .declare_function(STRING_PARSE_I32_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let parse_f64_id = self
            .module
            .declare_function(STRING_PARSE_F64_SYMBOL, Linkage::Import, &parse_signature)
            .map_err(codegen_error)?;
        let bytes_new_id = self
            .module
            .declare_function(
                BYTES_NEW_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_from_string_id = self
            .module
            .declare_function(
                BYTES_FROM_STRING_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_eq_id = self
            .module
            .declare_function(
                BYTES_EQ_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_compare_id = self
            .module
            .declare_function(
                BYTES_COMPARE_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I32)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_length_id = self
            .module
            .declare_function(
                BYTES_LENGTH_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_cursor_new_id = self
            .module
            .declare_function(
                BYTES_CURSOR_NEW_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_cursor_step_id = self
            .module
            .declare_function(
                BYTES_CURSOR_STEP_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_new_id = self
            .module
            .declare_function(
                MUT_BYTES_NEW_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_length_id = self
            .module
            .declare_function(
                MUT_BYTES_LENGTH_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_with_capacity_id = self
            .module
            .declare_function(
                MUT_BYTES_WITH_CAPACITY_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_capacity_id = self
            .module
            .declare_function(
                MUT_BYTES_CAPACITY_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_push_id = self
            .module
            .declare_function(
                MUT_BYTES_PUSH_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(types::I8)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_is_empty_id = self
            .module
            .declare_function(
                BYTES_IS_EMPTY_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_get_id = self
            .module
            .declare_function(
                BYTES_GET_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                    ],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_slice_id = self
            .module
            .declare_function(
                BYTES_SLICE_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                    ],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_concat_id = self
            .module
            .declare_function(
                BYTES_CONCAT_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let bytes_to_string_id = self
            .module
            .declare_function(
                BYTES_TO_STRING_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_from_bytes_id = self
            .module
            .declare_function(
                MUT_BYTES_FROM_BYTES_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_get_id = self
            .module
            .declare_function(
                MUT_BYTES_GET_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                    ],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_set_id = self
            .module
            .declare_function(
                MUT_BYTES_SET_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![
                        AbiParam::new(pointer_type),
                        AbiParam::new(pointer_type),
                        AbiParam::new(types::I8),
                    ],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_pop_id = self
            .module
            .declare_function(
                MUT_BYTES_POP_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_clear_id = self
            .module
            .declare_function(
                MUT_BYTES_CLEAR_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_reserve_id = self
            .module
            .declare_function(
                MUT_BYTES_RESERVE_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_to_bytes_id = self
            .module
            .declare_function(
                MUT_BYTES_TO_BYTES_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(pointer_type)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let mut_bytes_extend_id = self
            .module
            .declare_function(
                MUT_BYTES_EXTEND_SYMBOL,
                Linkage::Import,
                &Signature {
                    params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
                    returns: vec![AbiParam::new(types::I8)],
                    call_conv,
                },
            )
            .map_err(codegen_error)?;
        let list_cons_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let list_cons_id = self
            .module
            .declare_function(LIST_CONS_SYMBOL, Linkage::Import, &list_cons_signature)
            .map_err(codegen_error)?;
        let list_query_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let list_head_id = self
            .module
            .declare_function(LIST_HEAD_SYMBOL, Linkage::Import, &list_query_signature)
            .map_err(codegen_error)?;
        let list_length_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let list_tail_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let list_tail_id = self
            .module
            .declare_function(LIST_TAIL_SYMBOL, Linkage::Import, &list_tail_signature)
            .map_err(codegen_error)?;
        let list_length_id = self
            .module
            .declare_function(LIST_LENGTH_SYMBOL, Linkage::Import, &list_length_signature)
            .map_err(codegen_error)?;
        let list_reverse_id = self
            .module
            .declare_function(LIST_REVERSE_SYMBOL, Linkage::Import, &list_tail_signature)
            .map_err(codegen_error)?;
        let mut_list_new_signature = Signature {
            params: vec![AbiParam::new(pointer_type), AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let mut_list_new_id = self
            .module
            .declare_function(
                MUT_LIST_NEW_SYMBOL,
                Linkage::Import,
                &mut_list_new_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_push_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_list_push_id = self
            .module
            .declare_function(
                MUT_LIST_PUSH_SYMBOL,
                Linkage::Import,
                &mut_list_push_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_get_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_list_set_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_list_pop_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_list_get_id = self
            .module
            .declare_function(
                MUT_LIST_GET_SYMBOL,
                Linkage::Import,
                &mut_list_get_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_set_id = self
            .module
            .declare_function(
                MUT_LIST_SET_SYMBOL,
                Linkage::Import,
                &mut_list_set_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_pop_id = self
            .module
            .declare_function(
                MUT_LIST_POP_SYMBOL,
                Linkage::Import,
                &mut_list_pop_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_length_id = self
            .module
            .declare_function(
                MUT_LIST_LENGTH_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_capacity_id = self
            .module
            .declare_function(
                MUT_LIST_CAPACITY_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_cursor_new_id = self
            .module
            .declare_function(
                MUT_LIST_CURSOR_NEW_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_cursor_step_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let mut_list_cursor_step_id = self
            .module
            .declare_function(
                MUT_LIST_CURSOR_STEP_SYMBOL,
                Linkage::Import,
                &mut_list_cursor_step_signature,
            )
            .map_err(codegen_error)?;
        let mut_list_to_list_id = self
            .module
            .declare_function(
                MUT_LIST_TO_LIST_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let map_insert_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let map_insert_id = self
            .module
            .declare_function(MAP_INSERT_SYMBOL, Linkage::Import, &map_insert_signature)
            .map_err(codegen_error)?;
        let map_get_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let map_get_id = self
            .module
            .declare_function(MAP_GET_SYMBOL, Linkage::Import, &map_get_signature)
            .map_err(codegen_error)?;
        let map_remove_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let map_remove_id = self
            .module
            .declare_function(MAP_REMOVE_SYMBOL, Linkage::Import, &map_remove_signature)
            .map_err(codegen_error)?;
        let map_contains_key_signature = Signature {
            params: map_remove_signature.params.clone(),
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let map_contains_key_id = self
            .module
            .declare_function(
                MAP_CONTAINS_KEY_SYMBOL,
                Linkage::Import,
                &map_contains_key_signature,
            )
            .map_err(codegen_error)?;
        let map_length_id = self
            .module
            .declare_function(MAP_LENGTH_SYMBOL, Linkage::Import, &list_length_signature)
            .map_err(codegen_error)?;
        let map_cursor_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let map_cursor_new_id = self
            .module
            .declare_function(
                MAP_CURSOR_NEW_SYMBOL,
                Linkage::Import,
                &map_cursor_signature,
            )
            .map_err(codegen_error)?;
        let map_cursor_step_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let map_cursor_step_id = self
            .module
            .declare_function(
                MAP_CURSOR_STEP_SYMBOL,
                Linkage::Import,
                &map_cursor_step_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_new_signature = Signature {
            params: vec![
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
                AbiParam::new(pointer_type),
            ],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let mut_map_new_id = self
            .module
            .declare_function(MUT_MAP_NEW_SYMBOL, Linkage::Import, &mut_map_new_signature)
            .map_err(codegen_error)?;
        let mut_map_insert_signature = Signature {
            params: (0..11).map(|_| AbiParam::new(pointer_type)).collect(),
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_map_insert_id = self
            .module
            .declare_function(
                MUT_MAP_INSERT_SYMBOL,
                Linkage::Import,
                &mut_map_insert_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_get_id = self
            .module
            .declare_function(MUT_MAP_GET_SYMBOL, Linkage::Import, &map_get_signature)
            .map_err(codegen_error)?;
        let mut_map_remove_id = self
            .module
            .declare_function(MUT_MAP_REMOVE_SYMBOL, Linkage::Import, &map_get_signature)
            .map_err(codegen_error)?;
        let mut_map_contains_key_id = self
            .module
            .declare_function(
                MUT_MAP_CONTAINS_KEY_SYMBOL,
                Linkage::Import,
                &map_contains_key_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_length_id = self
            .module
            .declare_function(
                MUT_MAP_LENGTH_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_capacity_id = self
            .module
            .declare_function(
                MUT_MAP_CAPACITY_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_is_empty_signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let mut_map_is_empty_id = self
            .module
            .declare_function(
                MUT_MAP_IS_EMPTY_SYMBOL,
                Linkage::Import,
                &mut_map_is_empty_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_cursor_new_id = self
            .module
            .declare_function(
                MUT_MAP_CURSOR_NEW_SYMBOL,
                Linkage::Import,
                &list_length_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_cursor_step_id = self
            .module
            .declare_function(
                MUT_MAP_CURSOR_STEP_SYMBOL,
                Linkage::Import,
                &map_cursor_step_signature,
            )
            .map_err(codegen_error)?;
        let mut_map_to_list_signature = Signature {
            params: vec![AbiParam::new(pointer_type), AbiParam::new(types::I8)],
            returns: vec![AbiParam::new(pointer_type)],
            call_conv,
        };
        let mut_map_to_list_id = self
            .module
            .declare_function(
                MUT_MAP_TO_LIST_SYMBOL,
                Linkage::Import,
                &mut_map_to_list_signature,
            )
            .map_err(codegen_error)?;
        let mut callback_import = |name, result| {
            let signature = Signature {
                params: vec![AbiParam::new(pointer_type)],
                returns: vec![AbiParam::new(result)],
                call_conv,
            };
            self.module
                .declare_function(name, Linkage::Import, &signature)
                .map_err(codegen_error)
        };
        let callback_function = callback_import(CALLBACK_FUNCTION_SYMBOL, pointer_type)?;
        let callback_context = callback_import(CALLBACK_CONTEXT_SYMBOL, pointer_type)?;
        let callback_failed = callback_import(CALLBACK_FAILED_SYMBOL, types::I8)?;
        let [hasher_new, hasher_word, hasher_string, hasher_finish, hasher_bytes] =
            self.declare_hasher_runtime()?;
        let runtime_call_ids = RuntimeCallIds {
            hasher_new,
            hasher_word,
            hasher_string,
            hasher_bytes,
            hasher_finish,
            callback_function,
            callback_context,
            callback_failed,
            cown_new: cown_new_id,
            cown_acquire: cown_acquire_id,
            cown_acquire_many: cown_acquire_many_id,
            cown_payload: cown_payload_id,
            cown_release: cown_release_id,
            print: print_id,
            println: println_id,
            panic: panic_id,
            allocate: allocate_id,
            closure_allocate: closure_allocate_id,
            dup: dup_id,
            managed_drop: drop_id,
            string_from: string_from_id,
            bytes_from_data: bytes_from_data_id,
            string_len: string_len_id,
            string_c_string_check: string_c_string_check_id,
            string_from_cstr: string_from_cstr_id,
            c_alloc: c_alloc_id,
            c_free: c_free_id,
            show_i64: show_i64_id,
            debug_path: debug_path_id,
            debug_path_status: debug_path_status_id,
            debug_native_id: debug_native_id_id,
            debug_string: debug_string_id,
            debug_bytes: debug_bytes_id,
            debug_duration: debug_duration_id,
            echo: echo_id,
            show_u64: show_u64_id,
            show_f64: show_f64_id,
            show_bool: show_bool_id,
            string_concat: string_concat_id,
            string_eq: string_eq_id,
            string_compare: string_compare_id,
            string_starts_with: string_starts_with_id,
            string_ends_with: string_ends_with_id,
            string_contains: string_contains_id,
            string_scalar_count: string_scalar_count_id,
            string_grapheme_count: string_grapheme_count_id,
            string_is_ascii: string_is_ascii_id,
            string_trim: string_trim_id,
            string_to_upper: string_to_upper_id,
            string_to_lower: string_to_lower_id,
            string_split: string_split_id,
            path_call: path_call_id,
            string_replace: string_replace_id,
            string_get_byte: string_get_byte_id,
            string_slice: string_slice_id,
            parse_i8: parse_i8_id,
            parse_i16: parse_i16_id,
            parse_i64: parse_i64_id,
            parse_u8: parse_u8_id,
            parse_u16: parse_u16_id,
            parse_u32: parse_u32_id,
            parse_u64: parse_u64_id,
            parse_f32: parse_f32_id,
            parse_bool: parse_bool_id,
            parse_i32: parse_i32_id,
            parse_f64: parse_f64_id,
            batch_new: batch_new_id,
            batch_worker: batch_worker_id,
            batch_next: batch_next_id,
            batch_push: batch_push_id,
            batch_finish: batch_finish_id,
            seq_new: seq_new_id,
            seq_push: seq_push_id,
            seq_finish: seq_finish_id,
            list_cons: list_cons_id,
            list_head: list_head_id,
            list_tail: list_tail_id,
            list_length: list_length_id,
            list_reverse: list_reverse_id,
            mut_list_new: mut_list_new_id,
            mut_list_push: mut_list_push_id,
            mut_list_get: mut_list_get_id,
            mut_list_set: mut_list_set_id,
            mut_list_pop: mut_list_pop_id,
            mut_list_length: mut_list_length_id,
            mut_list_capacity: mut_list_capacity_id,
            mut_list_cursor_new: mut_list_cursor_new_id,
            mut_list_cursor_step: mut_list_cursor_step_id,
            mut_list_to_list: mut_list_to_list_id,
            map_insert: map_insert_id,
            map_get: map_get_id,
            map_remove: map_remove_id,
            map_contains_key: map_contains_key_id,
            map_length: map_length_id,
            map_cursor_new: map_cursor_new_id,
            map_cursor_step: map_cursor_step_id,
            mut_map_new: mut_map_new_id,
            mut_map_insert: mut_map_insert_id,
            mut_map_get: mut_map_get_id,
            mut_map_remove: mut_map_remove_id,
            mut_map_contains_key: mut_map_contains_key_id,
            mut_map_length: mut_map_length_id,
            mut_map_capacity: mut_map_capacity_id,
            mut_map_is_empty: mut_map_is_empty_id,
            mut_map_cursor_new: mut_map_cursor_new_id,
            mut_map_cursor_step: mut_map_cursor_step_id,
            mut_map_to_list: mut_map_to_list_id,
            bytes_new: bytes_new_id,
            bytes_from_string: bytes_from_string_id,
            bytes_eq: bytes_eq_id,
            bytes_compare: bytes_compare_id,
            bytes_length: bytes_length_id,
            bytes_cursor_new: bytes_cursor_new_id,
            bytes_cursor_step: bytes_cursor_step_id,
            mut_bytes_new: mut_bytes_new_id,
            mut_bytes_with_capacity: mut_bytes_with_capacity_id,
            mut_bytes_length: mut_bytes_length_id,
            mut_bytes_capacity: mut_bytes_capacity_id,
            mut_bytes_push: mut_bytes_push_id,
            bytes_is_empty: bytes_is_empty_id,
            bytes_get: bytes_get_id,
            bytes_slice: bytes_slice_id,
            bytes_concat: bytes_concat_id,
            bytes_to_string: bytes_to_string_id,
            mut_bytes_from_bytes: mut_bytes_from_bytes_id,
            mut_bytes_get: mut_bytes_get_id,
            mut_bytes_set: mut_bytes_set_id,
            mut_bytes_pop: mut_bytes_pop_id,
            mut_bytes_clear: mut_bytes_clear_id,
            mut_bytes_reserve: mut_bytes_reserve_id,
            mut_bytes_to_bytes: mut_bytes_to_bytes_id,
            mut_bytes_extend: mut_bytes_extend_id,
        };
        Ok(runtime_call_ids)
    }
}
