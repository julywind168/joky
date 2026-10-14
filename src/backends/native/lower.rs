//! Cranelift lowering of the shared typed IR.
use crate::core::ir::{
    BufferOp, CleanupOp, Constant, FloatKind, FunctionInfo, FunctionPlan, Op, ScopeBranch, Slot,
    TextSource,
};
use crate::runtime::cps::{CpsFrame, DONE, FAILED, INVALID_STATE, RUNTIME_ERROR};
use crate::{
    parser::ast::{BinOp, BufferIntrinsic, ScopeKind, Type, UnaryOp},
    Error, ErrorCode, ErrorPhase,
};
use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    types, AbiParam, FuncRef, GlobalValue, InstBuilder, MemFlagsData, Signature, StackSlotData,
    StackSlotKind, Value,
};
use cranelift_frontend::{FunctionBuilder, Switch, Variable};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

pub(super) struct DataRefs {
    pub domain: Option<GlobalValue>,
    pub dyn_tables: HashMap<usize, GlobalValue>,
    pub strings: HashMap<String, GlobalValue>,
    pub callables: HashMap<String, GlobalValue>,
}

fn native_type(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::BufferElement(_) | Type::BufferSlice(_) => None,
        Type::Int32 | Type::Int64 | Type::UInt32 | Type::UInt64 | Type::Rune => Some(
            if matches!(ty, Type::Rune) || ty.integer().unwrap().bits() == 32 {
                types::I32
            } else {
                types::I64
            },
        ),
        Type::Float32 => Some(types::F32),
        Type::Float64 => Some(types::F64),
        Type::Int8 | Type::UInt8 | Type::Bool => Some(types::I8),
        Type::Int16 | Type::UInt16 => Some(types::I16),
        Type::Unit | Type::Record(_) | Type::Enum(_) | Type::Tuple(_) => None,
        Type::Named(_) => Some(types::I64),
        Type::DynTrait(_) | Type::Parameter(_) | Type::Applied(_) | Type::Projection(_) => {
            unreachable!("generic parameter reached native lowering")
        }
        Type::Function(_)
        | Type::String
        | Type::Resource(_)
        | Type::Ptr
        | Type::Reference(_)
        | Type::Buffer(_) => Some(if cfg!(target_pointer_width = "64") {
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
        Constant::NullPtr => builder.ins().iconst(native_type(&Type::Ptr).unwrap(), 0),
        Constant::Float32(n) => builder.ins().f32const(*n),
        Constant::Float64(n) => builder.ins().f64const(*n),
        Constant::Int8(n) => builder.ins().iconst(types::I8, i64::from(*n)),
        Constant::Int16(n) => builder.ins().iconst(types::I16, i64::from(*n)),
        Constant::Int32(n) => builder.ins().iconst(types::I32, *n as i64),
        Constant::Int64(n) => builder.ins().iconst(types::I64, *n),
        Constant::UInt8(n) => builder.ins().iconst(types::I8, i64::from(*n)),
        Constant::UInt16(n) => builder.ins().iconst(types::I16, i64::from(*n)),
        Constant::UInt32(n) => builder.ins().iconst(types::I32, i64::from(*n)),
        Constant::Rune(n) => builder.ins().iconst(types::I32, i64::from(*n)),
        Constant::UInt64(n) => builder.ins().iconst(types::I64, *n as i64),
        Constant::Bool(b) => builder.ins().iconst(types::I8, *b as i64),
        Constant::String(text) => builder
            .ins()
            .symbol_value(native_type(&Type::String).unwrap(), strings[text]),
    }
}

/// Materialize a zero of any scalar native type; floats need a float constant
/// rather than `iconst`, which the verifier rejects for non-integer types.
fn zero_of(builder: &mut FunctionBuilder, ty: types::Type) -> Value {
    if ty == types::F32 {
        builder.ins().f32const(0.0)
    } else if ty == types::F64 {
        builder.ins().f64const(0.0)
    } else {
        builder.ins().iconst(ty, 0)
    }
}

fn lower_unary(builder: &mut FunctionBuilder, op: &UnaryOp, operand: Value) -> Value {
    match op {
        UnaryOp::Neg if builder.func.dfg.value_type(operand).is_float() => {
            builder.ins().fneg(operand)
        }
        UnaryOp::Neg => builder.ins().ineg(operand),
        UnaryOp::BitNot => builder.ins().bnot(operand),
        UnaryOp::Not => builder.ins().icmp_imm_u(IntCC::Equal, operand, 0),
        UnaryOp::Borrow | UnaryOp::BorrowMut | UnaryOp::Deref => {
            unreachable!("local borrows are lowered to references")
        }
    }
}

/// Function-boundary lanes widen narrow integers to I32. C compilers and the
/// Rust host adapters sign- or zero-extend narrow register values by type, so
/// keeping the language-internal I8/I16 lanes out of cross-boundary signatures
/// avoids relying on unspecified high register bits.
fn native_lane(ty: &Type) -> Option<types::Type> {
    match ty {
        Type::Int8 | Type::Int16 | Type::UInt8 | Type::UInt16 => Some(types::I32),
        _ => native_type(ty),
    }
}

/// Widen a value from its internal lane to the I32 boundary lane.
fn widen_lane(builder: &mut FunctionBuilder, ty: &Type, value: Value) -> Value {
    match native_type(ty) {
        Some(lane) if lane != types::I32 && native_lane(ty) == Some(types::I32) => {
            if ty.integer().is_some_and(|integer| integer.signed()) {
                builder.ins().sextend(types::I32, value)
            } else {
                builder.ins().uextend(types::I32, value)
            }
        }
        _ => value,
    }
}

/// Narrow a boundary I32 value back into its internal lane.
fn narrow_lane(builder: &mut FunctionBuilder, ty: &Type, value: Value) -> Value {
    if let Some(lane) = native_type(ty) {
        if lane != types::I32 && native_lane(ty) == Some(types::I32) {
            return builder.ins().ireduce(lane, value);
        }
    }
    value
}

fn lower_conversion(builder: &mut FunctionBuilder, dst: &Slot, src: &Slot, value: Value) -> Value {
    let target = native_type(&dst.ty).unwrap();
    if dst.ty.is_float() {
        if src.ty.is_float() {
            return if src.ty == dst.ty {
                value
            } else if dst.ty == Type::Float32 {
                builder.ins().fdemote(target, value)
            } else {
                builder.ins().fpromote(target, value)
            };
        }
        return if src.ty.integer().unwrap().signed() {
            builder.ins().fcvt_from_sint(target, value)
        } else {
            builder.ins().fcvt_from_uint(target, value)
        };
    }
    if src.ty.is_float() {
        // The trusted checked-conversion bridges have already proven the
        // rounded value inside the target's exact bounds, so saturating
        // conversion cannot fire and preserves the mathematical result.
        return if dst.ty.integer().unwrap().signed() {
            builder.ins().fcvt_to_sint_sat(target, value)
        } else {
            builder.ins().fcvt_to_uint_sat(target, value)
        };
    }
    let source = if src.ty == Type::Rune {
        crate::core::integer::IntegerType::Rune
    } else {
        src.ty.integer().unwrap()
    };
    match source.bits().cmp(&target.bits()) {
        std::cmp::Ordering::Greater => builder.ins().ireduce(target, value),
        std::cmp::Ordering::Less if source.signed() => builder.ins().sextend(target, value),
        std::cmp::Ordering::Less => builder.ins().uextend(target, value),
        std::cmp::Ordering::Equal => value,
    }
}

fn lower_float(builder: &mut FunctionBuilder, kind: FloatKind, value: Value) -> Value {
    match kind {
        FloatKind::FromText => unreachable!("text conversion requires a runtime helper"),
        FloatKind::Sqrt => builder.ins().sqrt(value),
        FloatKind::Floor => builder.ins().floor(value),
        FloatKind::Ceil => builder.ins().ceil(value),
        FloatKind::Trunc => builder.ins().trunc(value),
        FloatKind::RoundTiesEven => builder.ins().nearest(value),
        FloatKind::ToBits => {
            let int = if builder.func.dfg.value_type(value) == types::F64 {
                types::I64
            } else {
                types::I32
            };
            builder.ins().bitcast(int, MemFlagsData::new(), value)
        }
        FloatKind::FromBits => {
            let float = if builder.func.dfg.value_type(value) == types::I64 {
                types::F64
            } else {
                types::F32
            };
            builder.ins().bitcast(float, MemFlagsData::new(), value)
        }
    }
}

fn lower_binary(
    builder: &mut FunctionBuilder,
    op: &BinOp,
    lhs: Value,
    rhs: Value,
    unsigned: bool,
) -> Value {
    if builder.func.dfg.value_type(lhs).is_float() {
        return match op {
            BinOp::Add => builder.ins().fadd(lhs, rhs),
            BinOp::Sub => builder.ins().fsub(lhs, rhs),
            BinOp::Mul => builder.ins().fmul(lhs, rhs),
            BinOp::Div => builder.ins().fdiv(lhs, rhs),
            BinOp::Eq => builder.ins().fcmp(FloatCC::Equal, lhs, rhs),
            BinOp::Ne => builder.ins().fcmp(FloatCC::NotEqual, lhs, rhs),
            BinOp::Lt => builder.ins().fcmp(FloatCC::LessThan, lhs, rhs),
            BinOp::Le => builder.ins().fcmp(FloatCC::LessThanOrEqual, lhs, rhs),
            BinOp::Gt => builder.ins().fcmp(FloatCC::GreaterThan, lhs, rhs),
            BinOp::Ge => builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs),
            _ => unreachable!("invalid floating operation"),
        };
    }
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
        BinOp::Rem if unsigned => builder.ins().urem(lhs, rhs),
        BinOp::Rem => {
            // Substituting 1 for -1 avoids the MIN % -1 hardware trap
            // and produces the wrapping remainder, zero, at every width.
            let width = builder.func.dfg.value_type(lhs);
            let minus_one = builder.ins().iconst(width, -1);
            let one = builder.ins().iconst(width, 1);
            let negative_one = builder.ins().icmp(IntCC::Equal, rhs, minus_one);
            let divisor = builder.ins().select(negative_one, one, rhs);
            builder.ins().srem(lhs, divisor)
        }
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
                // Narrow signed lanes divide in I64 and wrap back to the
                // operand width, so Int8/Int16 MIN / -1 cannot trap.
                let width = builder.func.dfg.value_type(lhs);
                let lhs = builder.ins().sextend(types::I64, lhs);
                let rhs = builder.ins().sextend(types::I64, rhs);
                let quotient = builder.ins().sdiv(lhs, rhs);
                builder.ins().ireduce(width, quotient)
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
        for (index, ty) in info.signature.params.iter().enumerate() {
            if index == 0 && info.closure_entry && ty.closure_info().is_some() {
                sig.params
                    .push(AbiParam::new(module.target_config().pointer_type()));
            } else {
                for scalar in ty.scalar_types() {
                    sig.params.push(AbiParam::new(native_lane(scalar).unwrap()));
                }
            }
        }
        if info.signature.ret.is_aggregate() {
            sig.params
                .push(AbiParam::new(module.target_config().pointer_type()));
        }
        if info.needs_arena {
            sig.params
                .push(AbiParam::new(module.target_config().pointer_type()));
        }
        if let Some(ty) = native_lane(&info.signature.ret) {
            sig.returns.push(AbiParam::new(ty));
        }
    }
    sig
}

pub(super) fn instance_signature<M: Module>(module: &M) -> Signature {
    let mut signature = module.make_signature();
    let ptr = module.target_config().pointer_type();
    signature
        .params
        .extend([AbiParam::new(ptr), AbiParam::new(ptr), AbiParam::new(ptr)]);
    signature
}

/// Private typed trampoline: input/output lanes are bit patterns, while the
/// callee uses its real register ABI with caller-owned invocation storage.
pub(super) fn instance_entry(builder: &mut FunctionBuilder, body: FuncRef, info: &FunctionInfo) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let input = builder.block_params(entry)[0];
    let output = builder.block_params(entry)[1];
    let mut args = Vec::new();
    let mut index = 0;
    for ty in &info.signature.params {
        if *ty == Type::Unit {
            index += 1;
            continue;
        }
        for ty in ty.scalar_types() {
            let value = load(
                builder,
                input,
                &Slot {
                    index,
                    ty: ty.clone(),
                },
            );
            args.push(widen_lane(builder, ty, value));
            index += 1;
        }
    }
    if info.signature.ret.is_aggregate() {
        args.push(output);
    }
    if info.needs_arena {
        args.push(builder.block_params(entry)[2]);
    }
    let call = builder.ins().call(body, &args);
    if let Some(&result) = builder.inst_results(call).first() {
        let result = narrow_lane(builder, &info.signature.ret, result);
        store(
            builder,
            output,
            &Slot {
                index: 0,
                ty: info.signature.ret.clone(),
            },
            result,
        );
    }
    builder.ins().return_(&[]);
}

pub fn declare_helpers<M: Module>(module: &mut M) -> Result<HashMap<String, FuncId>, Error> {
    let ptr = module.target_config().pointer_type();
    let mut ids = HashMap::new();
    for (name, params, returns) in [
        ("__joky_float_env_enter", vec![], vec![types::I64]),
        ("__joky_float_env_reset", vec![], vec![]),
        ("__joky_float_env_restore", vec![types::I64], vec![]),
        (
            "joky_cps_poll_borrowing_v1",
            vec![ptr, ptr],
            vec![types::I32],
        ),
        ("__joky_arena_new", vec![], vec![ptr]),
        ("__joky_arena_drop", vec![ptr], vec![]),
        ("__joky_buffer_commit_next", vec![ptr], vec![]),
        ("__joky_buffer_cleanup_new", vec![ptr, ptr], vec![]),
        ("__joky_buffer_cleanup_owner", vec![ptr, ptr], vec![]),
        (
            "__joky_buffer_cleanup_guard",
            vec![ptr, ptr, types::I32],
            vec![],
        ),
        (
            "__joky_buffer_cleanup_hook",
            vec![ptr, ptr, ptr, ptr, ptr],
            vec![],
        ),
        (
            "__joky_arena_alloc",
            vec![ptr, ptr, ptr, types::I64, ptr, ptr],
            vec![ptr],
        ),
        ("__joky_memory_swap", vec![ptr, ptr, ptr], vec![]),
        ("__joky_buffer_swap", vec![ptr, ptr], vec![]),
        (
            "__joky_cleanup_storage_v2",
            vec![ptr, ptr, ptr, ptr],
            vec![],
        ),
        ("__joky_cleanup_snapshot", vec![ptr, ptr], vec![]),
        (
            "__joky_resource_create_v1",
            vec![ptr, ptr, types::I64],
            vec![ptr],
        ),
        (
            "__joky_resource_io_poll_v1",
            vec![ptr, ptr, types::I32],
            vec![types::I32],
        ),
        ("__joky_resource_poll_v1", vec![ptr, ptr], vec![types::I32]),
        ("__joky_dns_poll", vec![ptr], vec![types::I32]),
        ("__joky_tcp_poll", vec![ptr, types::I32], vec![types::I32]),
        (
            "__joky_domain_poll_v2",
            vec![ptr, types::I32],
            vec![types::I32],
        ),
        ("__joky_cleanup_storage_owner", vec![ptr, ptr, ptr], vec![]),
        (
            "__joky_cleanup_storage_hook",
            vec![ptr, ptr, ptr, ptr, ptr, ptr],
            vec![],
        ),
        (
            "__joky_cleanup_storage_guard",
            vec![ptr, ptr, ptr, types::I32],
            vec![],
        ),
        ("__joky_cleanup_new", vec![ptr], vec![ptr]),
        ("__joky_cleanup_register", vec![ptr, ptr, ptr], vec![]),
        ("__joky_cleanup_append", vec![ptr, ptr, ptr, ptr], vec![]),
        ("__joky_cleanup_hook", vec![ptr, ptr, ptr, ptr, ptr], vec![]),
        ("__joky_cleanup_forget", vec![ptr, ptr], vec![]),
        (
            "__joky_cleanup_extract_field",
            vec![ptr, ptr, ptr, ptr, ptr],
            vec![],
        ),
        (
            "__joky_cleanup_replace_field",
            vec![ptr, ptr, ptr, ptr, ptr],
            vec![],
        ),
        ("__joky_cleanup_move", vec![ptr, ptr, ptr], vec![]),
        ("__joky_cleanup_run", vec![ptr, ptr, ptr], vec![]),
        ("__joky_cleanup_finish", vec![ptr], vec![]),
        ("__joky_cleanup_finish_return", vec![ptr, ptr], vec![]),
        ("__joky_cps_slots", vec![ptr], vec![ptr]),
        ("__joky_cps_arena", vec![ptr], vec![ptr]),
        ("__joky_cps_cleanup_v2", vec![ptr], vec![ptr]),
        (
            "__joky_cps_transfer_owner_v2",
            vec![ptr, ptr, ptr, ptr],
            vec![],
        ),
        ("__joky_cps_capture_text_v2", vec![ptr, ptr], vec![]),
        ("__joky_text_cps_prepare_v2", vec![ptr, ptr, ptr], vec![ptr]),
        ("__joky_text_cps_argument_v2", vec![ptr, ptr], vec![]),
        ("__joky_cps_finish", vec![ptr], vec![]),
        ("__joky_cps_finish_return_v1", vec![ptr, ptr], vec![]),
        ("__joky_cps_child", vec![ptr, ptr], vec![ptr]),
        ("__joky_cps_release_child", vec![ptr], vec![]),
        ("__joky_cps_child_at", vec![ptr, ptr, ptr], vec![ptr]),
        ("__joky_cps_release_children", vec![ptr], vec![]),
        ("__joky_cps_task_scope_enter", vec![ptr], vec![]),
        ("__joky_cps_task_child", vec![ptr, ptr], vec![ptr]),
        ("__joky_cps_task_branch_commit", vec![ptr, ptr, ptr], vec![]),
        ("__joky_cps_task_join", vec![ptr], vec![types::I32]),
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
        (
            "__joky_runtime_call_failed",
            vec![types::I64, ptr, ptr],
            vec![],
        ),
        (
            "__joky_text_ffi_call_v1",
            vec![ptr, ptr, ptr, ptr, types::I32],
            vec![types::I64],
        ),
        ("__joky_string_view_v2", vec![ptr, ptr], vec![]),
        ("__joky_float_from_text_v1", vec![ptr], vec![types::I64]),
        (
            "__joky_string_input_v2",
            vec![ptr, ptr, types::I64],
            vec![ptr],
        ),
        ("__joky_string_result_copy_v2", vec![ptr, ptr], vec![]),
        ("joky_string_result_free_v2", vec![ptr], vec![]),
        (
            "__joky_string_from_view_v2",
            vec![ptr, ptr, types::I64, ptr, ptr],
            vec![ptr],
        ),
        (
            "__joky_string_clone_v2",
            vec![ptr, ptr, types::I64, ptr, ptr],
            vec![ptr],
        ),
        (
            "__joky_float_to_text_v2",
            vec![ptr, types::I64, types::I32, types::I64, ptr, ptr],
            vec![ptr],
        ),
        (
            "__joky_runtime_assert_failed_v2",
            vec![ptr, types::I64, ptr, ptr],
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
    for intrinsic in BufferIntrinsic::ALL {
        // Runtime helpers validate layouts/indices and return internal addresses.
        // Element values use typed inline loads/stores, never a polymorphic C ABI.
        let (params, returns) = match intrinsic {
            BufferIntrinsic::Allocate => (vec![ptr, types::I64, ptr, ptr], vec![ptr]),
            BufferIntrinsic::Len | BufferIntrinsic::Capacity | BufferIntrinsic::SliceLen => {
                (vec![ptr], vec![types::I64])
            }
            BufferIntrinsic::CapacityLimit => (vec![ptr], vec![types::I64]),
            BufferIntrinsic::Load | BufferIntrinsic::SliceLoad | BufferIntrinsic::SliceStore => {
                (vec![ptr, types::I64], vec![ptr])
            }
            BufferIntrinsic::ElementLoad | BufferIntrinsic::ElementStore => (vec![ptr], vec![ptr]),
            BufferIntrinsic::Borrow
            | BufferIntrinsic::BorrowRef
            | BufferIntrinsic::BorrowMutRef
            | BufferIntrinsic::BorrowMut
            | BufferIntrinsic::SliceBorrow
            | BufferIntrinsic::SliceBorrowMut
            | BufferIntrinsic::SliceBorrowRef
            | BufferIntrinsic::SliceBorrowMutRef => (vec![ptr, types::I64], vec![ptr]),
            BufferIntrinsic::Slice
            | BufferIntrinsic::SliceMut
            | BufferIntrinsic::Subslice
            | BufferIntrinsic::SubsliceMut => (vec![ptr, ptr, types::I64, types::I64], vec![ptr]),
            BufferIntrinsic::InitializeNext => (vec![ptr], vec![ptr]),
            BufferIntrinsic::Store => (vec![ptr, ptr, types::I64], vec![ptr]),
            BufferIntrinsic::TakeLast => (vec![ptr], vec![ptr]),
            BufferIntrinsic::SwapElements => (vec![ptr, types::I64, types::I64], vec![]),
            BufferIntrinsic::Transfer => (vec![ptr, ptr], vec![]),
            BufferIntrinsic::Truncate => (vec![ptr, ptr, types::I64], vec![]),
            BufferIntrinsic::Replace => (vec![ptr, ptr, ptr], vec![]),
            BufferIntrinsic::Release => (vec![ptr, ptr], vec![]),
        };
        let mut sig = module.make_signature();
        sig.params = params.into_iter().map(AbiParam::new).collect();
        if intrinsic.may_fail() {
            sig.params
                .extend([types::I64, ptr, ptr].into_iter().map(AbiParam::new));
        }
        sig.returns = returns.into_iter().map(AbiParam::new).collect();
        let id = module
            .declare_function(intrinsic.runtime_symbol(), Linkage::Import, &sig)
            .map_err(|e| {
                Error::new_with_code(ErrorPhase::Compile, ErrorCode::LinkError, e.to_string())
            })?;
        ids.insert(intrinsic.symbol().into(), id);
    }
    Ok(ids)
}

/// A distinct descriptor import prevents a legacy CPS symbol from satisfying a
/// borrowing import accidentally, including in standalone AOT programs.
pub fn borrowing_entry(
    builder: &mut FunctionBuilder,
    descriptor: GlobalValue,
    poll: FuncRef,
    ptr_ty: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let frame = builder.block_params(entry)[0];
    let descriptor = builder.ins().symbol_value(ptr_ty, descriptor);
    let call = builder.ins().call(poll, &[frame, descriptor]);
    let status = builder.inst_results(call)[0];
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

fn load(builder: &mut FunctionBuilder, slots: Value, slot: &Slot) -> Value {
    let value = builder.ins().load(
        types::I64,
        MemFlagsData::new(),
        slots,
        (slot.index * 8) as i32,
    );
    let ty = native_type(&slot.ty).unwrap();
    unpack(builder, ty, value)
}

fn unpack(builder: &mut FunctionBuilder, ty: types::Type, bits: Value) -> Value {
    if ty == types::F64 {
        builder.ins().bitcast(ty, MemFlagsData::new(), bits)
    } else if ty == types::F32 {
        let bits = builder.ins().ireduce(types::I32, bits);
        builder.ins().bitcast(ty, MemFlagsData::new(), bits)
    } else if ty == types::I64 {
        bits
    } else {
        builder.ins().ireduce(ty, bits)
    }
}

fn wide(builder: &mut FunctionBuilder, value: Value) -> Value {
    let ty = builder.func.dfg.value_type(value);
    let value = if ty.is_float() {
        builder.ins().bitcast(
            if ty == types::F32 {
                types::I32
            } else {
                types::I64
            },
            MemFlagsData::new(),
            value,
        )
    } else {
        value
    };
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
    cleanup: Option<Value>,
) {
    if let Some(cleanup) = cleanup.filter(|_| frame.is_none()) {
        builder
            .ins()
            .call(refs["__joky_cleanup_finish"], &[cleanup]);
    }
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
            .map(|param| zero_of(builder, param.value_type))
            .collect();
        builder.ins().return_(&values);
    }
}

fn check_call(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Option<Value>,
    cleanup: Option<Value>,
) {
    let status = helper(builder, refs, "joky_runtime_error_code", &[]);
    let failed = builder.create_block();
    let ready = builder.create_block();
    builder.ins().brif(status, failed, &[], ready, &[]);
    builder.switch_to_block(failed);
    failure_return(builder, refs, frame, cleanup);
    builder.switch_to_block(ready);
}

#[allow(clippy::too_many_arguments)]
fn check_integer_operand(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Option<Value>,
    cleanup: Option<Value>,
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
    failure_return(builder, refs, frame, cleanup);
    builder.switch_to_block(ready);
}

fn cps_slot_count(
    builder: &mut FunctionBuilder,
    info: &FunctionInfo,
    descriptor: Option<Value>,
    ptr_ty: types::Type,
) -> Value {
    match descriptor {
        Some(pointer) => {
            builder
                .ins()
                .load(ptr_ty, MemFlagsData::new(), pointer, ptr_ty.bytes() as i32)
        }
        None => builder.ins().iconst(ptr_ty, info.frame_slots as i64),
    }
}

fn poll_cps(
    builder: &mut FunctionBuilder,
    direct: Option<FuncRef>,
    descriptor: Option<Value>,
    child: Value,
    ptr_ty: types::Type,
) -> Value {
    let call = match descriptor {
        Some(pointer) => {
            let entry = builder.ins().load(ptr_ty, MemFlagsData::new(), pointer, 0);
            let mut signature = Signature::new(builder.func.signature.call_conv);
            signature.params.push(AbiParam::new(ptr_ty));
            signature.returns.push(AbiParam::new(types::I32));
            let signature = builder.import_signature(signature);
            builder.ins().call_indirect(signature, entry, &[child])
        }
        None => builder
            .ins()
            .call(direct.expect("declared CPS entry"), &[child]),
    };
    builder.inst_results(call)[0]
}

fn child_at(
    builder: &mut FunctionBuilder,
    refs: &HashMap<String, FuncRef>,
    frame: Value,
    index: usize,
    count: Value,
    ptr_ty: types::Type,
) -> Value {
    let index = builder.ins().iconst(ptr_ty, index as i64);
    helper(builder, refs, "__joky_cps_child_at", &[frame, index, count])
}

fn lower_cleanup(
    builder: &mut FunctionBuilder,
    op: &CleanupOp,
    plan: &FunctionPlan,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    slots: &mut SlotAccess<'_>,
    mode: ExecMode<'_>,
) {
    let CleanupOp::Register {
        value,
        place,
        hooks,
        elements,
    } = op;
    let cleanup = mode.cleanup().unwrap();
    if let Some(place) = place {
        let pointer = slots.read(builder, place);
        for ((offset, ty), scalar) in value
            .ty
            .storage_scalars()
            .unwrap()
            .into_iter()
            .zip(value.scalars())
        {
            let bits = builder.ins().load(
                native_type(&ty).unwrap(),
                MemFlagsData::new(),
                pointer,
                offset as i32,
            );
            slots.write(builder, &scalar, bits);
        }
    }
    let owner = value.scalars().find(|s| s.ty.is_buffer()).unwrap();
    let index = builder.ins().iconst(ptr_ty, owner.index as i64);
    let handle = slots.read(builder, &owner);
    builder
        .ins()
        .call(refs["__joky_cleanup_register"], &[cleanup, index, handle]);
    for field in value.scalars().filter(|s| s.ty.is_buffer()).skip(1) {
        let field_index = builder.ins().iconst(ptr_ty, field.index as i64);
        let handle = slots.read(builder, &field);
        builder.ins().call(
            refs["__joky_cleanup_append"],
            &[cleanup, index, field_index, handle],
        );
    }
    for element in elements {
        let scalar = value.scalars().nth(element.buffer).unwrap();
        let handle = slots.read(builder, &scalar);
        let charge = element.owners.len() * std::mem::size_of::<usize>()
            + element.hooks.len() * std::mem::size_of::<crate::runtime::buffer::ElementHook>()
            + element
                .hooks
                .iter()
                .map(|hook| hook.guards.len() * std::mem::size_of::<(usize, i32)>())
                .sum::<usize>();
        let charge = builder.ins().iconst(ptr_ty, charge as i64);
        builder
            .ins()
            .call(refs["__joky_buffer_cleanup_new"], &[handle, charge]);
        for offset in &element.owners {
            let offset = builder.ins().iconst(ptr_ty, *offset as i64);
            builder
                .ins()
                .call(refs["__joky_buffer_cleanup_owner"], &[handle, offset]);
        }
        for hook in &element.hooks {
            let before = builder.ins().iconst(ptr_ty, hook.before as i64);
            let offset = builder.ins().iconst(ptr_ty, hook.offset as i64);
            let indirect = builder.ins().iconst(ptr_ty, hook.indirect as i64);
            let callback = builder.ins().func_addr(ptr_ty, refs[&hook.name]);
            builder.ins().call(
                refs["__joky_buffer_cleanup_hook"],
                &[handle, before, offset, indirect, callback],
            );
            for (offset, tag) in &hook.guards {
                let offset = builder.ins().iconst(ptr_ty, *offset as i64);
                let tag = builder.ins().iconst(types::I32, *tag as i64);
                builder
                    .ins()
                    .call(refs["__joky_buffer_cleanup_guard"], &[handle, offset, tag]);
            }
        }
    }
    if place.is_some() || !hooks.is_empty() {
        let pointer = if let Some(place) = place {
            slots.read(builder, place)
        } else {
            let layout = value.ty.storage_layout().unwrap();
            let pointer = if let SlotAccess::Frames { slots } = &*slots {
                let offset = plan.cps_local_slots[&value.index] * 8;
                builder.ins().iadd_imm_u(*slots, offset as i64)
            } else {
                let storage = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    layout.size.max(1) as u32,
                    layout.align.trailing_zeros() as u8,
                ));
                builder.ins().stack_addr(ptr_ty, storage, 0)
            };
            for ((offset, _), scalar) in value
                .ty
                .storage_scalars()
                .unwrap()
                .into_iter()
                .zip(value.scalars())
            {
                let bits = slots.read(builder, &scalar);
                builder
                    .ins()
                    .store(MemFlagsData::new(), bits, pointer, offset as i32);
            }
            pointer
        };
        let size = builder
            .ins()
            .iconst(ptr_ty, value.ty.storage_layout().unwrap().size as i64);
        builder.ins().call(
            refs["__joky_cleanup_storage_v2"],
            &[cleanup, index, pointer, size],
        );
        for (offset, ty) in value.ty.storage_scalars().unwrap() {
            if ty.is_buffer() {
                let offset = builder.ins().iconst(ptr_ty, offset as i64);
                builder.ins().call(
                    refs["__joky_cleanup_storage_owner"],
                    &[cleanup, index, offset],
                );
            }
        }
        for hook in hooks {
            let before = builder.ins().iconst(ptr_ty, hook.before as i64);
            let offset = builder.ins().iconst(ptr_ty, hook.offset as i64);
            let indirect = builder.ins().iconst(ptr_ty, hook.indirect as i64);
            let callback = builder.ins().func_addr(ptr_ty, refs[&hook.name]);
            builder.ins().call(
                refs["__joky_cleanup_storage_hook"],
                &[cleanup, index, before, offset, indirect, callback],
            );
            for (offset, tag) in &hook.guards {
                let offset = builder.ins().iconst(ptr_ty, *offset as i64);
                let tag = builder.ins().iconst(types::I32, *tag as i64);
                builder.ins().call(
                    refs["__joky_cleanup_storage_guard"],
                    &[cleanup, index, offset, tag],
                );
            }
        }
    }
    if !elements.is_empty() {
        check_call(builder, refs, mode.frame(), mode.cleanup());
    }
}

struct ScopeContext<'a> {
    plan: &'a FunctionPlan,
    mode: ExecMode<'a>,
    result: Option<&'a Slot>,
    kind: ScopeKind,
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
        plan,
        mode,
        result,
        kind,
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
    // frames remain in the parent until the structured operation ends.
    for (index, branch) in branches.iter().enumerate() {
        let descriptor = branch
            .indirect
            .as_ref()
            .map(|target| load(builder, slots, target));
        let count = cps_slot_count(builder, &functions[&branch.name], descriptor, ptr_ty);
        let child = child_at(builder, refs, frame, index, count, ptr_ty);
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
        for (to, arg) in branch.args.iter().enumerate().filter_map(|(to, arg)| {
            arg.as_ref()
                .filter(|s| s.ty.is_suspend_safe_owner())
                .map(|s| (to, s))
        }) {
            let from = builder.ins().iconst(ptr_ty, arg.owner_key() as i64);
            let to = builder.ins().iconst(ptr_ty, to as i64);
            builder.ins().call(
                refs["__joky_cps_transfer_owner_v2"],
                &[frame, child, from, to],
            );
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

    let finished = builder.create_block();
    // Poll incomplete children in source order. A completed join child is
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
        let descriptor = branch
            .indirect
            .as_ref()
            .map(|target| load(builder, slots, target));
        let count = cps_slot_count(builder, &functions[&branch.name], descriptor, ptr_ty);
        let child = child_at(builder, refs, frame, index, count, ptr_ty);
        let status = poll_cps(
            builder,
            refs.get(&branch.name).copied(),
            descriptor,
            child,
            ptr_ty,
        );
        if functions[&branch.name].may_fail {
            check_call(builder, refs, Some(frame), None);
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
        if let Some(dst) = branch.result.as_ref().or(result) {
            if dst.ty.is_aggregate() {
                let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
                let source = Slot {
                    index: functions[&branch.name].aggregate_result_offset(),
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
                    std::mem::offset_of!(CpsFrame, result) as i32,
                );
                let value = unpack(builder, native_type(&dst.ty).unwrap(), value);
                store(builder, slots, dst, value);
            }
        }
        if let Some(cleanup) = &branch.cleanup {
            lower_cleanup(
                builder,
                cleanup,
                plan,
                refs,
                ptr_ty,
                &mut SlotAccess::Frames { slots },
                mode,
            );
        }
        let one = builder.ins().iconst(types::I8, 1);
        store(builder, slots, &branch.done, one);
        builder.ins().jump(
            if kind == ScopeKind::Race {
                finished
            } else {
                after
            },
            &[],
        );

        builder.switch_to_block(after);
    }

    let pending = builder.create_block();
    let continuation = builder.create_block();
    if kind == ScopeKind::Join {
        for branch in branches {
            let next = builder.create_block();
            let done = load(builder, slots, &branch.done);
            let one = builder.ins().iconst(types::I8, 1);
            let is_done = builder.ins().icmp(IntCC::Equal, done, one);
            builder.ins().brif(is_done, next, &[], pending, &[]);
            builder.switch_to_block(next);
        }
        builder.ins().jump(finished, &[]);
    } else {
        builder.ins().jump(pending, &[]);
    }
    builder.switch_to_block(finished);
    // ScopeFinish releases children after the caller registers the result.
    // No source continuation can run between these two IR operations.
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
        cleanup: Option<Value>,
    },
    Suspends {
        frame: Value,
        arena: Option<Value>,
        cleanup: Option<Value>,
        states: &'a [cranelift_codegen::ir::Block],
        state_offset: i32,
    },
}

impl ExecMode<'_> {
    fn arena(self) -> Value {
        match self {
            Self::Direct { arena, .. } | Self::Suspends { arena, .. } => {
                arena.expect("allocation capability propagated before lowering")
            }
        }
    }

    fn cleanup(self) -> Option<Value> {
        match self {
            Self::Direct { cleanup, .. } | Self::Suspends { cleanup, .. } => cleanup,
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
    indirect: &Option<Slot>,
    args: &[Option<Slot>],
    result: &Option<Slot>,
    functions: &HashMap<String, FunctionInfo>,
    refs: &HashMap<String, FuncRef>,
    ptr_ty: types::Type,
    span: crate::Span,
) {
    let mut call_args: Vec<_> = args
        .iter()
        .flatten()
        .map(|slot| {
            let value = slots.read(builder, slot);
            widen_lane(builder, &slot.ty, value)
        })
        .collect();
    if functions[name].closure_entry {
        if let Some(environment) = functions[name]
            .signature
            .params
            .first()
            .filter(|ty| ty.closure_info().is_some())
        {
            let layout = environment.storage_layout().unwrap();
            let storage = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size.max(1) as u32,
                layout.align.trailing_zeros() as u8,
            ));
            let pointer = builder.ins().stack_addr(ptr_ty, storage, 0);
            for ((offset, ty), value) in environment
                .storage_scalars()
                .unwrap()
                .into_iter()
                .zip(call_args.iter().copied())
            {
                let value = narrow_lane(builder, &ty, value);
                builder
                    .ins()
                    .store(MemFlagsData::new(), value, pointer, offset as i32);
            }
            call_args.splice(..environment.scalar_types().len(), [pointer]);
        }
    }
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
    if functions[name].needs_arena {
        call_args.push(mode.arena());
    }
    if let Some(cleanup) = mode.cleanup() {
        for arg in args.iter().flatten().filter(|arg| arg.ty.is_owner()) {
            let index = builder.ins().iconst(ptr_ty, arg.owner_key() as i64);
            builder
                .ins()
                .call(refs["__joky_cleanup_forget"], &[cleanup, index]);
        }
    }
    let call = if let Some(target) = indirect {
        let info = &functions[name];
        let mut signature = Signature::new(builder.func.signature.call_conv);
        for ty in info.signature.params.iter().flat_map(Type::scalar_types) {
            signature
                .params
                .push(AbiParam::new(native_lane(ty).unwrap()));
        }
        if info.signature.ret.is_aggregate() {
            signature.params.push(AbiParam::new(ptr_ty));
        }
        signature.params.push(AbiParam::new(ptr_ty));
        if let Some(ty) = native_lane(&info.signature.ret) {
            signature.returns.push(AbiParam::new(ty));
        }
        let signature = builder.import_signature(signature);
        let address = slots.read(builder, target);
        builder.ins().call_indirect(signature, address, &call_args)
    } else {
        builder.ins().call(refs[name], &call_args)
    };
    if functions[name].may_fail {
        let status = helper(builder, refs, "joky_runtime_error_code", &[]);
        let failed = builder.create_block();
        let ready = builder.create_block();
        builder.ins().brif(status, failed, &[], ready, &[]);
        builder.switch_to_block(failed);
        let source = builder
            .ins()
            .iconst(types::I64, span.source_id().raw() as i64);
        let start = builder.ins().iconst(ptr_ty, span.start() as i64);
        let end = builder.ins().iconst(ptr_ty, span.end() as i64);
        builder
            .ins()
            .call(refs["__joky_runtime_call_failed"], &[source, start, end]);
        failure_return(builder, refs, mode.frame(), mode.cleanup());
        builder.switch_to_block(ready);
    }
    if let Some(dst) = result {
        if let Some(buffer) = buffer {
            for (offset, dst) in dst.scalars().enumerate() {
                let value =
                    builder
                        .ins()
                        .stack_load(ptr_ty, types::I64, buffer, (offset * 8) as i32);
                let ty = native_type(&dst.ty).unwrap();
                let value = unpack(builder, ty, value);
                slots.write(builder, &dst, value);
            }
        } else if !dst.ty.is_aggregate() {
            let value = builder.inst_results(call)[0];
            let value = narrow_lane(builder, &dst.ty, value);
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
    indirect: &Option<Slot>,
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
    let descriptor = indirect.as_ref().map(|target| slots.read(builder, target));
    let count = cps_slot_count(builder, &functions[name], descriptor, ptr_ty);
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
    for (to, arg) in args.iter().enumerate().filter_map(|(to, arg)| {
        arg.as_ref()
            .filter(|s| s.ty.is_suspend_safe_owner())
            .map(|s| (to, s))
    }) {
        let from = builder.ins().iconst(ptr_ty, arg.owner_key() as i64);
        let to = builder.ins().iconst(ptr_ty, to as i64);
        builder.ins().call(
            refs["__joky_cps_transfer_owner_v2"],
            &[frame, child, from, to],
        );
    }
    let saved = builder.ins().iconst(types::I32, i64::from(state));
    builder
        .ins()
        .store(MemFlagsData::new(), saved, frame, state_offset);
    builder.ins().jump(states[state as usize], &[]);
    builder.switch_to_block(states[state as usize]);
    // Re-enter only the child poll. Argument evaluation and side
    // effects before this point must not run again on resumption.
    let descriptor = indirect.as_ref().map(|target| slots.read(builder, target));
    let count = cps_slot_count(builder, &functions[name], descriptor, ptr_ty);
    let child = helper(builder, refs, "__joky_cps_child", &[frame, count]);
    let status = poll_cps(builder, refs.get(name).copied(), descriptor, child, ptr_ty);
    if functions[name].may_fail {
        check_call(builder, refs, Some(frame), None);
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
            let value = unpack(builder, native_type(&dst.ty).unwrap(), value);
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
    data: &DataRefs,
    slots: &mut SlotAccess<'_>,
    mode: ExecMode<'_>,
) -> bool {
    let labels: Vec<_> = (0..plan.label_count)
        .map(|_| builder.create_block())
        .collect();
    let mut terminated = false;
    for (op, &span) in plan.ops.iter().zip(&plan.op_spans) {
        if terminated && !matches!(op, Op::Label(_)) {
            continue;
        }
        match op {
            Op::Closure(_) => unreachable!("closure responsibility is erased after checking"),
            Op::Cleanup(op) => lower_cleanup(builder, op, plan, refs, ptr_ty, slots, mode),
            Op::Buffer(op) => match op {
                BufferOp::SwapReferences {
                    left,
                    right,
                    target,
                } => {
                    let left = slots.read(builder, left);
                    let right = slots.read(builder, right);
                    if target.is_owner() && !target.is_inline_owner() {
                        builder
                            .ins()
                            .call(refs["__joky_buffer_swap"], &[left, right]);
                    } else {
                        let size = builder
                            .ins()
                            .iconst(ptr_ty, target.storage_layout().unwrap().size as i64);
                        builder
                            .ins()
                            .call(refs["__joky_memory_swap"], &[left, right, size]);
                    }
                }
                BufferOp::Swap {
                    reference,
                    value,
                    place,
                } => {
                    let left = slots.read(builder, reference);
                    let right = slots.read(
                        builder,
                        place
                            .as_ref()
                            .unwrap_or(&value.scalars().find(|s| s.ty.is_buffer()).unwrap()),
                    );
                    if place.is_some() {
                        let size = builder
                            .ins()
                            .iconst(ptr_ty, value.ty.storage_layout().unwrap().size as i64);
                        builder
                            .ins()
                            .call(refs["__joky_memory_swap"], &[left, right, size]);
                    } else {
                        builder
                            .ins()
                            .call(refs["__joky_buffer_swap"], &[left, right]);
                    }
                }
                BufferOp::Borrow { dst, src, place } => {
                    let source = place
                        .clone()
                        .unwrap_or_else(|| src.scalars().find(|s| s.ty.is_buffer()).unwrap());
                    let value = slots.read(builder, &source);
                    slots.write(builder, dst, value);
                }
                BufferOp::Access { .. } => {
                    unreachable!("owner access markers are erased after checking")
                }
                BufferOp::Move { dst, src } => {
                    if let Some(cleanup) = mode.cleanup() {
                        let dst_key = builder.ins().iconst(ptr_ty, dst.owner_key() as i64);
                        let src_key = builder.ins().iconst(ptr_ty, src.owner_key() as i64);
                        builder
                            .ins()
                            .call(refs["__joky_cleanup_move"], &[cleanup, dst_key, src_key]);
                        check_call(builder, refs, mode.frame(), mode.cleanup());
                    }
                    for (dst, src) in dst.scalars().zip(src.scalars()) {
                        let value = slots.read(builder, &src);
                        slots.write(builder, &dst, value);
                        if dst.index != src.index && src.ty.is_buffer() {
                            let empty = builder.ins().iconst(ptr_ty, 0);
                            slots.write(builder, &src, empty);
                        }
                    }
                }
                BufferOp::Extract { dst, src, owner } | BufferOp::Detach { dst, src, owner } => {
                    let (first, count) = owner.owner_range(src);
                    let cleanup = mode.cleanup().unwrap();
                    let key = builder.ins().iconst(ptr_ty, owner.owner_key() as i64);
                    let first = builder.ins().iconst(ptr_ty, first as i64);
                    let count = builder.ins().iconst(ptr_ty, count as i64);
                    let destination = builder.ins().iconst(ptr_ty, dst.owner_key() as i64);
                    builder.ins().call(
                        refs["__joky_cleanup_extract_field"],
                        &[cleanup, destination, key, first, count],
                    );
                    for (dst, src) in dst.scalars().zip(src.scalars()) {
                        let value = slots.read(builder, &src);
                        slots.write(builder, &dst, value);
                    }
                }
                BufferOp::Replace {
                    dst,
                    src,
                    owner,
                    place,
                    path,
                } => {
                    let (first, count) = owner.owner_range(dst);
                    let cleanup = mode.cleanup().unwrap();
                    let key = builder.ins().iconst(ptr_ty, owner.owner_key() as i64);
                    let first = builder.ins().iconst(ptr_ty, first as i64);
                    let count = builder.ins().iconst(ptr_ty, count as i64);
                    let source = builder.ins().iconst(ptr_ty, src.owner_key() as i64);
                    builder.ins().call(
                        refs["__joky_cleanup_replace_field"],
                        &[cleanup, key, first, count, source],
                    );
                    check_call(builder, refs, mode.frame(), mode.cleanup());
                    let pointer = slots.read(builder, place);
                    let (base, _) = owner.ty.storage_field_path(path).unwrap();
                    for (((offset, _), dst), src) in dst
                        .ty
                        .storage_scalars()
                        .unwrap()
                        .into_iter()
                        .zip(dst.scalars())
                        .zip(src.scalars())
                    {
                        let value = slots.read(builder, &src);
                        slots.write(builder, &dst, value);
                        builder.ins().store(
                            MemFlagsData::new(),
                            value,
                            pointer,
                            (base + offset) as i32,
                        );
                        if src.ty.is_buffer() {
                            let empty = builder.ins().iconst(ptr_ty, 0);
                            slots.write(builder, &src, empty);
                        }
                    }
                }
                BufferOp::End(owner) => {
                    if let Some(cleanup) = mode.cleanup() {
                        let index = builder.ins().iconst(ptr_ty, owner.index as i64);
                        let width = builder
                            .ins()
                            .iconst(ptr_ty, owner.ty.scalar_types().len() as i64);
                        builder
                            .ins()
                            .call(refs["__joky_cleanup_run"], &[cleanup, index, width]);
                        check_call(builder, refs, mode.frame(), mode.cleanup());
                    }
                }
                BufferOp::Storage {
                    intrinsic,
                    element,
                    args,
                    dst,
                } => {
                    if let Some(cleanup) = mode.cleanup() {
                        for arg in args.iter().filter(|arg| {
                            arg.ty.is_owner()
                                && matches!(
                                    intrinsic,
                                    BufferIntrinsic::Release | BufferIntrinsic::Replace
                                )
                        }) {
                            let index = builder.ins().iconst(ptr_ty, arg.owner_key() as i64);
                            builder
                                .ins()
                                .call(refs["__joky_cleanup_forget"], &[cleanup, index]);
                        }
                    }
                    let layout = element
                        .storage_layout()
                        .expect("validated buffer element layout");
                    let mut values = Vec::new();
                    match intrinsic {
                        BufferIntrinsic::Allocate => {
                            values.push(mode.arena());
                            values.push(slots.read(builder, &args[0]));
                            values.push(builder.ins().iconst(ptr_ty, layout.size as i64));
                            values.push(builder.ins().iconst(ptr_ty, layout.align as i64));
                        }
                        BufferIntrinsic::Replace
                        | BufferIntrinsic::Release
                        | BufferIntrinsic::Truncate => {
                            values.push(mode.arena());
                            values.extend(args.iter().map(|s| slots.read(builder, s)));
                        }
                        BufferIntrinsic::CapacityLimit => {
                            values.push(builder.ins().iconst(ptr_ty, layout.size as i64));
                        }
                        intrinsic if intrinsic.borrows_storage() => {
                            if intrinsic.allocates() {
                                values.push(mode.arena());
                            }
                            values.extend(args.iter().map(|s| slots.read(builder, s)));
                        }
                        BufferIntrinsic::Store | BufferIntrinsic::SliceStore => {
                            if *intrinsic == BufferIntrinsic::Store {
                                values.push(mode.arena());
                            }
                            values.extend(args[..2].iter().map(|s| slots.read(builder, s)));
                        }
                        BufferIntrinsic::InitializeNext | BufferIntrinsic::ElementStore => {
                            values.push(slots.read(builder, &args[0]));
                        }
                        _ => values.extend(args.iter().map(|s| slots.read(builder, s))),
                    }
                    if intrinsic.may_fail() {
                        values.push(
                            builder
                                .ins()
                                .iconst(types::I64, span.source_id().raw() as i64),
                        );
                        values.push(builder.ins().iconst(ptr_ty, span.start() as i64));
                        values.push(builder.ins().iconst(ptr_ty, span.end() as i64));
                    }
                    let call = builder.ins().call(refs[intrinsic.symbol()], &values);
                    if intrinsic.may_fail() {
                        check_call(builder, refs, mode.frame(), mode.cleanup());
                    }
                    match intrinsic {
                        BufferIntrinsic::BorrowRef
                        | BufferIntrinsic::BorrowMutRef
                        | BufferIntrinsic::SliceBorrowRef
                        | BufferIntrinsic::SliceBorrowMutRef => {
                            let pointer = builder.inst_results(call)[0];
                            // One-field owner wrappers borrow the underlying handle;
                            // mixed aggregates borrow their inline element storage.
                            let pointer = if !element.is_owner() || element.is_inline_owner() {
                                pointer
                            } else {
                                builder.ins().load(ptr_ty, MemFlagsData::new(), pointer, 0)
                            };
                            slots.write(builder, dst.as_ref().unwrap(), pointer);
                        }
                        BufferIntrinsic::Load
                        | BufferIntrinsic::TakeLast
                        | BufferIntrinsic::ElementLoad
                        | BufferIntrinsic::SliceLoad => {
                            let pointer = builder.inst_results(call)[0];
                            if let Some(dst) = dst {
                                for ((offset, ty), dst) in element
                                    .storage_scalars()
                                    .unwrap()
                                    .into_iter()
                                    .zip(dst.scalars())
                                {
                                    let value = builder.ins().load(
                                        native_type(&ty).unwrap(),
                                        MemFlagsData::new(),
                                        pointer,
                                        offset as i32,
                                    );
                                    slots.write(builder, &dst, value);
                                }
                            }
                        }
                        BufferIntrinsic::Store
                        | BufferIntrinsic::InitializeNext
                        | BufferIntrinsic::ElementStore
                        | BufferIntrinsic::SliceStore => {
                            let pointer = builder.inst_results(call)[0];
                            let initializing = *intrinsic == BufferIntrinsic::InitializeNext;
                            let value = args.last().unwrap();
                            for ((offset, _), src) in element
                                .storage_scalars()
                                .unwrap()
                                .into_iter()
                                .zip(value.scalars())
                            {
                                let value = slots.read(builder, &src);
                                builder.ins().store(
                                    MemFlagsData::new(),
                                    value,
                                    pointer,
                                    offset as i32,
                                );
                            }
                            if initializing {
                                builder
                                    .ins()
                                    .call(refs["__joky_buffer_commit_next"], &values[..1]);
                            }
                        }
                        _ => {
                            if let Some(dst) = dst {
                                slots.write(builder, dst, builder.inst_results(call)[0]);
                            }
                        }
                    }
                    if let Some(cleanup) = mode.cleanup() {
                        for arg in args.iter().filter(|arg| arg.ty.is_owner()) {
                            let index = builder.ins().iconst(ptr_ty, arg.owner_key() as i64);
                            builder
                                .ins()
                                .call(refs["__joky_cleanup_forget"], &[cleanup, index]);
                        }
                    }
                }
            },
            Op::Graph(_) => unreachable!("construction operations are erased after checking"),
            Op::EndLocal(_) => unreachable!("local lifetime markers are erased after checking"),
            Op::Copy(dst, src) | Op::Reference { dst, src, .. } | Op::Reserve { dst, src } => {
                for (dst, src) in dst.scalars().zip(src.scalars()) {
                    let value = slots.read(builder, &src);
                    slots.write(builder, &dst, value);
                }
            }
            Op::DynTable { dst, table } => {
                let value = builder
                    .ins()
                    .symbol_value(ptr_ty, data.dyn_tables[&table.id]);
                slots.write(builder, dst, value);
            }
            Op::DynMethod { dst, table, method } => {
                let table = slots.read(builder, table);
                let entry = builder.ins().load(
                    ptr_ty,
                    MemFlagsData::new(),
                    table,
                    (*method * ptr_ty.bytes() as usize) as i32,
                );
                slots.write(builder, dst, entry);
            }
            Op::Project { dst, src, .. } | Op::PatternProject { dst, src, .. } => {
                let pointer = slots.read(builder, src);
                let offset = match op {
                    Op::PatternProject { offset, .. } => *offset,
                    Op::Project { path, .. } => {
                        src.ty
                            .reference_type()
                            .unwrap()
                            .target
                            .storage_field_path(path)
                            .expect("validated local field layout")
                            .0
                    }
                    _ => unreachable!(),
                };
                let pointer = builder.ins().iadd_imm_u(pointer, offset as i64);
                let target = &dst.ty.reference_type().unwrap().target;
                let pointer = if target.is_owner() && !target.is_inline_owner() {
                    builder.ins().load(ptr_ty, MemFlagsData::new(), pointer, 0)
                } else {
                    pointer
                };
                slots.write(builder, dst, pointer);
            }
            Op::Alloc { dst, value, target } | Op::Local { dst, value, target } => {
                if matches!(op, Op::Local { .. }) && target.is_closure_value() && value.is_none() {
                    let zero = builder.ins().iconst(ptr_ty, 0);
                    slots.write(builder, dst, zero);
                    continue;
                }
                let layout = target.storage_layout().expect("validated memory layout");
                let pointer = if matches!(op, Op::Local { .. }) {
                    if let SlotAccess::Frames { slots } = &*slots {
                        let offset = plan.cps_local_slots[&dst.index] * 8;
                        builder.ins().iadd_imm_u(*slots, offset as i64)
                    } else {
                        let storage = builder.create_sized_stack_slot(StackSlotData::new(
                            StackSlotKind::ExplicitSlot,
                            layout.size.max(1) as u32,
                            layout.align.trailing_zeros() as u8,
                        ));
                        builder.ins().stack_addr(ptr_ty, storage, 0)
                    }
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
                    check_call(builder, refs, mode.frame(), mode.cleanup());
                    pointer
                };
                if let Some(value) = value {
                    for ((offset, _), src) in target
                        .storage_scalars()
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
            }
            | Op::ReadOwner {
                dst,
                reference,
                path,
                ..
            } => {
                if let Op::ReadOwner {
                    owner,
                    ownership_path,
                    ..
                } = op
                {
                    if let Some(cleanup) = mode.cleanup() {
                        let index = builder.ins().iconst(ptr_ty, owner.owner_key() as i64);
                        let (first, count) = owner.owner_range(&owner.field_path(ownership_path));
                        let first = builder.ins().iconst(ptr_ty, first as i64);
                        let count = builder.ins().iconst(ptr_ty, count as i64);
                        let destination = builder
                            .ins()
                            .iconst(ptr_ty, dst.as_ref().unwrap().owner_key() as i64);
                        builder.ins().call(
                            refs["__joky_cleanup_extract_field"],
                            &[cleanup, destination, index, first, count],
                        );
                    }
                }
                let pointer = slots.read(builder, reference);
                let (base, target) = reference
                    .ty
                    .reference_type()
                    .unwrap()
                    .target
                    .storage_field_path(path)
                    .unwrap();
                if let Some(dst) = dst {
                    for ((offset, ty), dst) in target
                        .storage_scalars()
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
            }
            | Op::StoreOwner {
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
                    .storage_field_path(path)
                    .unwrap();
                if let Some(value) = value {
                    for ((offset, _), src) in target
                        .storage_scalars()
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
            Op::Text { dst, source } => {
                let (name, mut args) = match source {
                    TextSource::Literal(text) => {
                        let view = text_view_slot(builder, ptr_ty);
                        let bytes = builder.ins().symbol_value(ptr_ty, data.strings[text]);
                        builder.ins().store(MemFlagsData::new(), bytes, view, 0);
                        let length = builder.ins().iconst(types::I64, text.len() as i64);
                        builder.ins().store(
                            MemFlagsData::new(),
                            length,
                            view,
                            std::mem::offset_of!(crate::runtime::string::JokyText, byte_length)
                                as i32,
                        );
                        ("__joky_string_from_view_v2", vec![mode.arena(), view])
                    }
                    TextSource::Clone(src) => (
                        "__joky_string_clone_v2",
                        vec![mode.arena(), slots.read(builder, src)],
                    ),
                    TextSource::Float(src) => {
                        // Slots hold the raw bit pattern; the runtime formats
                        // per width, so Float32 stays zero-extended here.
                        let value = slots.read(builder, src);
                        let bits = wide(builder, value);
                        let width = builder
                            .ins()
                            .iconst(types::I32, if src.ty == Type::Float64 { 64 } else { 32 });
                        ("__joky_float_to_text_v2", vec![mode.arena(), bits, width])
                    }
                };
                args.push(
                    builder
                        .ins()
                        .iconst(types::I64, span.source_id().raw() as i64),
                );
                args.push(builder.ins().iconst(ptr_ty, span.start() as i64));
                args.push(builder.ins().iconst(ptr_ty, span.end() as i64));
                let value = helper(builder, refs, name, &args);
                check_call(builder, refs, mode.frame(), mode.cleanup());
                slots.write(builder, dst, value);
            }
            Op::Literal(dst, literal) => {
                let value = lower_literal(builder, literal, &data.strings);
                slots.write(builder, dst, value);
            }
            Op::DomainBits { dst, src, decode } => {
                let value = slots.read(builder, src);
                let value = if *decode {
                    unpack(builder, native_type(&dst.ty).unwrap(), value)
                } else {
                    wide(builder, value)
                };
                slots.write(builder, dst, value);
            }
            Op::DomainModule(dst) => {
                let value = builder
                    .ins()
                    .symbol_value(ptr_ty, data.domain.expect("domain module descriptor"));
                slots.write(builder, dst, value);
            }
            Op::FunctionRef(dst, name) => {
                let value = if matches!(&dst.ty, Type::Function(ty) if ty.suspends) {
                    builder.ins().symbol_value(ptr_ty, data.callables[name])
                } else {
                    let entry = if matches!(dst.ty, Type::Function(_)) {
                        refs[name]
                    } else {
                        *refs
                            .get(&format!("joky/callback/{name}"))
                            .unwrap_or(&refs[name])
                    };
                    builder.ins().func_addr(ptr_ty, entry)
                };
                slots.write(builder, dst, value);
            }
            Op::Scope {
                result,
                kind,
                branches,
                state,
            } => {
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
                        plan,
                        mode,
                        result: result.as_ref(),
                        kind: *kind,
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
            Op::ScopeFinish => {
                let frame = mode.frame().expect("structured scope requires CPS");
                builder
                    .ins()
                    .call(refs["__joky_cps_release_children"], &[frame]);
                check_call(builder, refs, Some(frame), None);
            }
            Op::Panic { message } => {
                let message = slots.read(builder, message);
                let source = builder
                    .ins()
                    .iconst(types::I64, span.source_id().raw() as i64);
                let start = builder.ins().iconst(ptr_ty, span.start() as i64);
                let end = builder.ins().iconst(ptr_ty, span.end() as i64);
                builder.ins().call(
                    refs["__joky_runtime_assert_failed_v2"],
                    &[message, source, start, end],
                );
                failure_return(builder, refs, mode.frame(), mode.cleanup());
                terminated = true;
            }
            Op::Assert { condition, message } => {
                let condition = slots.read(builder, condition);
                let message = slots.read(builder, message);
                let failed = builder.create_block();
                let ready = builder.create_block();
                builder.ins().brif(condition, ready, &[], failed, &[]);
                builder.switch_to_block(failed);
                let source = builder
                    .ins()
                    .iconst(types::I64, span.source_id().raw() as i64);
                let start = builder.ins().iconst(ptr_ty, span.start() as i64);
                let end = builder.ins().iconst(ptr_ty, span.end() as i64);
                builder.ins().call(
                    refs["__joky_runtime_assert_failed_v2"],
                    &[message, source, start, end],
                );
                failure_return(builder, refs, mode.frame(), mode.cleanup());
                builder.switch_to_block(ready);
            }
            Op::Binary(dst, op, lhs, rhs) => {
                let unsigned = lhs.ty.integer().is_some_and(|integer| !integer.signed());
                let lhs = slots.read(builder, lhs);
                let rhs = slots.read(builder, rhs);
                if op.can_fail() && !builder.func.dfg.value_type(lhs).is_float() {
                    check_integer_operand(
                        builder,
                        refs,
                        mode.frame(),
                        mode.cleanup(),
                        rhs,
                        op,
                        span,
                        ptr_ty,
                    );
                }
                let value = lower_binary(builder, op, lhs, rhs, unsigned);
                slots.write(builder, dst, value);
            }
            Op::Convert(dst, src) => {
                let value = slots.read(builder, src);
                let value = lower_conversion(builder, dst, src, value);
                slots.write(builder, dst, value);
            }
            Op::Float { dst, src, kind } => {
                let value = slots.read(builder, src);
                let value = if *kind == FloatKind::FromText {
                    let bits = helper(builder, refs, "__joky_float_from_text_v1", &[value]);
                    check_call(builder, refs, mode.frame(), mode.cleanup());
                    builder.ins().bitcast(types::F64, MemFlagsData::new(), bits)
                } else {
                    lower_float(builder, *kind, value)
                };
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
                indirect,
            } => {
                lower_sync_call(
                    builder, slots, mode, name, indirect, args, result, functions, refs, ptr_ty,
                    span,
                );
            }
            Op::Call {
                name,
                args,
                result,
                state: Some(state),
                indirect,
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
                    indirect,
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
            Op::TaskScopeEnter => {
                let ExecMode::Suspends { frame, .. } = mode else {
                    unreachable!("task scopes are only valid in suspending functions")
                };
                builder
                    .ins()
                    .call(refs["__joky_cps_task_scope_enter"], &[frame]);
            }
            Op::TaskBranch {
                entry, captures, ..
            } => {
                let ExecMode::Suspends { frame, .. } = mode else {
                    unreachable!("task branches are only valid in suspending functions")
                };
                let SlotAccess::Frames { slots } = &*slots else {
                    unreachable!("task branches address frame memory")
                };
                // Registration snapshots the captures: values are copied into
                // the child frame and owning inputs move their cleanup duty.
                for capture in captures.iter().flatten().filter(|s| s.ty.is_owner()) {
                    let key = builder.ins().iconst(ptr_ty, capture.owner_key() as i64);
                    builder.ins().call(
                        refs["__joky_cleanup_snapshot"],
                        &[mode.cleanup().unwrap(), key],
                    );
                    check_call(builder, refs, Some(frame), None);
                }
                let count = cps_slot_count(builder, &functions[entry], None, ptr_ty);
                let child = helper(builder, refs, "__joky_cps_task_child", &[frame, count]);
                // A null child means the live-task quota was exhausted and a
                // controlled runtime error is already recorded.
                check_call(builder, refs, Some(frame), None);
                let child_slots = helper(builder, refs, "__joky_cps_slots", &[child]);
                for (slot_index, capture) in captures.iter().enumerate() {
                    if let Some(capture) = capture {
                        let value = load(builder, *slots, capture);
                        store(
                            builder,
                            child_slots,
                            &Slot {
                                index: slot_index,
                                ty: capture.ty.clone(),
                            },
                            value,
                        );
                    }
                }
                for (to, capture) in captures.iter().enumerate().filter_map(|(to, capture)| {
                    capture
                        .as_ref()
                        .filter(|s| s.ty.is_suspend_safe_owner())
                        .map(|s| (to, s))
                }) {
                    let from = builder.ins().iconst(ptr_ty, capture.owner_key() as i64);
                    let to = builder.ins().iconst(ptr_ty, to as i64);
                    builder.ins().call(
                        refs["__joky_cps_transfer_owner_v2"],
                        &[frame, child, from, to],
                    );
                }
                let entry_addr = builder.ins().func_addr(ptr_ty, refs[entry]);
                builder.ins().call(
                    refs["__joky_cps_task_branch_commit"],
                    &[frame, child, entry_addr],
                );
            }
            Op::TaskJoin { state, .. } => {
                let ExecMode::Suspends {
                    frame,
                    states,
                    state_offset,
                    ..
                } = mode
                else {
                    unreachable!("task joins are only valid in suspending functions")
                };
                let saved = builder.ins().iconst(types::I32, *state as i64);
                builder
                    .ins()
                    .store(MemFlagsData::new(), saved, frame, state_offset);
                builder.ins().jump(states[*state as usize], &[]);
                builder.switch_to_block(states[*state as usize]);
                let status = helper(builder, refs, "__joky_cps_task_join", &[frame]);
                let zero = builder.ins().iconst(types::I32, 0);
                let completed = builder.ins().icmp(IntCC::Equal, status, zero);
                let status_check = builder.create_block();
                let pending = builder.create_block();
                let failure = builder.create_block();
                let continuation = builder.create_block();
                builder
                    .ins()
                    .brif(completed, continuation, &[], status_check, &[]);
                builder.switch_to_block(status_check);
                let one = builder.ins().iconst(types::I32, 1);
                let is_pending = builder.ins().icmp(IntCC::Equal, status, one);
                builder.ins().brif(is_pending, pending, &[], failure, &[]);
                builder.switch_to_block(pending);
                builder.ins().return_(&[one]);
                builder.switch_to_block(failure);
                builder.ins().call(refs["__joky_cps_finish"], &[frame]);
                builder.ins().return_(&[status]);
                builder.switch_to_block(continuation);
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
    data: &DataRefs,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let frame = builder.block_params(entry)[0];
    let slots = helper(builder, refs, "__joky_cps_slots", &[frame]);
    let arena = info
        .needs_arena
        .then(|| helper(builder, refs, "__joky_cps_arena", &[frame]));
    let cleanup = plan
        .has_cleanups()
        .then(|| helper(builder, refs, "__joky_cps_cleanup_v2", &[frame]));
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
        cleanup,
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
        data,
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
        if let Some(result) = plan.result.as_ref().filter(|s| s.ty.is_owner()) {
            if result.ty == Type::String {
                let value = load(builder, slots, result);
                builder
                    .ins()
                    .call(refs["__joky_cps_capture_text_v2"], &[frame, value]);
                check_call(builder, refs, Some(frame), None);
            }
            if cleanup.is_some() {
                let key = builder.ins().iconst(ptr_ty, result.owner_key() as i64);
                builder
                    .ins()
                    .call(refs["__joky_cps_finish_return_v1"], &[frame, key]);
            }
        }
        let done = builder.ins().iconst(types::I32, DONE as i64);
        builder
            .ins()
            .store(MemFlagsData::new(), done, frame, state_offset);
        let status = builder.ins().iconst(types::I32, 0);
        builder.ins().call(refs["__joky_cps_finish"], &[frame]);
        check_call(builder, refs, Some(frame), None);
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
    data: &DataRefs,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let mut variables = vec![None; plan.slot_count];
    let mut parameters = builder.block_params(entry).to_vec().into_iter();
    let mut index = 0;
    for (parameter_index, ty) in info.signature.params.iter().enumerate() {
        let parameter = Slot {
            index,
            ty: ty.clone(),
        };
        if parameter_index == 0 && info.closure_entry && ty.closure_info().is_some() {
            let pointer = parameters.next().unwrap();
            for ((offset, scalar), slot) in ty
                .storage_scalars()
                .unwrap()
                .into_iter()
                .zip(parameter.scalars())
            {
                let value = builder.ins().load(
                    native_type(&scalar).unwrap(),
                    MemFlagsData::new(),
                    pointer,
                    offset as i32,
                );
                define_slot(builder, &mut variables, &slot, value);
            }
        } else {
            for slot in parameter.scalars() {
                let value = narrow_lane(builder, &slot.ty, parameters.next().unwrap());
                define_slot(builder, &mut variables, &slot, value);
            }
        }
        index += ty.parameter_slots();
    }
    let result_pointer = info
        .signature
        .ret
        .is_aggregate()
        .then(|| parameters.next().unwrap());

    let arena = info.needs_arena.then(|| parameters.next().unwrap());
    let cleanup = plan
        .has_cleanups()
        .then(|| helper(builder, refs, "__joky_cleanup_new", &[arena.unwrap()]));
    let mut access = SlotAccess::Ssa {
        variables: &mut variables,
    };
    let terminated = lower_ops(
        builder,
        plan,
        functions,
        refs,
        ptr_ty,
        data,
        &mut access,
        ExecMode::Direct { arena, cleanup },
    );
    if !terminated {
        if let Some(cleanup) = cleanup {
            if let Some(result) = plan.result.as_ref().filter(|s| s.ty.is_owner()) {
                let owner = result.scalars().find(|s| s.ty.is_buffer()).unwrap();
                let index = builder.ins().iconst(ptr_ty, owner.index as i64);
                builder
                    .ins()
                    .call(refs["__joky_cleanup_finish_return"], &[cleanup, index]);
            } else {
                builder
                    .ins()
                    .call(refs["__joky_cleanup_finish"], &[cleanup]);
            }
            check_call(builder, refs, None, None);
        }
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
                .map(|slot| {
                    let value = builder.use_var(variables[slot.index].unwrap());
                    widen_lane(builder, &slot.ty, value)
                })
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
    data: &DataRefs,
) {
    if info.suspends {
        lower_cps(builder, info, plan, functions, refs, ptr_ty, data);
    } else {
        lower_direct(builder, info, plan, functions, refs, ptr_ty, data);
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
    failure_return(builder, refs, None, None);
    builder.switch_to_block(ready);
    args.push(arena);
    let call = builder.ins().call(body, &args);
    let result = builder.inst_results(call).to_vec();
    builder.ins().call(refs["__joky_arena_drop"], &[arena]);
    builder.ins().return_(&result);
    builder.seal_all_blocks();
}

fn text_view_slot(builder: &mut FunctionBuilder, ptr: types::Type) -> Value {
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        std::mem::size_of::<crate::runtime::string::JokyText>() as u32,
        std::mem::align_of::<crate::runtime::string::JokyText>().trailing_zeros() as u8,
    ));
    builder.ins().stack_addr(ptr, slot, 0)
}

/// A registered C text descriptor uses the same owning task ABI as language code.
pub(super) fn text_ffi_entry(
    builder: &mut FunctionBuilder,
    descriptor: cranelift_codegen::ir::GlobalValue,
    info: &FunctionInfo,
    refs: &HashMap<String, FuncRef>,
    ptr: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let mut args = builder.block_params(entry).to_vec();
    let arena = args.pop().unwrap();
    let cleanup = helper(builder, refs, "__joky_cleanup_new", &[arena]);
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (args.len().max(1) as u32) * ptr.bytes(),
        ptr.bytes().trailing_zeros() as u8,
    ));
    for (index, (&arg, ty)) in args.iter().zip(&info.signature.params).enumerate() {
        builder
            .ins()
            .stack_store(ptr, arg, slot, index as i32 * ptr.bytes() as i32);
        if *ty == Type::String {
            let key = builder.ins().iconst(ptr, index as i64);
            builder
                .ins()
                .call(refs["__joky_cleanup_register"], &[cleanup, key, arg]);
        }
    }
    let inputs = builder.ins().stack_addr(ptr, slot, 0);
    let descriptor = builder.ins().symbol_value(ptr, descriptor);
    let count = builder.ins().iconst(ptr, args.len() as i64);
    let scalar = builder
        .ins()
        .iconst(types::I32, i64::from(info.signature.ret != Type::String));
    let result = helper(
        builder,
        refs,
        "__joky_text_ffi_call_v1",
        &[arena, descriptor, inputs, count, scalar],
    );
    check_call(builder, refs, None, Some(cleanup));
    builder
        .ins()
        .call(refs["__joky_cleanup_finish"], &[cleanup]);
    check_call(builder, refs, None, None);
    if info.signature.ret == Type::Unit {
        builder.ins().return_(&[]);
    } else {
        let ty = builder.func.signature.returns[0].value_type;
        let result = if ty.bits() < 64 {
            builder.ins().ireduce(ty, result)
        } else {
            result
        };
        builder.ins().return_(&[result]);
    }
    builder.seal_all_blocks();
}

/// A logical text call always uses this adapter, never the host wire address.
pub(super) fn host_text_direct_entry(
    builder: &mut FunctionBuilder,
    body: FuncRef,
    info: &FunctionInfo,
    refs: &HashMap<String, FuncRef>,
    ptr: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let mut args = builder.block_params(entry).to_vec();
    let arena = args.pop().unwrap();
    let cleanup = helper(builder, refs, "__joky_cleanup_new", &[arena]);
    let mut index = 0;
    for ty in &info.signature.params {
        if *ty == Type::Unit {
            continue;
        }
        if *ty == Type::String {
            let key = builder.ins().iconst(ptr, index as i64);
            builder.ins().call(
                refs["__joky_cleanup_register"],
                &[cleanup, key, args[index]],
            );
        }
        index += 1;
    }
    index = 0;
    for ty in &info.signature.params {
        if *ty == Type::Unit {
            continue;
        }
        if *ty == Type::String
            || ty
                .reference_type()
                .is_some_and(|r| r.target == Type::String)
        {
            let view = text_view_slot(builder, ptr);
            builder
                .ins()
                .call(refs["__joky_string_view_v2"], &[args[index], view]);
            args[index] = view;
            check_call(builder, refs, None, Some(cleanup));
        }
        index += 1;
    }
    let call = builder.ins().call(body, &args);
    let mut result = builder.inst_results(call).to_vec();
    // Never inspect a returned pointer until the host error channel has passed.
    check_call(builder, refs, None, Some(cleanup));
    if info.signature.ret == Type::String {
        let source = builder
            .ins()
            .iconst(types::I64, info.span.source_id().raw() as i64);
        let start = builder.ins().iconst(ptr, info.span.start() as i64);
        let end = builder.ins().iconst(ptr, info.span.end() as i64);
        result[0] = helper(
            builder,
            refs,
            "__joky_string_from_view_v2",
            &[arena, result[0], source, start, end],
        );
        check_call(builder, refs, None, Some(cleanup));
    }
    builder
        .ins()
        .call(refs["__joky_cleanup_finish"], &[cleanup]);
    check_call(builder, refs, None, None);
    builder.ins().return_(&result);
    builder.seal_all_blocks();
}

/// Poll the host child with stable borrowed descriptors and owned input lifetimes.
pub(super) fn host_text_cps_entry(
    builder: &mut FunctionBuilder,
    body: FuncRef,
    info: &FunctionInfo,
    refs: &HashMap<String, FuncRef>,
    ptr: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let frame = builder.block_params(entry)[0];
    let first = builder.create_block();
    let resumed = builder.create_block();
    let poll = builder.create_block();
    builder.append_block_param(poll, ptr);
    let state = builder.ins().load(
        types::I32,
        MemFlagsData::new(),
        frame,
        std::mem::offset_of!(CpsFrame, state) as i32,
    );
    let count = builder.ins().iconst(ptr, info.frame_slots as i64);
    builder.ins().brif(state, resumed, &[], first, &[]);
    builder.switch_to_block(first);
    let params = builder.ins().iconst(ptr, info.parameter_slots() as i64);
    let child = helper(
        builder,
        refs,
        "__joky_text_cps_prepare_v2",
        &[frame, count, params],
    );
    for (index, ty) in info.signature.params.iter().enumerate() {
        if *ty == Type::String {
            let index = builder.ins().iconst(ptr, index as i64);
            builder
                .ins()
                .call(refs["__joky_text_cps_argument_v2"], &[child, index]);
            check_call(builder, refs, Some(frame), None);
        }
    }
    check_call(builder, refs, Some(frame), None);
    builder.ins().jump(poll, &[child.into()]);
    builder.switch_to_block(resumed);
    let child = helper(builder, refs, "__joky_cps_child", &[frame, count]);
    builder.ins().jump(poll, &[child.into()]);
    builder.switch_to_block(poll);
    let child = builder.block_params(poll)[0];
    let call = builder.ins().call(body, &[child]);
    let status = builder.inst_results(call)[0];
    check_call(builder, refs, Some(frame), None);
    let pending = builder.create_block();
    let terminal = builder.create_block();
    let is_pending = builder.ins().icmp_imm_u(IntCC::Equal, status, 1);
    builder.ins().brif(is_pending, pending, &[], terminal, &[]);
    builder.switch_to_block(pending);
    builder.ins().return_(&[status]);
    builder.switch_to_block(terminal);
    let success = builder.create_block();
    let failure = builder.create_block();
    builder.ins().brif(status, failure, &[], success, &[]);
    builder.switch_to_block(failure);
    builder.ins().call(refs["__joky_cps_finish"], &[frame]);
    builder.ins().return_(&[status]);
    builder.switch_to_block(success);
    let mut value = builder.ins().load(
        types::I64,
        MemFlagsData::new(),
        child,
        std::mem::offset_of!(CpsFrame, result) as i32,
    );
    if info.signature.ret == Type::String {
        let arena = helper(builder, refs, "__joky_cps_arena", &[frame]);
        let source = builder
            .ins()
            .iconst(types::I64, info.span.source_id().raw() as i64);
        let start = builder.ins().iconst(ptr, info.span.start() as i64);
        let end = builder.ins().iconst(ptr, info.span.end() as i64);
        if ptr != types::I64 {
            value = builder.ins().ireduce(ptr, value);
        }
        value = helper(
            builder,
            refs,
            "__joky_string_from_view_v2",
            &[arena, value, source, start, end],
        );
        check_call(builder, refs, Some(frame), None);
        value = wide(builder, value);
    }
    builder.ins().store(
        MemFlagsData::new(),
        value,
        frame,
        std::mem::offset_of!(CpsFrame, result) as i32,
    );
    let done = builder.ins().iconst(types::I32, DONE as i64);
    builder.ins().store(
        MemFlagsData::new(),
        done,
        frame,
        std::mem::offset_of!(CpsFrame, state) as i32,
    );
    builder.ins().call(refs["__joky_cps_finish"], &[frame]);
    check_call(builder, refs, Some(frame), None);
    let zero = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[zero]);
    builder.seal_all_blocks();
}

/// Public v2 roots expand each text input to data/length, and use status + output.
pub(super) fn text_signature<M: Module>(module: &M, info: &FunctionInfo) -> Signature {
    if info.suspends {
        return signature(module, info);
    }
    let mut sig = module.make_signature();
    let ptr = module.target_config().pointer_type();
    for ty in &info.signature.params {
        if *ty == Type::String {
            sig.params
                .extend([ptr, types::I64].into_iter().map(AbiParam::new));
        } else {
            sig.params.extend(
                ty.scalar_types()
                    .iter()
                    .map(|ty| AbiParam::new(native_type(ty).unwrap())),
            );
        }
    }
    if info.signature.ret != Type::Unit {
        sig.params.push(AbiParam::new(ptr));
    }
    sig.returns.push(AbiParam::new(types::I32));
    sig
}

pub(super) fn text_entry(
    builder: &mut FunctionBuilder,
    body: FuncRef,
    info: &FunctionInfo,
    refs: &HashMap<String, FuncRef>,
    ptr: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let parameters = builder.block_params(entry).to_vec();
    if info.suspends {
        let frame = parameters[0];
        if info.signature.ret == Type::String {
            let flag = builder.ins().iconst(types::I8, 1);
            builder.ins().store(
                MemFlagsData::new(),
                flag,
                frame,
                std::mem::offset_of!(CpsFrame, root_text) as i32,
            );
        }
        let call = builder.ins().call(body, &[frame]);
        let status = builder.inst_results(call)[0];
        builder.ins().return_(&[status]);
        builder.seal_all_blocks();
        return;
    }
    let output = (info.signature.ret != Type::Unit).then(|| *parameters.last().unwrap());
    if let Some(output) = output {
        if info.signature.ret == Type::String {
            let zero = builder.ins().iconst(ptr, 0);
            builder.ins().store(MemFlagsData::new(), zero, output, 0);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().store(
                MemFlagsData::new(),
                zero,
                output,
                std::mem::offset_of!(crate::runtime::owned_text::JokyOwnedTextV2, byte_length)
                    as i32,
            );
        } else {
            let zero = zero_of(builder, native_type(&info.signature.ret).unwrap());
            builder.ins().store(MemFlagsData::new(), zero, output, 0);
        }
    }
    let arena = helper(builder, refs, "__joky_arena_new", &[]);
    let cleanup = helper(builder, refs, "__joky_cleanup_new", &[arena]);
    let failed = builder.create_block();
    let mut args = Vec::new();
    let mut cursor = 0;
    let mut text_keys = Vec::new();
    let check = |builder: &mut FunctionBuilder| {
        let error = helper(builder, refs, "joky_runtime_error_code", &[]);
        let ready = builder.create_block();
        builder.ins().brif(error, failed, &[], ready, &[]);
        builder.switch_to_block(ready);
    };
    check(builder);
    for ty in &info.signature.params {
        if *ty == Type::Unit {
            continue;
        }
        if *ty == Type::String {
            let owner = helper(
                builder,
                refs,
                "__joky_string_input_v2",
                &[arena, parameters[cursor], parameters[cursor + 1]],
            );
            check(builder);
            let key = builder.ins().iconst(ptr, args.len() as i64);
            builder
                .ins()
                .call(refs["__joky_cleanup_register"], &[cleanup, key, owner]);
            text_keys.push(key);
            args.push(owner);
            cursor += 2;
        } else {
            args.push(parameters[cursor]);
            cursor += 1;
        }
    }
    for key in text_keys {
        builder
            .ins()
            .call(refs["__joky_cleanup_forget"], &[cleanup, key]);
    }
    args.push(arena);
    let call = builder.ins().call(body, &args);
    let result = builder.inst_results(call).to_vec();
    check(builder);
    if let Some(output) = output {
        if info.signature.ret == Type::String {
            builder
                .ins()
                .call(refs["__joky_string_result_copy_v2"], &[result[0], output]);
            check(builder);
        } else {
            builder
                .ins()
                .store(MemFlagsData::new(), result[0], output, 0);
        }
    }
    builder
        .ins()
        .call(refs["__joky_cleanup_finish"], &[cleanup]);
    builder.ins().call(refs["__joky_arena_drop"], &[arena]);
    let zero = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[zero]);
    builder.switch_to_block(failed);
    builder
        .ins()
        .call(refs["__joky_cleanup_finish"], &[cleanup]);
    builder.ins().call(refs["__joky_arena_drop"], &[arena]);
    if let Some(output) = output.filter(|_| info.signature.ret == Type::String) {
        builder
            .ins()
            .call(refs["joky_string_result_free_v2"], &[output]);
    }
    let status = helper(builder, refs, "joky_runtime_error_code", &[]);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

/// Managed resource factories adopt the host token before returning to checked code.
pub(super) fn resource_factory_entry(
    builder: &mut FunctionBuilder,
    suspends: bool,
    byte_io: Option<crate::runtime::byte_request::ByteIoKind>,
    descriptor: GlobalValue,
    refs: &HashMap<String, FuncRef>,
    ptr: types::Type,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let args = builder.block_params(entry).to_vec();
    let descriptor = builder.ins().symbol_value(ptr, descriptor);
    if let Some(kind) = byte_io {
        let kind = builder.ins().iconst(types::I32, kind as i64);
        let status = helper(
            builder,
            refs,
            "__joky_resource_io_poll_v1",
            &[args[0], descriptor, kind],
        );
        builder.ins().return_(&[status]);
        builder.seal_all_blocks();
        return;
    }
    if suspends {
        let status = helper(
            builder,
            refs,
            "__joky_resource_poll_v1",
            &[args[0], descriptor],
        );
        builder.ins().return_(&[status]);
        builder.seal_all_blocks();
        return;
    }
    let result = helper(
        builder,
        refs,
        "__joky_resource_create_v1",
        &[args[1], descriptor, args[0]],
    );
    check_call(builder, refs, None, None);
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
}

pub(super) fn tcp_entry(
    builder: &mut FunctionBuilder,
    op: crate::runtime::tcp::Operation,
    refs: &HashMap<String, FuncRef>,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let frame = builder.block_params(entry)[0];
    let kind = builder.ins().iconst(types::I32, op as i64);
    let status = helper(builder, refs, "__joky_tcp_poll", &[frame, kind]);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

pub(super) fn dns_entry(builder: &mut FunctionBuilder, refs: &HashMap<String, FuncRef>) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let frame = builder.block_params(entry)[0];
    let status = helper(builder, refs, "__joky_dns_poll", &[frame]);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}

pub(super) fn domain_entry(
    builder: &mut FunctionBuilder,
    op: crate::runtime::domain::Operation,
    refs: &HashMap<String, FuncRef>,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let frame = builder.block_params(entry)[0];
    let kind = builder.ins().iconst(types::I32, op as i64);
    let status = helper(builder, refs, "__joky_domain_poll_v2", &[frame, kind]);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
}
