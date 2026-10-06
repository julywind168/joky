//! Protect native activations that compute floats or call external code.
//! Instrument the completed CFG so success, failure and Pending share the same
//! exit rule. The saved register token is an SSA value, never a CPS frame field.
use cranelift_codegen::{
    cursor::{Cursor, FuncCursor},
    ir::{Function, Inst, InstBuilder, InstructionData, Opcode},
};
use cranelift_module::{FuncId, Module};
use std::collections::HashMap;

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
        } else if external_call(cursor.func, inst) {
            // This includes indirect and CPS calls, as well as runtime helpers
            // that may invoke a host poll, stop, text release or Drop hook.
            cursor.goto_after_inst(inst);
            cursor.ins().call(reset, &[]);
        }
    }
    true
}
