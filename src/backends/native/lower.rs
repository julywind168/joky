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
        Type::String | Type::Reference(_) => Some(if cfg!(target_pointer_width = "64") {
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
        UnaryOp::Borrow | UnaryOp::BorrowMut => {
            unreachable!("local borrows are lowered to references")
        }
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
        if info.uses_arena {
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
        ("__joky_arena_new", vec![], vec![ptr]),
        ("__joky_arena_drop", vec![ptr], vec![]),
        (
            "__joky_arena_alloc",
            vec![ptr, ptr, ptr, types::I64, ptr, ptr],
            vec![ptr],
        ),
        ("__joky_cps_slots", vec![ptr], vec![ptr]),
        ("__joky_cps_arena", vec![ptr], vec![ptr]),
        ("__joky_cps_finish", vec![ptr], vec![]),
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
        builder.ins().call(refs["__joky_cps_finish"], &[frame]);
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
        builder.ins().call(refs["__joky_cps_finish"], &[frame]);
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

/// Where a function's IR slots live. Suspending functions address a CPS
/// frame's memory block; direct functions keep scalars in Cranelift SSA
/// variables so the register allocator handles loop backedges. Shared op
/// lowering is written once against this choice.
enum SlotAccess<'a> {
    Frames {
        slots: Value,
    },
    Ssa {
        variables: &'a mut Vec<Option<Variable>>,
    },
}

impl SlotAccess<'_> {
    fn read(&mut self, builder: &mut FunctionBuilder, slot: &Slot) -> Value {
        match self {
            Self::Frames { slots } => load(builder, *slots, slot),
            Self::Ssa { variables } => builder.use_var(variables[slot.index].unwrap()),
        }
    }

    fn write(&mut self, builder: &mut FunctionBuilder, slot: &Slot, value: Value) {
        match self {
            Self::Frames { slots } => store(builder, *slots, slot, value),
            Self::Ssa { variables } => define_slot(builder, variables, slot, value),
        }
    }
}

/// Direct functions have no frame to check or resume; suspending functions
/// re-enter through one state block per pending call.
#[derive(Clone, Copy)]
enum ExecMode<'a> {
    Direct {
        arena: Option<Value>,
    },
    Suspends {
        frame: Value,
        arena: Option<Value>,
        states: &'a [cranelift_codegen::ir::Block],
        state_offset: i32,
    },
}

impl ExecMode<'_> {
    fn arena(self) -> Value {
        match self {
            Self::Direct { arena } | Self::Suspends { arena, .. } => {
                arena.expect("allocation capability propagated before lowering")
            }
        }
    }

    fn frame(self) -> Option<Value> {
        match self {
            Self::Direct { .. } => None,
            Self::Suspends { frame, .. } => Some(frame),
        }
    }
}

/// Non-suspending call. Direct aggregates collect through a stack buffer;
/// CPS aggregates write straight into the caller's frame slots.
#[allow(clippy::too_many_arguments)]
fn lower_sync_call(
    builder: &mut FunctionBuilder,
    slots: &mut SlotAccess<'_>,
    mode: ExecMode<'_>,
    name: &str,
    args: &[Option<Slot>],
    result: &Option<Slot>,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
) {
    let mut call_args: Vec<_> = args
        .iter()
        .flatten()
        .map(|slot| slots.read(builder, slot))
        .collect();
    let buffer = match result.as_ref().filter(|slot| slot.ty.is_aggregate()) {
        Some(dst) => match mode.frame() {
            Some(_) => {
                let SlotAccess::Frames { slots } = &*slots else {
                    unreachable!("suspending functions address frame memory")
                };
                call_args.push(builder.ins().iadd_imm_s(*slots, (dst.index * 8) as i64));
                None
            }
            None => {
                let bytes = (dst.ty.scalar_types().len() * 8).max(8) as u32;
                let buffer = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    bytes,
                    3,
                ));
                call_args.push(builder.ins().stack_addr(ptr_ty, buffer, 0));
                Some(buffer)
            }
        },
        None => None,
    };
    if functions[name].uses_arena {
        call_args.push(mode.arena());
    }
    let call = builder.ins().call(refs[name], &call_args);
    if functions[name].may_fail {
        check_call(builder, refs, mode.frame());
    }
    if let Some(dst) = result {
        if let Some(buffer) = buffer {
            for (offset, dst) in dst.scalars().enumerate() {
                let value =
                    builder
                        .ins()
                        .stack_load(ptr_ty, types::I64, buffer, (offset * 8) as i32);
                let ty = native_type(&dst.ty).unwrap();
                let value = if ty == types::I64 {
                    value
                } else {
                    builder.ins().ireduce(ty, value)
                };
                slots.write(builder, &dst, value);
            }
        } else if !dst.ty.is_aggregate() {
            let value = builder.inst_results(call)[0];
            slots.write(builder, dst, value);
        }
    }
}

/// Suspending call: allocate the child, evaluate arguments once, save the
/// resumption state, and re-enter only the child poll after resumption.
#[allow(clippy::too_many_arguments)]
fn lower_suspending_call(
    builder: &mut FunctionBuilder,
    slots: &mut SlotAccess<'_>,
    name: &str,
    args: &[Option<Slot>],
    result: &Option<Slot>,
    state: u32,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    frame: Value,
    states: &[cranelift_codegen::ir::Block],
    state_offset: i32,
) {
    let result_offset = std::mem::offset_of!(CpsFrame, result) as i32;
    let count = builder
        .ins()
        .iconst(ptr_ty, functions[name].frame_slots as i64);
    let child = helper(builder, refs, "__joky_cps_child", &[frame, count]);
    let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
    for (index, arg) in args.iter().enumerate() {
        if let Some(arg) = arg {
            let value = slots.read(builder, arg);
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
    let saved = builder.ins().iconst(types::I32, i64::from(state));
    builder
        .ins()
        .store(MemFlagsData::new(), saved, frame, state_offset);
    builder.ins().jump(states[state as usize], &[]);
    builder.switch_to_block(states[state as usize]);
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
    builder.ins().call(refs["__joky_cps_finish"], &[frame]);
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
                slots.write(builder, &dst, value);
            }
        } else {
            let value = builder
                .ins()
                .load(types::I64, MemFlagsData::new(), child, result_offset);
            slots.write(builder, dst, value);
        }
    }
    builder
        .ins()
        .call(refs["__joky_cps_release_child"], &[frame]);
}

/// The op loop shared by direct and CPS lowering. These arms must agree on
/// types, evaluation order, and trap placement in both execution shapes;
/// keeping them in one match makes that agreement structural.
#[allow(clippy::too_many_arguments)]
fn lower_ops(
    builder: &mut FunctionBuilder,
    plan: &FunctionPlan,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    strings: &HashMap<String, GlobalValue>,
    slots: &mut SlotAccess<'_>,
    mode: ExecMode<'_>,
) -> bool {
    let labels: Vec<_> = (0..plan.label_count)
        .map(|_| builder.create_block())
        .collect();
    let mut terminated = false;
    for (op, &span) in plan.ops.iter().zip(&plan.op_spans) {
        match op {
            Op::Graph(_) => unreachable!("construction operations are erased after checking"),
            Op::EndLocal(_) => unreachable!("local lifetime markers are erased after checking"),
            Op::Copy(dst, src) | Op::Reference { dst, src, .. } | Op::Reserve { dst, src } => {
                let value = slots.read(builder, src);
                slots.write(builder, dst, value);
            }
            Op::Project { dst, src, path, .. } => {
                let pointer = slots.read(builder, src);
                let (offset, _) = src
                    .ty
                    .reference_type()
                    .unwrap()
                    .target
                    .memory_field_path(path)
                    .expect("validated local field layout");
                let pointer = builder.ins().iadd_imm_u(pointer, offset as i64);
                slots.write(builder, dst, pointer);
            }
            Op::Alloc { dst, value, target } | Op::Local { dst, value, target } => {
                let layout = target.memory_layout().expect("validated memory layout");
                let pointer = if matches!(op, Op::Local { .. }) {
                    assert!(mode.frame().is_none());
                    let storage = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        layout.size.max(1) as u32,
                        layout.align.trailing_zeros() as u8,
                    ));
                    builder.ins().stack_addr(ptr_ty, storage, 0)
                } else {
                    let arena = mode.arena();
                    let size = builder.ins().iconst(ptr_ty, layout.size as i64);
                    let align = builder.ins().iconst(ptr_ty, layout.align as i64);
                    let source = builder
                        .ins()
                        .iconst(types::I64, span.source_id().raw() as i64);
                    let start = builder.ins().iconst(ptr_ty, span.start() as i64);
                    let end = builder.ins().iconst(ptr_ty, span.end() as i64);
                    let pointer = helper(
                        builder,
                        refs,
                        "__joky_arena_alloc",
                        &[arena, size, align, source, start, end],
                    );
                    check_call(builder, refs, mode.frame());
                    pointer
                };
                if let Some(value) = value {
                    for ((offset, _), src) in target
                        .memory_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(value.scalars())
                    {
                        let value = slots.read(builder, &src);
                        builder
                            .ins()
                            .store(MemFlagsData::new(), value, pointer, offset as i32);
                    }
                }
                slots.write(builder, dst, pointer);
            }
            Op::ReadMemory {
                dst,
                reference,
                path,
            } => {
                let pointer = slots.read(builder, reference);
                let (base, target) = reference
                    .ty
                    .reference_type()
                    .unwrap()
                    .target
                    .memory_field_path(path)
                    .unwrap();
                if let Some(dst) = dst {
                    for ((offset, ty), dst) in target
                        .memory_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(dst.scalars())
                    {
                        let value = builder.ins().load(
                            native_type(&ty).unwrap(),
                            MemFlagsData::new(),
                            pointer,
                            (base + offset) as i32,
                        );
                        slots.write(builder, &dst, value);
                    }
                }
            }
            Op::WriteMemory {
                reference,
                path,
                value,
                target,
            } => {
                let pointer = slots.read(builder, reference);
                let (base, _) = reference
                    .ty
                    .reference_type()
                    .unwrap()
                    .target
                    .memory_field_path(path)
                    .unwrap();
                if let Some(value) = value {
                    for ((offset, _), src) in target
                        .memory_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(value.scalars())
                    {
                        let value = slots.read(builder, &src);
                        builder.ins().store(
                            MemFlagsData::new(),
                            value,
                            pointer,
                            (base + offset) as i32,
                        );
                    }
                }
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
                let test = slots.read(builder, condition);
                builder
                    .ins()
                    .brif(test, labels[*then_label], &[], labels[*else_label], &[]);
                terminated = true;
            }
            Op::Literal(dst, literal) => {
                let value = lower_literal(builder, literal, strings);
                slots.write(builder, dst, value);
            }
            Op::FunctionRef(dst, name) => {
                let value = builder.ins().func_addr(ptr_ty, refs[name]);
                slots.write(builder, dst, value);
            }
            Op::Scope { branches, state } => {
                let ExecMode::Suspends {
                    frame,
                    states,
                    state_offset,
                    ..
                } = mode
                else {
                    unreachable!("scope is only valid in suspending functions")
                };
                let SlotAccess::Frames { slots } = &*slots else {
                    unreachable!("scope branches address frame memory")
                };
                lower_scope(
                    builder,
                    ScopeContext {
                        branches,
                        scope_state: *state,
                        functions,
                        refs,
                        ptr_ty,
                        frame,
                        slots: *slots,
                        state_offset,
                        states,
                    },
                );
            }
            Op::Binary(dst, op, lhs, rhs) => {
                let unsigned = lhs.ty.integer().is_some_and(|integer| !integer.signed());
                let lhs = slots.read(builder, lhs);
                let rhs = slots.read(builder, rhs);
                if op.can_fail() {
                    check_integer_operand(builder, refs, mode.frame(), rhs, op, span, ptr_ty);
                }
                let value = lower_binary(builder, op, lhs, rhs, unsigned);
                slots.write(builder, dst, value);
            }
            Op::Convert(dst, src) => {
                let value = slots.read(builder, src);
                let value = lower_conversion(builder, dst, src, value);
                slots.write(builder, dst, value);
            }
            Op::Unary(dst, op, operand) => {
                let operand = slots.read(builder, operand);
                let value = lower_unary(builder, op, operand);
                slots.write(builder, dst, value);
            }
            Op::Call {
                name,
                args,
                result,
                state: None,
            } => {
                lower_sync_call(
                    builder, slots, mode, name, args, result, functions, refs, ptr_ty,
                );
            }
            Op::Call {
                name,
                args,
                result,
                state: Some(state),
            } => {
                let ExecMode::Suspends {
                    frame,
                    states,
                    state_offset,
                    ..
                } = mode
                else {
                    unreachable!("suspending calls are only valid in suspending functions")
                };
                lower_suspending_call(
                    builder,
                    slots,
                    name,
                    args,
                    result,
                    *state,
                    functions,
                    refs,
                    ptr_ty,
                    frame,
                    states,
                    state_offset,
                );
            }
        }
    }
    terminated
}

fn lower_cps(
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
    let frame = builder.block_params(entry)[0];
    let slots = helper(builder, refs, "__joky_cps_slots", &[frame]);
    let arena = info
        .uses_arena
        .then(|| helper(builder, refs, "__joky_cps_arena", &[frame]));
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
    builder.ins().call(refs["__joky_cps_finish"], &[frame]);
    let error = builder.ins().iconst(types::I32, INVALID_STATE as i64);
    builder.ins().return_(&[error]);
    builder.switch_to_block(states[0]);

    let mode = ExecMode::Suspends {
        frame,
        arena,
        states: &states,
        state_offset,
    };
    let mut access = SlotAccess::Frames { slots };
    let terminated = lower_ops(
        builder,
        plan,
        functions,
        refs,
        ptr_ty,
        strings,
        &mut access,
        mode,
    );
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
        builder.ins().call(refs["__joky_cps_finish"], &[frame]);
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

    let arena = info.uses_arena.then(|| parameters.next().unwrap());
    let mut access = SlotAccess::Ssa {
        variables: &mut variables,
    };
    let terminated = lower_ops(
        builder,
        plan,
        functions,
        refs,
        ptr_ty,
        strings,
        &mut access,
        ExecMode::Direct { arena },
    );
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
        lower_cps(builder, info, plan, functions, refs, ptr_ty, strings);
    } else {
        lower_direct(builder, info, plan, functions, refs, ptr_ty, strings);
    }
}

/// Keep the existing host signature while passing one arena through internal calls.
/// Every return from the body, including native failure, rejoins this cleanup.
pub(super) fn arena_entry(
    builder: &mut FunctionBuilder,
    body: FuncRef,
    refs: &HashMap<String, FuncRef>,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let mut args = builder.block_params(entry).to_vec();
    let arena = helper(builder, refs, "__joky_arena_new", &[]);
    let ready = builder.create_block();
    let failed = builder.create_block();
    // Test this allocation, not a possibly pre-existing C error flag.
    builder.ins().brif(arena, ready, &[], failed, &[]);
    builder.switch_to_block(failed);
    failure_return(builder, refs, None);
    builder.switch_to_block(ready);
    args.push(arena);
    let call = builder.ins().call(body, &args);
    let result = builder.inst_results(call).to_vec();
    builder.ins().call(refs["__joky_arena_drop"], &[arena]);
    builder.ins().return_(&result);
    builder.seal_all_blocks();
}
