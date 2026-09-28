//! Cranelift lowering of the shared typed IR.
use crate::core::ir::{Constant, FunctionInfo, FunctionPlan, Op, ScopeBranch, Slot};
use crate::runtime::cps::{CpsFrame, DONE, FAILED, INVALID_STATE, RUNTIME_ERROR};
use crate::{
    parser::ast::{BinOp, Type, UnaryOp},
    Error, ErrorCode, ErrorPhase,
};
use cranelift_codegen::ir::{
    condcodes::IntCC, types, AbiParam, FuncRef, GlobalValue, InstBuilder, MemFlagsData, Signature,
    StackSlotData, StackSlotKind, Value,
};
use cranelift_frontend::{FunctionBuilder, Switch, Variable};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

fn native_type(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int32 | Type::Int64 | Type::UInt32 | Type::UInt64 => {
            Some(if ty.integer().unwrap().bits() == 32 {
                types::I32
            } else {
                types::I64
            })
        }
        Type::Bool => Some(types::I8),
        Type::Unit | Type::Record(_) | Type::Enum(_) | Type::Tuple(_) => None,
        Type::Named(_) => Some(types::I64),
        Type::Parameter(_) | Type::Applied(_) => {
            unreachable!("generic parameter reached native lowering")
        }
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
        Constant::Int64(n) => builder.ins().iconst(types::I64, *n),
        Constant::UInt32(n) => builder.ins().iconst(types::I32, i64::from(*n)),
        Constant::UInt64(n) => builder.ins().iconst(types::I64, *n as i64),
        Constant::Bool(b) => builder.ins().iconst(types::I8, *b as i64),
        Constant::String(text) => builder
            .ins()
            .symbol_value(native_type(&Type::String).unwrap(), strings[text]),
    }
}

fn lower_unary(builder: &mut FunctionBuilder, op: &UnaryOp, operand: Value) -> Value {
    match op {
        UnaryOp::Neg => builder.ins().ineg(operand),
        UnaryOp::BitNot => builder.ins().bnot(operand),
        UnaryOp::Not => builder.ins().icmp_imm_u(IntCC::Equal, operand, 0),
    }
}

fn lower_conversion(builder: &mut FunctionBuilder, dst: &Slot, src: &Slot, value: Value) -> Value {
    let target = native_type(&dst.ty).unwrap();
    let source = src.ty.integer().unwrap();
    match source.bits().cmp(&target.bits()) {
        std::cmp::Ordering::Greater => builder.ins().ireduce(target, value),
        std::cmp::Ordering::Less if source.signed() => builder.ins().sextend(target, value),
        std::cmp::Ordering::Less => builder.ins().uextend(target, value),
        std::cmp::Ordering::Equal => value,
    }
}

fn lower_binary(
    builder: &mut FunctionBuilder,
    op: &BinOp,
    lhs: Value,
    rhs: Value,
    unsigned: bool,
) -> Value {
    match op {
        BinOp::Add => builder.ins().iadd(lhs, rhs),
        BinOp::Sub => builder.ins().isub(lhs, rhs),
        BinOp::Mul => builder.ins().imul(lhs, rhs),
        BinOp::BitAnd => builder.ins().band(lhs, rhs),
        BinOp::BitOr => builder.ins().bor(lhs, rhs),
        BinOp::BitXor => builder.ins().bxor(lhs, rhs),
        BinOp::Shl => builder.ins().ishl(lhs, rhs),
        BinOp::Shr if unsigned => builder.ins().ushr(lhs, rhs),
        BinOp::Shr => builder.ins().sshr(lhs, rhs),
        BinOp::Div if unsigned => builder.ins().udiv(lhs, rhs),
        BinOp::Div => {
            // Zero is guarded by the caller. Int64 has no wider native lane;
            // substitute a safe divisor for -1 before emitting sdiv, then use
            // wrapping negation. A select after an overflowing sdiv is too late.
            if builder.func.dfg.value_type(lhs) == types::I64 {
                let minus_one = builder.ins().iconst(types::I64, -1);
                let one = builder.ins().iconst(types::I64, 1);
                let negate = builder.ins().icmp(IntCC::Equal, rhs, minus_one);
                let divisor = builder.ins().select(negate, one, rhs);
                let quotient = builder.ins().sdiv(lhs, divisor);
                let negative = builder.ins().ineg(lhs);
                builder.ins().select(negate, negative, quotient)
            } else {
                let lhs = builder.ins().sextend(types::I64, lhs);
                let rhs = builder.ins().sextend(types::I64, rhs);
                let quotient = builder.ins().sdiv(lhs, rhs);
                builder.ins().ireduce(types::I32, quotient)
            }
        }
        BinOp::Eq => builder.ins().icmp(IntCC::Equal, lhs, rhs),
        BinOp::Ne => builder.ins().icmp(IntCC::NotEqual, lhs, rhs),
        BinOp::Lt => builder.ins().icmp(
            if unsigned {
                IntCC::UnsignedLessThan
            } else {
                IntCC::SignedLessThan
            },
            lhs,
            rhs,
        ),
        BinOp::Le => builder.ins().icmp(
            if unsigned {
                IntCC::UnsignedLessThanOrEqual
            } else {
                IntCC::SignedLessThanOrEqual
            },
            lhs,
            rhs,
        ),
        BinOp::Gt => builder.ins().icmp(
            if unsigned {
                IntCC::UnsignedGreaterThan
            } else {
                IntCC::SignedGreaterThan
            },
            lhs,
            rhs,
        ),
        BinOp::Ge => builder.ins().icmp(
            if unsigned {
                IntCC::UnsignedGreaterThanOrEqual
            } else {
                IntCC::SignedGreaterThanOrEqual
            },
            lhs,
            rhs,
        ),
    }
}

pub fn signature<M: Module>(module: &M, info: &FunctionInfo) -> Signature {
    let mut sig = module.make_signature();
    if info.suspends {
        sig.params
            .push(AbiParam::new(module.target_config().pointer_type()));
        sig.returns.push(AbiParam::new(types::I32));
    } else {
        for ty in info.signature.params.iter().flat_map(Type::scalar_types) {
            sig.params.push(AbiParam::new(native_type(ty).unwrap()));
        }
        if info.signature.ret.is_aggregate() {
            sig.params
                .push(AbiParam::new(module.target_config().pointer_type()));
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
        ("__joky_runtime_division_by_zero", vec![ptr, ptr], vec![]),
        ("__joky_runtime_invalid_shift", vec![ptr, ptr], vec![]),
        (
            "__joky_runtime_division_by_zero_in",
            vec![types::I64, ptr, ptr],
            vec![],
        ),
        (
            "__joky_runtime_invalid_shift_in",
            vec![types::I64, ptr, ptr],
            vec![],
        ),
        ("joky_runtime_error_code", vec![], vec![types::I32]),
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

/// Emit a cold failure return without changing the public direct function ABI.
/// Callers must check the error channel before using the unspecified result.
fn failure_return(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Option<Value>,
) {
    if let Some(frame) = frame {
        builder
            .ins()
            .call(refs["__joky_cps_release_children"], &[frame]);
        let failed = builder.ins().iconst(types::I32, FAILED as i64);
        builder.ins().store(
            MemFlagsData::new(),
            failed,
            frame,
            std::mem::offset_of!(CpsFrame, state) as i32,
        );
        let status = builder.ins().iconst(types::I32, RUNTIME_ERROR as i64);
        builder.ins().return_(&[status]);
    } else {
        let returns = builder.func.signature.returns.clone();
        let values: Vec<_> = returns
            .iter()
            .map(|param| builder.ins().iconst(param.value_type, 0))
            .collect();
        builder.ins().return_(&values);
    }
}

fn check_call(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Option<Value>,
) {
    let status = helper(builder, refs, "joky_runtime_error_code", &[]);
    let failed = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(status, failed, &[], ready, &[]);
    builder.switch_to_block(failed);
    failure_return(builder, refs, frame);
    builder.switch_to_block(ready);
}

fn check_integer_operand(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Option<Value>,
    rhs: Value,
    op: &BinOp,
    span: crate::Span,
    ptr_ty: types::Type,
) {
    let (invalid, helper_name) = if op.is_shift() {
        // Comparing the complete unsigned bit pattern rejects negative signed
        // counts too. Check before lowering: machine shifts mask large counts.
        let width = i64::from(builder.func.dfg.value_type(rhs).bits());
        (
            builder
                .ins()
                .icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, rhs, width),
            "__joky_runtime_invalid_shift",
        )
    } else {
        (
            builder.ins().icmp_imm_u(IntCC::Equal, rhs, 0),
            "__joky_runtime_division_by_zero",
        )
    };
    let failed = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(invalid, failed, &[], ready, &[]);
    builder.switch_to_block(failed);
    let start = builder.ins().iconst(ptr_ty, span.start() as i64);
    let end = builder.ins().iconst(ptr_ty, span.end() as i64);
    if span.source_id() == crate::SourceId::ANONYMOUS {
        builder.ins().call(refs[helper_name], &[start, end]);
    } else {
        let source = builder
            .ins()
            .iconst(types::I64, span.source_id().raw() as i64);
        builder
            .ins()
            .call(refs[&format!("{helper_name}_in")], &[source, start, end]);
    }
    failure_return(builder, refs, frame);
    builder.switch_to_block(ready);
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
        if functions[&branch.name].may_fail {
            check_call(builder, refs, Some(frame));
        }
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

    for (op, &span) in plan.ops.iter().zip(&plan.op_spans) {
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
                let unsigned = lhs.ty.integer().is_some_and(|integer| !integer.signed());
                let lhs = load(builder, slots, lhs);
                let rhs = load(builder, slots, rhs);
                if op.can_fail() {
                    check_integer_operand(builder, refs, Some(frame), rhs, op, span, ptr_ty);
                }
                let value = lower_binary(builder, op, lhs, rhs, unsigned);
                store(builder, slots, dst, value);
            }
            Op::Convert(dst, src) => {
                let value = load(builder, slots, src);
                let value = lower_conversion(builder, dst, src, value);
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
                let mut args: Vec<_> = args
                    .iter()
                    .flatten()
                    .map(|s| load(builder, slots, s))
                    .collect();
                if let Some(dst) = result.as_ref().filter(|s| s.ty.is_aggregate()) {
                    args.push(builder.ins().iadd_imm_s(slots, (dst.index * 8) as i64));
                }
                let call = builder.ins().call(refs[name], &args);
                if functions[name].may_fail {
                    check_call(builder, refs, Some(frame));
                }
                if let Some(dst) = result.as_ref().filter(|s| !s.ty.is_aggregate()) {
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
                if functions[name].may_fail {
                    check_call(builder, refs, Some(frame));
                }
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
                    if dst.ty.is_aggregate() {
                        let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
                        let source = Slot {
                            index: functions[name].aggregate_result_offset(),
                            ty: dst.ty.clone(),
                        };
                        for (dst, src) in dst.scalars().zip(source.scalars()) {
                            let value = load(builder, child_slots, &src);
                            store(builder, slots, &dst, value);
                        }
                    } else {
                        let value = builder.ins().load(
                            types::I64,
                            MemFlagsData::new(),
                            child,
                            result_offset,
                        );
                        builder.ins().store(
                            MemFlagsData::new(),
                            value,
                            slots,
                            (dst.index * 8) as i32,
                        );
                    }
                }
                builder
                    .ins()
                    .call(refs["__joky_cps_release_child"], &[frame]);
            }
        }
    }
    if !terminated {
        if let Some(result) = plan.result.as_ref().filter(|s| !s.ty.is_aggregate()) {
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
    }
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
    functions: &HashMap<String, FunctionInfo>,
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
    let mut index = 0;
    for ty in &info.signature.params {
        let parameter = Slot {
            index,
            ty: ty.clone(),
        };
        for slot in parameter.scalars() {
            define_slot(builder, &mut variables, &slot, parameters.next().unwrap());
        }
        index += ty.parameter_slots();
    }
    let result_pointer = info
        .signature
        .ret
        .is_aggregate()
        .then(|| parameters.next().unwrap());
    let labels: Vec<_> = (0..plan.label_count)
        .map(|_| builder.create_block())
        .collect();
    let mut terminated = false;
    for (op, &span) in plan.ops.iter().zip(&plan.op_spans) {
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
                let unsigned = lhs.ty.integer().is_some_and(|integer| !integer.signed());
                let lhs = builder.use_var(variables[lhs.index].unwrap());
                let rhs = builder.use_var(variables[rhs.index].unwrap());
                if op.can_fail() {
                    check_integer_operand(builder, refs, None, rhs, op, span, ptr_ty);
                }
                let value = lower_binary(builder, op, lhs, rhs, unsigned);
                define_slot(builder, &mut variables, dst, value);
            }
            Op::Convert(dst, src) => {
                let value = builder.use_var(variables[src.index].unwrap());
                let value = lower_conversion(builder, dst, src, value);
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
                let mut args: Vec<_> = args
                    .iter()
                    .flatten()
                    .map(|slot| builder.use_var(variables[slot.index].unwrap()))
                    .collect();
                let buffer = result.as_ref().filter(|s| s.ty.is_aggregate()).map(|slot| {
                    let bytes = (slot.ty.scalar_types().len() * 8).max(8) as u32;
                    let buffer = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        bytes,
                        3,
                    ));
                    args.push(builder.ins().stack_addr(ptr_ty, buffer, 0));
                    buffer
                });
                let call = builder.ins().call(refs[name], &args);
                if functions[name].may_fail {
                    check_call(builder, refs, None);
                }
                if let Some(dst) = result {
                    if let Some(buffer) = buffer {
                        for (offset, dst) in dst.scalars().enumerate() {
                            let value = builder.ins().stack_load(
                                ptr_ty,
                                types::I64,
                                buffer,
                                (offset * 8) as i32,
                            );
                            let ty = native_type(&dst.ty).unwrap();
                            let value = if ty == types::I64 {
                                value
                            } else {
                                builder.ins().ireduce(ty, value)
                            };
                            define_slot(builder, &mut variables, &dst, value);
                        }
                    } else {
                        let value = builder.inst_results(call)[0];
                        define_slot(builder, &mut variables, dst, value);
                    }
                }
            }
            Op::Scope { .. } => unreachable!("scope is only valid in suspending functions"),
        }
    }
    if !terminated {
        if let Some(pointer) = result_pointer {
            if let Some(result) = &plan.result {
                for (offset, slot) in result.scalars().enumerate() {
                    let value = builder.use_var(variables[slot.index].unwrap());
                    store(
                        builder,
                        pointer,
                        &Slot {
                            index: offset,
                            ty: slot.ty,
                        },
                        value,
                    );
                }
            }
            builder.ins().return_(&[]);
        } else {
            let result: Vec<_> = plan
                .result
                .iter()
                .map(|slot| builder.use_var(variables[slot.index].unwrap()))
                .collect();
            builder.ins().return_(&result);
        }
    }
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
        lower_direct(builder, info, plan, functions, refs, ptr_ty, strings);
    }
}
