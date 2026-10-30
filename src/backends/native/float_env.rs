//! Protect native activations that compute floats or call external code.
//! Instrument the completed CFG so success, failure and Pending share the same
//! exit rule. The saved register token is an SSA value, never a CPS frame field.
use cranelift_codegen::{
    cursor::{Cursor, FuncCursor},
    ir::{ExternalName, Function, Inst, InstBuilder, InstructionData, Opcode},
};
use cranelift_module::{FuncId, Module};
use std::collections::{HashMap, HashSet};

/// A closed set of runtime declarations audited to preserve the thread's FP
/// control and status registers on every returning path. Capture the IDs before
/// user declarations are added: a source name or an indirect target is no proof.
pub(super) struct Effects {
    preserving: HashSet<u32>,
}
impl Effects {
    pub(super) fn runtime_helpers(helpers: &HashMap<String, FuncId>) -> Self {
        // buffer.rs: integer header/cursor operations and checked addresses,
        // never element arithmetic, ownership cleanup or user callbacks. Bounds
        // failures only construct/record the ordinary owned Error metadata.
        // In particular Store/Truncate/Replace/Release may run cleanup hooks and
        // must not be admitted along with the address-only Load operations.
        let names = [
            "__joky_buffer_len",
            "__joky_buffer_capacity",
            "__joky_buffer_capacity_limit",
            "__joky_buffer_load",
            "__joky_buffer_element_load",
            "__joky_buffer_initialize_next",
            "__joky_buffer_commit_next",
            "__joky_buffer_take_last",
            "__joky_buffer_swap_elements",
            "__joky_buffer_iter_advance",
            "__joky_buffer_iter_current",
            "__joky_buffer_pair_advance",
            "__joky_buffer_pair_project",
            "__joky_buffer_entry_key",
            "__joky_buffer_entry_value",
            // cps.rs / error.rs: slot address and TLS/atomic state queries.
            // The cancellation path records an Error; it does not run cleanup.
            "__joky_cps_slots",
            "joky_runtime_error_code",
            "__joky_task_checkpoint_v1",
        ];
        Self {
            preserving: names.iter().map(|name| helpers[*name].as_u32()).collect(),
        }
    }

    fn preserves(&self, function: &Function, inst: Inst) -> bool {
        let InstructionData::Call { func_ref, .. } = &function.dfg.insts[inst] else {
            return false;
        };
        let ExternalName::User(name) = function.dfg.ext_funcs[*func_ref].name else {
            return false;
        };
        // cranelift_module declares function IDs in namespace 0 (data uses 1).
        let name = &function.params.user_named_funcs()[name];
        name.namespace == 0 && self.preserving.contains(&name.index)
    }
}

fn external_call(function: &Function, inst: Inst) -> bool {
    match &function.dfg.insts[inst] {
        InstructionData::Call { func_ref, .. } => !function.dfg.ext_funcs[*func_ref].colocated,
        data => data.opcode().is_call(),
    }
}

pub(super) fn instrument<M: Module>(
    module: &mut M,
    function: &mut Function,
    helpers: &HashMap<String, FuncId>,
    effects: &Effects,
) -> bool {
    let instructions: Vec<_> = function
        .layout
        .blocks()
        .flat_map(|block| function.layout.block_insts(block))
        .collect();
    let needs_guard = instructions.iter().any(|&inst| {
        external_call(function, inst)
            || function
                .dfg
                .inst_args(inst)
                .iter()
                .chain(function.dfg.inst_results(inst))
                .any(|&value| function.dfg.value_type(value).is_float())
    });
    if !needs_guard {
        // Pure integer code, including calls to other generated functions,
        // still links without the runtime.
        return false;
    }
    let enter = module.declare_func_in_func(helpers["__joky_float_env_enter"], function);
    let reset = module.declare_func_in_func(helpers["__joky_float_env_reset"], function);
    let restore = module.declare_func_in_func(helpers["__joky_float_env_restore"], function);
    let entry = function.layout.entry_block().expect("native entry block");
    let mut cursor = FuncCursor::new(function).at_first_inst(entry);
    let call = cursor.ins().call(enter, &[]);
    let saved = cursor.func.dfg.inst_results(call)[0];
    for inst in instructions {
        let opcode = cursor.func.dfg.insts[inst].opcode();
        if opcode == Opcode::Return {
            cursor.goto_inst(inst);
            cursor.ins().call(restore, &[saved]);
        } else if external_call(cursor.func, inst) && !effects.preserves(cursor.func, inst) {
            // This includes indirect and CPS calls, as well as runtime helpers
            // that may invoke a host poll, stop, text release or Drop hook.
            cursor.goto_after_inst(inst);
            cursor.ins().call(reset, &[]);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::float_env::{self, testing, Guard};
    use cranelift_codegen::ir::{types, AbiParam};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_jit::{JITBuilder, JITModule};
    use cranelift_module::Linkage;
    use std::cell::Cell;

    thread_local! { static RESETS: Cell<usize> = const { Cell::new(0) }; }
    extern "C" fn counted_reset() {
        RESETS.with(|n| n.set(n.get() + 1));
        float_env::reset();
    }
    extern "C" fn dirty() {
        testing::hostile();
    }

    #[test]
    fn float_environment_elides_audited_calls_but_resets_host_and_indirect_calls() {
        let mut builder = JITBuilder::new(cranelift_module::default_libcall_names()).unwrap();
        for (name, pointer) in float_env::symbols() {
            builder.symbol(name, pointer);
        }
        builder.symbol("__joky_float_env_reset", counted_reset as *const u8);
        builder.symbol(
            "joky_runtime_error_code",
            crate::runtime::error::joky_runtime_error_code as *const u8,
        );
        // A private-looking name is not evidence that an arbitrary host preserves FP.
        builder.symbol("__joky_buffer_untrusted_probe", dirty as *const u8);
        let mut module = JITModule::new(builder);
        let helpers = super::super::lower::declare_helpers(&mut module).unwrap();
        let effects = Effects::runtime_helpers(&helpers);
        let host_sig = module.make_signature();
        let host = module
            .declare_function("__joky_buffer_untrusted_probe", Linkage::Import, &host_sig)
            .unwrap();
        let mut ctx = module.make_context();
        ctx.func.signature.params = vec![AbiParam::new(types::F64); 2];
        ctx.func.signature.returns = vec![AbiParam::new(types::F64)];
        let function = module
            .declare_function("probe", Linkage::Export, &ctx.func.signature)
            .unwrap();
        let error = module.declare_func_in_func(helpers["joky_runtime_error_code"], &mut ctx.func);
        let host = module.declare_func_in_func(host, &mut ctx.func);
        let indirect = ctx.func.import_signature(host_sig);
        let mut frontend = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let a = b.block_params(entry)[0];
        let c = b.block_params(entry)[1];
        b.ins().call(error, &[]);
        b.ins().call(host, &[]);
        b.ins().call(error, &[]);
        let target = b.ins().iconst(
            module.target_config().pointer_type(),
            dirty as *const () as i64,
        );
        b.ins().call_indirect(indirect, target, &[]);
        b.ins().call(error, &[]);
        let sum = b.ins().fadd(a, c);
        b.ins().return_(&[sum]);
        b.seal_all_blocks();
        b.finalize(module.target_config());
        assert!(instrument(&mut module, &mut ctx.func, &helpers, &effects));
        module.define_function(function, &mut ctx).unwrap();
        module.finalize_definitions().unwrap();
        let call: unsafe extern "C" fn(f64, f64) -> f64 =
            unsafe { std::mem::transmute(module.get_finalized_function(function)) };
        let _restore = Guard::enter();
        testing::hostile();
        let saved = testing::state();
        RESETS.with(|n| n.set(0));
        let value = unsafe { call(1.0, f64::from_bits(0x3ca0000000000000)) };
        assert_eq!(value.to_bits(), 1.0f64.to_bits());
        assert_eq!(
            testing::state(),
            saved,
            "restore the caller's complete environment"
        );
        RESETS.with(|n| assert_eq!(n.get(), 2, "only host and indirect returns reset"));
        unsafe { module.free_memory() };
    }
}
