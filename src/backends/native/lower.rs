//! Cranelift lowering of the shared typed IR.
use crate::core::ir::{Constant, FunctionInfo, FunctionPlan, Op, ScopeBranch, Slot};
use crate::runtime::cps::{CpsFrame, DONE, INVALID_STATE};
use crate::{
    parser::ast::{BinOp, Type, UnaryOp},
    Error, ErrorCode, ErrorPhase,
};
use cranelift_codegen::ir::{
    condcodes::IntCC, types, AbiParam, FuncRef, GlobalValue, InstBuilder, MemFlagsData, Signature,
    Value,
};
use cranelift_frontend::{FunctionBuilder, Switch, Variable};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

fn native_type(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int32 => Some(types::I32),
        Type::Bool => Some(types::I8),
        Type::Unit => None,
        Type::Named(_) => Some(types::I64),
        Type::String => Some(if cfg!(target_pointer_width = "64") {
            types::I64
        } else {
            types::I32
        }),
    }
}

fn lower_literal(
    builder: &mut FunctionBuilder,
    literal: &Constant,
    strings: &HashMap<String, GlobalValue>,
) -> Value {
    match literal {
        Constant::Int32(n) => builder.ins().iconst(types::I32, *n as i64),
        Constant::Bool(b) => builder.ins().iconst(types::I8, *b as i64),
        Constant::String(text) => builder
            .ins()
            .symbol_value(native_type(&Type::String).unwrap(), strings[text]),
    }
}

fn lower_unary(builder: &mut FunctionBuilder, op: &UnaryOp, operand: Value) -> Value {
    match op {
        UnaryOp::Neg => builder.ins().ineg(operand),
        UnaryOp::Not => builder.ins().icmp_imm_u(IntCC::Equal, operand, 0),
    }
}

fn lower_binary(builder: &mut FunctionBuilder, op: &BinOp, lhs: Value, rhs: Value) -> Value {
    match op {
        BinOp::Add => builder.ins().iadd(lhs, rhs),
        BinOp::Sub => builder.ins().isub(lhs, rhs),
        BinOp::Mul => builder.ins().imul(lhs, rhs),
        BinOp::Div => builder.ins().sdiv(lhs, rhs),
        BinOp::Eq => builder.ins().icmp(IntCC::Equal, lhs, rhs),
        BinOp::Ne => builder.ins().icmp(IntCC::NotEqual, lhs, rhs),
        BinOp::Lt => builder.ins().icmp(IntCC::SignedLessThan, lhs, rhs),
        BinOp::Le => builder.ins().icmp(IntCC::SignedLessThanOrEqual, lhs, rhs),
        BinOp::Gt => builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs),
        BinOp::Ge => builder
            .ins()
            .icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs),
    }
}

pub fn signature<M: Module>(module: &M, info: &FunctionInfo) -> Signature {
    let mut sig = module.make_signature();
    if info.suspends {
        sig.params
            .push(AbiParam::new(module.target_config().pointer_type()));
        sig.returns.push(AbiParam::new(types::I32));
    } else {
        for ty in &info.signature.params {
            if let Some(ty) = native_type(ty) {
                sig.params.push(AbiParam::new(ty));
            }
        }
        if let Some(ty) = native_type(&info.signature.ret) {
            sig.returns.push(AbiParam::new(ty));
        }
    }
    sig
}

pub fn declare_helpers<M: Module>(module: &mut M) -> Result<HashMap<String, FuncId>, Error> {
    let ptr = module.target_config().pointer_type();
    let mut ids = HashMap::new();
    for (name, params, returns) in [
        ("__joky_cps_slots", vec![ptr], vec![ptr]),
        ("__joky_cps_child", vec![ptr, ptr], vec![ptr]),
        ("__joky_cps_release_child", vec![ptr], vec![]),
        ("__joky_cps_child_at", vec![ptr, ptr, ptr], vec![ptr]),
        ("__joky_cps_release_children", vec![ptr], vec![]),
    ] {
        let mut sig = module.make_signature();
        sig.params = params.into_iter().map(AbiParam::new).collect();
        sig.returns = returns.into_iter().map(AbiParam::new).collect();
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| {
                Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, e.to_string())
            })?;
        ids.insert(name.into(), id);
    }
    Ok(ids)
}

fn load(builder: &mut FunctionBuilder, slots: Value, slot: &Slot) -> Value {
    let value = builder.ins().load(
        types::I64,
        MemFlagsData::new(),
        slots,
        (slot.index * 8) as i32,
    );
    let ty = native_type(&slot.ty).unwrap();
    if ty == types::I64 {
        value
    } else {
        builder.ins().ireduce(ty, value)
    }
}

fn wide(builder: &mut FunctionBuilder, value: Value) -> Value {
    if builder.func.dfg.value_type(value) == types::I64 {
        value
    } else {
        builder.ins().uextend(types::I64, value)
    }
}

fn store(builder: &mut FunctionBuilder, slots: Value, slot: &Slot, value: Value) {
    let value = wide(builder, value);
    builder
        .ins()
        .store(MemFlagsData::new(), value, slots, (slot.index * 8) as i32);
}

fn helper(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    name: &str,
    args: &[Value],
) -> Value {
    let call = builder.ins().call(refs[name], args);
    builder.inst_results(call)[0]
}

fn child_at(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Value,
    index: usize,
    slot_count: usize,
    ptr_ty: types::Type,
) -> Value {
    let index = builder.ins().iconst(ptr_ty, index as i64);
    let count = builder.ins().iconst(ptr_ty, slot_count as i64);
    helper(builder, refs, "__joky_cps_child_at", &[frame, index, count])
}

struct ScopeContext<'a> {
    branches: &'a [ScopeBranch],
    scope_state: u32,
    functions: &'a HashMap<String, FunctionInfo>,
    refs: &'a HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    frame: Value,
    slots: Value,
    state_offset: i32,
    states: &'a [cranelift_codegen::ir::Block],
}

fn lower_scope(builder: &mut FunctionBuilder, context: ScopeContext<'_>) {
    let ScopeContext {
        branches,
        scope_state,
        functions,
        refs,
        ptr_ty,
        frame,
        slots,
        state_offset,
        states,
    } = context;
    // Allocate every child and evaluate all branch arguments once. The child
    // frames remain in the parent until every branch has completed.
    for (index, branch) in branches.iter().enumerate() {
        let child = child_at(
            builder,
            refs,
            frame,
            index,
            functions[&branch.name].frame_slots,
            ptr_ty,
        );
        let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
        for (slot_index, arg) in branch.args.iter().enumerate() {
            if let Some(arg) = arg {
                let value = load(builder, slots, arg);
                store(
                    builder,
                    child_slots,
                    &Slot {
                        index: slot_index,
                        ty: arg.ty.clone(),
                    },
                    value,
                );
            }
        }
        let pending = builder.ins().iconst(types::I8, 0);
        store(builder, slots, &branch.done, pending);
    }
    let saved = builder.ins().iconst(types::I32, scope_state as i64);
    builder
        .ins()
        .store(MemFlagsData::new(), saved, frame, state_offset);
    builder.ins().jump(states[scope_state as usize], &[]);
    builder.switch_to_block(states[scope_state as usize]);

    // Poll every incomplete child on every parent poll. A completed child is
    // skipped using the persistent done flag, so native callbacks do not need
    // to expose the generated DONE state themselves.
    for (index, branch) in branches.iter().enumerate() {
        let poll = builder.create_block();
        let after = builder.create_block();
        let complete = builder.create_block();
        let status_check = builder.create_block();
        let failure = builder.create_block();
        let done = load(builder, slots, &branch.done);
        let one = builder.ins().iconst(types::I8, 1);
        let is_done = builder.ins().icmp(IntCC::Equal, done, one);
        builder.ins().brif(is_done, after, &[], poll, &[]);

        builder.switch_to_block(poll);
        let child = child_at(
            builder,
            refs,
            frame,
            index,
            functions[&branch.name].frame_slots,
            ptr_ty,
        );
        let call = builder.ins().call(refs[&branch.name], &[child]);
        let status = builder.inst_results(call)[0];
        let zero = builder.ins().iconst(types::I32, 0);
        let completed = builder.ins().icmp(IntCC::Equal, status, zero);
        builder
            .ins()
            .brif(completed, complete, &[], status_check, &[]);

        builder.switch_to_block(status_check);
        let pending = builder.ins().iconst(types::I32, 1);
        let is_pending = builder.ins().icmp(IntCC::Equal, status, pending);
        builder.ins().brif(is_pending, after, &[], failure, &[]);

        builder.switch_to_block(failure);
        builder
            .ins()
            .call(refs["__joky_cps_release_children"], &[frame]);
        builder.ins().return_(&[status]);

        builder.switch_to_block(complete);
        let one = builder.ins().iconst(types::I8, 1);
        store(builder, slots, &branch.done, one);
        builder.ins().jump(after, &[]);

        builder.switch_to_block(after);
    }

    let pending = builder.create_block();
    let continuation = builder.create_block();
    for branch in branches {
        let next = builder.create_block();
        let done = load(builder, slots, &branch.done);
        let one = builder.ins().iconst(types::I8, 1);
        let is_done = builder.ins().icmp(IntCC::Equal, done, one);
        builder.ins().brif(is_done, next, &[], pending, &[]);
        builder.switch_to_block(next);
    }
    builder
        .ins()
        .call(refs["__joky_cps_release_children"], &[frame]);
    builder.ins().jump(continuation, &[]);
    builder.switch_to_block(pending);
    let pending_status = builder.ins().iconst(types::I32, 1);
    builder.ins().return_(&[pending_status]);
    builder.switch_to_block(continuation);
}

fn lower_cps(
    builder: &mut FunctionBuilder,
    plan: &FunctionPlan,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    strings: &HashMap<String, GlobalValue>,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let frame = builder.block_params(entry)[0];
    let slots = helper(builder, refs, "__joky_cps_slots", &[frame]);
    let state_offset = std::mem::offset_of!(CpsFrame, state) as i32;
    let result_offset = std::mem::offset_of!(CpsFrame, result) as i32;
    let state = builder
        .ins()
        .load(types::I32, MemFlagsData::new(), frame, state_offset);
    let states: Vec<_> = (0..plan.state_count)
        .map(|_| builder.create_block())
        .collect();
    let invalid = builder.create_block();
    let mut switch = Switch::new();
    for (index, block) in states.iter().enumerate() {
        switch.set_entry(index as u128, *block);
    }
    switch.emit(builder, state, invalid);
    builder.switch_to_block(invalid);
    let error = builder.ins().iconst(types::I32, INVALID_STATE as i64);
    builder.ins().return_(&[error]);
    builder.switch_to_block(states[0]);

    let labels: Vec<_> = (0..plan.label_count)
        .map(|_| builder.create_block())
        .collect();
    let mut terminated = false;

    for op in &plan.ops {
        match op {
            Op::Copy(dst, src) => {
                let value = load(builder, slots, src);
                store(builder, slots, dst, value);
            }
            Op::Label(label) => {
                if !terminated {
                    builder.ins().jump(labels[*label], &[]);
                }
                builder.switch_to_block(labels[*label]);
                terminated = false;
            }
            Op::Jump(label) => {
                builder.ins().jump(labels[*label], &[]);
                terminated = true;
            }
            Op::Branch {
                condition,
                then_label,
                else_label,
            } => {
                let test = load(builder, slots, condition);
                builder
                    .ins()
                    .brif(test, labels[*then_label], &[], labels[*else_label], &[]);
                terminated = true;
            }
            Op::Literal(dst, literal) => {
                let value = lower_literal(builder, literal, strings);
                store(builder, slots, dst, value);
            }
            Op::FunctionRef(dst, name) => {
                let value = builder.ins().func_addr(ptr_ty, refs[name]);
                store(builder, slots, dst, value);
            }
            Op::Scope { branches, state } => {
                lower_scope(
                    builder,
                    ScopeContext {
                        branches,
                        scope_state: *state,
                        functions,
                        refs,
                        ptr_ty,
                        frame,
                        slots,
                        state_offset,
                        states: &states,
                    },
                );
            }
            Op::Binary(dst, op, lhs, rhs) => {
                let lhs = load(builder, slots, lhs);
                let rhs = load(builder, slots, rhs);
                let value = lower_binary(builder, op, lhs, rhs);
                store(builder, slots, dst, value);
            }
            Op::Unary(dst, op, operand) => {
                let operand = load(builder, slots, operand);
                let value = lower_unary(builder, op, operand);
                store(builder, slots, dst, value);
            }
            Op::Call {
                name,
                args,
                result,
                state: None,
            } => {
                let args: Vec<_> = args
                    .iter()
                    .flatten()
                    .map(|s| load(builder, slots, s))
                    .collect();
                let call = builder.ins().call(refs[name], &args);
                if let Some(dst) = result {
                    let value = builder.inst_results(call)[0];
                    store(builder, slots, dst, value);
                }
            }
            Op::Call {
                name,
                args,
                result,
                state: Some(state),
            } => {
                let count = builder
                    .ins()
                    .iconst(ptr_ty, functions[name].frame_slots as i64);
                let child = helper(builder, refs, "__joky_cps_child", &[frame, count]);
                let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
                for (index, arg) in args.iter().enumerate() {
                    if let Some(arg) = arg {
                        let value = load(builder, slots, arg);
                        store(
                            builder,
                            child_slots,
                            &Slot {
                                index,
                                ty: arg.ty.clone(),
                            },
                            value,
                        );
                    }
                }
                let saved = builder.ins().iconst(types::I32, *state as i64);
                builder
                    .ins()
                    .store(MemFlagsData::new(), saved, frame, state_offset);
                builder.ins().jump(states[*state as usize], &[]);
                builder.switch_to_block(states[*state as usize]);
                // Re-enter only the child poll. Argument evaluation and side
                // effects before this point must not run again on resumption.
                let count = builder
                    .ins()
                    .iconst(ptr_ty, functions[name].frame_slots as i64);
                let child = helper(builder, refs, "__joky_cps_child", &[frame, count]);
                let call = builder.ins().call(refs[name], &[child]);
                let status = builder.inst_results(call)[0];
                let pending = builder.create_block();
                let ready = builder.create_block();
                let failure = builder.create_block();
                let zero = builder.ins().iconst(types::I32, 0);
                let completed = builder.ins().icmp(IntCC::Equal, status, zero);
                let check_pending = builder.create_block();
                builder
                    .ins()
                    .brif(completed, ready, &[], check_pending, &[]);
                builder.switch_to_block(check_pending);
                let one = builder.ins().iconst(types::I32, 1);
                let is_pending = builder.ins().icmp(IntCC::Equal, status, one);
                builder.ins().brif(is_pending, pending, &[], failure, &[]);
                builder.switch_to_block(pending);
                builder.ins().return_(&[status]);
                builder.switch_to_block(failure);
                builder.ins().return_(&[status]);
                builder.switch_to_block(ready);
                if let Some(dst) = result {
                    let value =
                        builder
                            .ins()
                            .load(types::I64, MemFlagsData::new(), child, result_offset);
                    builder
                        .ins()
                        .store(MemFlagsData::new(), value, slots, (dst.index * 8) as i32);
                }
                builder
                    .ins()
                    .call(refs["__joky_cps_release_child"], &[frame]);
            }
        }
    }
    if let Some(result) = &plan.result {
        let value = load(builder, slots, result);
        let value = wide(builder, value);
        builder
            .ins()
            .store(MemFlagsData::new(), value, frame, result_offset);
    }
    let done = builder.ins().iconst(types::I32, DONE as i64);
    builder
        .ins()
        .store(MemFlagsData::new(), done, frame, state_offset);
    let status = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

/// Cranelift's variable builder inserts SSA block parameters for loop backedges.
fn define_slot(
    builder: &mut FunctionBuilder,
    variables: &mut [Option<Variable>],
    slot: &Slot,
    value: Value,
) {
    let variable = *variables[slot.index]
        .get_or_insert_with(|| builder.declare_var(native_type(&slot.ty).unwrap()));
    builder.def_var(variable, value);
}

/// Direct functions keep IR values in SSA rather than allocating CPS frames.
fn lower_direct(
    builder: &mut FunctionBuilder,
    info: &FunctionInfo,
    plan: &FunctionPlan,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    strings: &HashMap<String, GlobalValue>,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let mut variables = vec![None; plan.slot_count];
    let mut parameters = builder.block_params(entry).to_vec().into_iter();
    for (index, ty) in info.signature.params.iter().enumerate() {
        if *ty != Type::Unit {
            define_slot(
                builder,
                &mut variables,
                &Slot {
                    index,
                    ty: ty.clone(),
                },
                parameters.next().unwrap(),
            );
        }
    }
    let labels: Vec<_> = (0..plan.label_count)
        .map(|_| builder.create_block())
        .collect();
    let mut terminated = false;
    for op in &plan.ops {
        match op {
            Op::Copy(dst, src) => {
                let value = builder.use_var(variables[src.index].unwrap());
                define_slot(builder, &mut variables, dst, value);
            }
            Op::Label(label) => {
                if !terminated {
                    builder.ins().jump(labels[*label], &[]);
                }
                builder.switch_to_block(labels[*label]);
                terminated = false;
            }
            Op::Jump(label) => {
                builder.ins().jump(labels[*label], &[]);
                terminated = true;
            }
            Op::Branch {
                condition,
                then_label,
                else_label,
            } => {
                let test = builder.use_var(variables[condition.index].unwrap());
                builder
                    .ins()
                    .brif(test, labels[*then_label], &[], labels[*else_label], &[]);
                terminated = true;
            }
            Op::Literal(dst, literal) => {
                let value = lower_literal(builder, literal, strings);
                define_slot(builder, &mut variables, dst, value);
            }
            Op::FunctionRef(dst, name) => {
                let value = builder.ins().func_addr(ptr_ty, refs[name]);
                define_slot(builder, &mut variables, dst, value);
            }
            Op::Binary(dst, op, lhs, rhs) => {
                let lhs = builder.use_var(variables[lhs.index].unwrap());
                let rhs = builder.use_var(variables[rhs.index].unwrap());
                let value = lower_binary(builder, op, lhs, rhs);
                define_slot(builder, &mut variables, dst, value);
            }
            Op::Unary(dst, op, operand) => {
                let operand = builder.use_var(variables[operand.index].unwrap());
                let value = lower_unary(builder, op, operand);
                define_slot(builder, &mut variables, dst, value);
            }
            Op::Call {
                name,
                args,
                result,
                state,
            } => {
                debug_assert!(state.is_none());
                let args: Vec<_> = args
                    .iter()
                    .flatten()
                    .map(|slot| builder.use_var(variables[slot.index].unwrap()))
                    .collect();
                let call = builder.ins().call(refs[name], &args);
                if let Some(dst) = result {
                    let value = builder.inst_results(call)[0];
                    define_slot(builder, &mut variables, dst, value);
                }
            }
            Op::Scope { .. } => unreachable!("scope is only valid in suspending functions"),
        }
    }
    let result: Vec<_> = plan
        .result
        .iter()
        .map(|slot| builder.use_var(variables[slot.index].unwrap()))
        .collect();
    builder.ins().return_(&result);
    builder.seal_all_blocks();
}

pub(super) fn lower(
    builder: &mut FunctionBuilder,
    info: &FunctionInfo,
    plan: &FunctionPlan,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    strings: &HashMap<String, GlobalValue>,
) {
    if info.suspends {
        lower_cps(builder, plan, functions, refs, ptr_ty, strings);
    } else {
        lower_direct(builder, info, plan, refs, ptr_ty, strings);
    }
}
