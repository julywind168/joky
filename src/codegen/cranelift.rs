use std::collections::{BTreeSet, HashMap};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use cranelift_codegen::ir::{types, AbiParam, Signature};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module};

use crate::diagnostic::CodegenError;
use crate::mir::{MirContinuationId, MirFunctionId, MirProgram, MirStatement};
use crate::sema::Type;

pub(super) use self::helpers::collect_mir_constant_strings;
use self::helpers::{
    closure_capture_types, collect_tagged_continuation_cleanup_types, is_main_mir,
    mir_function_symbol, mir_function_type,
};
use self::strings::{collect_function_strings, collect_program_strings};
use super::abi::abi_types;
use super::functions::CompileContext;
use super::helpers::codegen_error;
use crate::mir::machine_entry_blocker;

mod callbacks;
mod foreign;
mod helpers;
mod initialization;
mod pending;
mod program;
mod runtime_ids;
mod strings;

pub(super) fn source_location(span: Option<crate::Span>) -> cranelift_codegen::ir::SourceLoc {
    span.and_then(|span| u32::try_from(span.start()).ok())
        .filter(|offset| *offset != u32::MAX)
        .map(cranelift_codegen::ir::SourceLoc::new)
        .unwrap_or_default()
}

/// Process-wide allocation avoids collisions between independently compiled
/// JIT modules, whose MIR function and continuation ids both start at zero.
static NEXT_CONTINUATION_ENTRY_KEY: AtomicUsize = AtomicUsize::new(1);
/// Cranelift's JIT memory/linker path is process-global even though each
/// backend owns its `JITModule`. Keep JIT compilation/finalization serialized,
/// but release this lock before entering generated code so independent runtime
/// scopes can execute concurrently. AOT `ObjectModule` emission does not use
/// that memory, so it skips this lock and shares only the Cranelift `compile()`
/// job slots in `pending`.
static JIT_COMPILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) struct CraneliftBackend<M: Module = JITModule> {
    pub(super) module: M,
    pub(super) debug_info: Option<super::debug::DebugInfo>,
    pub(super) function_context: FunctionBuilderContext,
    pub(super) next_function: usize,
    pub(super) map_key_adapters:
        HashMap<Type, (cranelift_module::FuncId, cranelift_module::FuncId)>,
    string_literals: Vec<Box<[u8]>>,
    /// Dropped after scopes drain and generated code is unmapped in Drop.
    foreign_libraries: HashMap<String, libloading::Library>,
    continuation_entry_keys: HashMap<(MirFunctionId, MirContinuationId), usize>,
    aot_machine_entries: Vec<(usize, String)>,
    registered_machine_scopes: Vec<joky_runtime::host::RuntimeScope>,
    pending_machine_functions: Vec<pending::PendingMachineFunction>,
}

pub(crate) trait ModuleLifecycle: Module {
    const IS_AOT: bool;

    fn finalize_module(&mut self) -> Result<(), String>;
    fn function_address(&self, id: cranelift_module::FuncId) -> *const u8;
    fn materialize_string(
        &mut self,
        function: &mut cranelift_codegen::ir::Function,
        pointer: *const u8,
        length: usize,
    ) -> crate::codegen::functions::StringValue;
}

impl ModuleLifecycle for cranelift_object::ObjectModule {
    const IS_AOT: bool = true;

    fn finalize_module(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn function_address(&self, _id: cranelift_module::FuncId) -> *const u8 {
        std::ptr::null()
    }

    fn materialize_string(
        &mut self,
        function: &mut cranelift_codegen::ir::Function,
        pointer: *const u8,
        length: usize,
    ) -> crate::codegen::functions::StringValue {
        use cranelift_module::{DataDescription, FuncOrDataId, Init, Linkage};
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        let mut name = String::from("joky_literal_");
        for byte in bytes {
            name.push_str(&format!("{byte:02x}"));
        }
        let data_id = match self.get_name(&name) {
            Some(FuncOrDataId::Data(id)) => id,
            _ => {
                let id = self
                    .declare_data(&name, Linkage::Local, false, false)
                    .expect("declare AOT string literal");
                let mut data = DataDescription::new();
                data.init = Init::Bytes {
                    contents: bytes.to_vec().into_boxed_slice(),
                };
                self.define_data(id, &data)
                    .expect("define AOT string literal");
                id
            }
        };
        crate::codegen::functions::StringValue::Global(self.declare_data_in_func(data_id, function))
    }
}

impl ModuleLifecycle for JITModule {
    const IS_AOT: bool = false;

    fn finalize_module(&mut self) -> Result<(), String> {
        self.finalize_definitions()
            .map_err(|error| error.to_string())
    }

    fn function_address(&self, id: cranelift_module::FuncId) -> *const u8 {
        self.get_finalized_function(id)
    }

    fn materialize_string(
        &mut self,
        _function: &mut cranelift_codegen::ir::Function,
        pointer: *const u8,
        _length: usize,
    ) -> crate::codegen::functions::StringValue {
        crate::codegen::functions::StringValue::Pointer(pointer)
    }
}

#[derive(Clone, Copy)]
pub(super) struct TaskRuntimeIds {
    pub(super) group_new: cranelift_module::FuncId,
    pub(super) group_free: cranelift_module::FuncId,
    pub(super) group_cancel_free: cranelift_module::FuncId,
    pub(super) spawn: cranelift_module::FuncId,
    pub(super) spawn_heap: cranelift_module::FuncId,
    pub(super) join: cranelift_module::FuncId,
    pub(super) cancel: cranelift_module::FuncId,
    pub(super) race: cranelift_module::FuncId,
    pub(super) claim_result: cranelift_module::FuncId,
    pub(super) result_pointer: cranelift_module::FuncId,
    pub(super) is_cancelled: cranelift_module::FuncId,
    pub(super) mark_cancelled_result: cranelift_module::FuncId,
    pub(super) abort: cranelift_module::FuncId,
    pub(super) abort_payload: cranelift_module::FuncId,
    pub(super) failure_operation: cranelift_module::FuncId,
    pub(super) failure_payload: cranelift_module::FuncId,
    pub(super) failure_claim: cranelift_module::FuncId,
    pub(super) failure_rethrow: cranelift_module::FuncId,
    pub(super) handler_frame_new: cranelift_module::FuncId,
    pub(super) handler_frame_enter: cranelift_module::FuncId,
    pub(super) handler_frame_exit: cranelift_module::FuncId,
    pub(super) handler_frame_free: cranelift_module::FuncId,
    pub(super) handler_frame_begin_resumption_with_payload: cranelift_module::FuncId,
    pub(super) handler_frame_begin_resumption_with_payload_env: cranelift_module::FuncId,
    pub(super) handler_frame_free_resumption: cranelift_module::FuncId,
    pub(super) handler_frame_set_resumption_string: cranelift_module::FuncId,
    pub(super) handler_frame_set_resumption_managed_payload: cranelift_module::FuncId,
    pub(super) handler_frame_resumption_payload_copy: cranelift_module::FuncId,
    pub(super) handler_frame_resumption_payload_consume: cranelift_module::FuncId,
    pub(super) handler_frame_set_resumption_payload_thunk: cranelift_module::FuncId,
    pub(super) handler_frame_set_resumption_thunk: cranelift_module::FuncId,
    pub(super) handler_frame_set_resumption_thunk_with_result_env: cranelift_module::FuncId,
    pub(super) handler_frame_dispatch_resumption: cranelift_module::FuncId,
    pub(super) handler_frame_static_thunk: cranelift_module::FuncId,
    pub(super) handler_frame_forward_thunk: cranelift_module::FuncId,
    pub(super) handler_frame_transform_i64_thunk: cranelift_module::FuncId,
}

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(super) fn continuation_entry_key(
        &mut self,
        function: MirFunctionId,
        continuation: MirContinuationId,
    ) -> usize {
        if M::IS_AOT {
            return self.continuation_entry_keys[&(function, continuation)];
        }
        *self
            .continuation_entry_keys
            .entry((function, continuation))
            .or_insert_with(|| {
                NEXT_CONTINUATION_ENTRY_KEY
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |key| {
                        key.checked_add(1)
                    })
                    .expect("continuation entry key space exhausted")
            })
    }
}

impl CraneliftBackend<cranelift_object::ObjectModule> {
    pub(crate) fn new_object(
        target: crate::AotTarget,
        release: bool,
    ) -> Result<Self, CodegenError> {
        let module = crate::codegen::object::object_module(target, release)
            .map_err(|message| CodegenError::RuntimeError { message })?;
        Ok(Self {
            module,
            debug_info: None,
            function_context: FunctionBuilderContext::new(),
            next_function: 0,
            string_literals: Vec::new(),
            foreign_libraries: HashMap::new(),
            continuation_entry_keys: HashMap::new(),
            map_key_adapters: HashMap::new(),
            aot_machine_entries: Vec::new(),
            registered_machine_scopes: Vec::new(),
            pending_machine_functions: Vec::new(),
        })
    }

    pub(crate) fn finish_object(mut self) -> Result<Vec<u8>, String> {
        let debug_info = self.debug_info.take();
        let address_size = self.module.target_config().pointer_bytes();
        let endianness = self.module.isa().endianness();
        let module = unsafe { std::ptr::read(&self.module) };
        std::mem::forget(self);
        let mut product = module.finish();
        if let Some(debug_info) = debug_info {
            debug_info.emit(&mut product, address_size, endianness)?;
        }
        product.emit().map_err(|error| error.to_string())
    }

    pub(crate) fn aot_machine_entries(&self) -> &[(usize, String)] {
        &self.aot_machine_entries
    }
}

impl<M: Module> Drop for CraneliftBackend<M> {
    fn drop(&mut self) {
        for scope in self.registered_machine_scopes.drain(..) {
            scope.close_and_wait();
            scope.discard_root_failure();
        }
        // Module-specific executable memory reclamation is handled by the
        // backend owner. The generic compiler layer only retires runtime
        // registrations before dropping the module.
    }
}

#[cfg(test)]
mod tests;
