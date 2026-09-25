use cranelift_codegen::ir::FuncRef;
use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    types, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, TrapCode, Value,
};
use cranelift_frontend::FunctionBuilder;

use crate::mir::NumericMethod;
use crate::sema::Type;
use crate::syntax::{ArithmeticMode, ArithmeticOp, BinaryOp};
use crate::Diagnostic;

use super::environment::CompiledValue;
use super::types::cranelift_type;

pub(super) struct NumericValue {
    pub(super) value: Value,
    pub(super) ty: Type,
}

pub(super) fn expect_numeric(value: CompiledValue) -> Result<NumericValue, Diagnostic> {
    match value {
        CompiledValue::Numeric { value, ty } => Ok(NumericValue { value, ty }),
        _ => Err(Diagnostic::codegen("expected a numeric value")),
    }
}

pub(super) fn compile_unary_value(
    builder: &mut FunctionBuilder<'_>,
    operator: crate::syntax::UnaryOp,
    operand: CompiledValue,
    result_type: Type,
    pointer_type: cranelift_codegen::ir::Type,
    panic_ref: FuncRef,
) -> Result<CompiledValue, Diagnostic> {
    if operator == crate::syntax::UnaryOp::Not {
        let CompiledValue::Boolean { value } = operand else {
            return Err(Diagnostic::codegen("logical not requires a boolean value"));
        };
        if result_type != Type::Bool {
            return Err(Diagnostic::codegen("logical not has a non-boolean result"));
        }
        return Ok(CompiledValue::Boolean {
            value: builder.ins().bxor_imm_u(value, 1),
        });
    }
    let NumericValue { value, ty } = expect_numeric(operand)?;
    if operator == crate::syntax::UnaryOp::BitNot {
        if !ty.is_integer() || ty != result_type {
            return Err(Diagnostic::codegen("bitwise not requires an integer"));
        }
        return Ok(CompiledValue::Numeric {
            value: builder.ins().bnot(value),
            ty: result_type,
        });
    }
    if operator != crate::syntax::UnaryOp::Negate || ty != result_type {
        return Err(Diagnostic::codegen("invalid unary MIR operation"));
    }
    let value = if result_type.is_float() {
        builder.ins().fneg(value)
    } else {
        let native = builder.func.dfg.value_type(value);
        let width = native.bits();
        let minimum = builder.ins().iconst(native, (1u64 << (width - 1)) as i64);
        let overflow = builder.ins().icmp(IntCC::Equal, value, minimum);
        panic_if(
            builder,
            overflow,
            b"integer arithmetic overflow",
            pointer_type,
            panic_ref,
        );
        builder.ins().ineg(value)
    };
    Ok(CompiledValue::Numeric {
        value,
        ty: result_type,
    })
}

pub(super) fn compile_binary_value(
    builder: &mut FunctionBuilder<'_>,
    operator: BinaryOp,
    left: CompiledValue,
    right: CompiledValue,
    result_type: Type,
    pointer_type: cranelift_codegen::ir::Type,
    panic_ref: FuncRef,
) -> Result<CompiledValue, Diagnostic> {
    if operator.is_comparison() {
        let value = match (left, right) {
            (
                CompiledValue::Numeric {
                    value: left,
                    ty: left_type,
                },
                CompiledValue::Numeric {
                    value: right,
                    ty: right_type,
                },
            ) if left_type == right_type && result_type == Type::Bool => {
                if left_type.is_float() {
                    builder.ins().fcmp(float_condition(operator)?, left, right)
                } else {
                    builder
                        .ins()
                        .icmp(int_condition(operator, left_type)?, left, right)
                }
            }
            (CompiledValue::Boolean { value: left }, CompiledValue::Boolean { value: right })
                if result_type == Type::Bool =>
            {
                builder
                    .ins()
                    .icmp(int_condition(operator, Type::Bool)?, left, right)
            }
            _ => {
                return Err(Diagnostic::codegen(
                    "comparison operands have incompatible values",
                ))
            }
        };
        return Ok(CompiledValue::Boolean { value });
    }

    let NumericValue {
        value: left_value,
        ty: left_type,
    } = expect_numeric(left)?;
    let NumericValue {
        value: right_value,
        ty: right_type,
    } = expect_numeric(right)?;
    if left_type != right_type || left_type != result_type {
        return Err(Diagnostic::codegen("binary operands have different types"));
    }
    let value = if left_type.is_integer() && operator.arithmetic().is_some() {
        let (op, mode) = operator.arithmetic().unwrap();
        let (value, overflow, zero) =
            integer_arithmetic(builder, op, left_value, right_value, left_type);
        if matches!(op, ArithmeticOp::Divide | ArithmeticOp::Remainder) {
            panic_if(builder, zero, b"division by zero", pointer_type, panic_ref);
        }
        match mode {
            ArithmeticMode::Panic => {
                panic_if(
                    builder,
                    overflow,
                    b"integer arithmetic overflow",
                    pointer_type,
                    panic_ref,
                );
                value
            }
            ArithmeticMode::Wrapping => value,
            ArithmeticMode::Saturating => {
                let bound = saturation_bound(builder, op, left_value, right_value, left_type);
                builder.ins().select(overflow, bound, value)
            }
            ArithmeticMode::Checked => {
                return Err(Diagnostic::codegen("checked arithmetic was not lowered"))
            }
        }
    } else if left_type.is_float() {
        match operator {
            BinaryOp::Add => builder.ins().fadd(left_value, right_value),
            BinaryOp::Subtract => builder.ins().fsub(left_value, right_value),
            BinaryOp::Multiply => builder.ins().fmul(left_value, right_value),
            BinaryOp::Divide => builder.ins().fdiv(left_value, right_value),
            BinaryOp::Remainder => float_remainder(builder, left_value, right_value),
            _ => {
                return Err(Diagnostic::codegen(
                    "invalid floating-point binary operation",
                ))
            }
        }
    } else {
        match operator {
            BinaryOp::Add => builder.ins().iadd(left_value, right_value),
            BinaryOp::Subtract => builder.ins().isub(left_value, right_value),
            BinaryOp::Multiply => builder.ins().imul(left_value, right_value),
            BinaryOp::Divide if left_type.is_signed_integer() => {
                builder.ins().sdiv(left_value, right_value)
            }
            BinaryOp::Divide => builder.ins().udiv(left_value, right_value),
            BinaryOp::Remainder if left_type.is_signed_integer() => {
                builder.ins().srem(left_value, right_value)
            }
            BinaryOp::Remainder => builder.ins().urem(left_value, right_value),
            BinaryOp::ShiftLeft => builder.ins().ishl(left_value, right_value),
            BinaryOp::ShiftRight if left_type.is_signed_integer() => {
                builder.ins().sshr(left_value, right_value)
            }
            BinaryOp::ShiftRight => builder.ins().ushr(left_value, right_value),
            BinaryOp::BitAnd => builder.ins().band(left_value, right_value),
            BinaryOp::BitXor => builder.ins().bxor(left_value, right_value),
            BinaryOp::BitOr => builder.ins().bor(left_value, right_value),
            _ => return Err(Diagnostic::codegen("invalid integer binary operation")),
        }
    };
    Ok(CompiledValue::Numeric {
        value,
        ty: result_type,
    })
}

/// Truncating remainder. The sign matches the dividend, same as Cranelift's
/// missing `frem` and Rust's `%`.
fn float_remainder(builder: &mut FunctionBuilder<'_>, left: Value, right: Value) -> Value {
    let quotient = builder.ins().fdiv(left, right);
    let truncated = builder.ins().trunc(quotient);
    let product = builder.ins().fmul(truncated, right);
    builder.ins().fsub(left, product)
}

/// Return a safe result, an overflow flag, and a divide-by-zero flag.
/// Invalid divisors are replaced before division so checked modes never trap.
fn integer_arithmetic(
    builder: &mut FunctionBuilder<'_>,
    op: ArithmeticOp,
    left: Value,
    right: Value,
    ty: Type,
) -> (Value, Value, Value) {
    let clear = builder.ins().iconst(types::I8, 0);
    let signed = ty.is_signed_integer();
    let (value, overflow) = match (op, signed) {
        (ArithmeticOp::Add, true) => builder.ins().sadd_overflow(left, right),
        (ArithmeticOp::Add, false) => builder.ins().uadd_overflow(left, right),
        (ArithmeticOp::Subtract, true) => builder.ins().ssub_overflow(left, right),
        (ArithmeticOp::Subtract, false) => builder.ins().usub_overflow(left, right),
        (ArithmeticOp::Multiply, true) => builder.ins().smul_overflow(left, right),
        (ArithmeticOp::Multiply, false) => builder.ins().umul_overflow(left, right),
        (ArithmeticOp::Divide | ArithmeticOp::Remainder, _) => {
            let native = builder.func.dfg.value_type(left);
            let zero = builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
            let overflow = if signed {
                let minimum = builder
                    .ins()
                    .iconst(native, (1u64 << (native.bits() - 1)) as i64);
                let at_min = builder.ins().icmp(IntCC::Equal, left, minimum);
                let negative_one = builder.ins().icmp_imm_s(IntCC::Equal, right, -1);
                builder.ins().band(at_min, negative_one)
            } else {
                clear
            };
            let invalid = builder.ins().bor(zero, overflow);
            let one = builder.ins().iconst(native, 1);
            let divisor = builder.ins().select(invalid, one, right);
            let value = match (op, signed) {
                (ArithmeticOp::Divide, true) => builder.ins().sdiv(left, divisor),
                (ArithmeticOp::Divide, false) => builder.ins().udiv(left, divisor),
                (_, true) => builder.ins().srem(left, divisor),
                (_, false) => builder.ins().urem(left, divisor),
            };
            // MIN % -1 is the representable mathematical remainder zero.
            return (
                value,
                if op == ArithmeticOp::Remainder {
                    clear
                } else {
                    overflow
                },
                zero,
            );
        }
    };
    (value, overflow, clear)
}

fn saturation_bound(
    builder: &mut FunctionBuilder<'_>,
    op: ArithmeticOp,
    left: Value,
    right: Value,
    ty: Type,
) -> Value {
    let native = builder.func.dfg.value_type(left);
    if !ty.is_signed_integer() {
        return builder
            .ins()
            .iconst(native, if op == ArithmeticOp::Subtract { 0 } else { -1 });
    }
    let min_bits = 1u64 << (native.bits() - 1);
    let minimum = builder.ins().iconst(native, min_bits as i64);
    let maximum = builder.ins().iconst(native, (min_bits - 1) as i64);
    let sign = match op {
        ArithmeticOp::Multiply => builder.ins().bxor(left, right),
        ArithmeticOp::Divide | ArithmeticOp::Remainder => return maximum,
        _ => left,
    };
    let negative = builder.ins().icmp_imm_s(IntCC::SignedLessThan, sign, 0);
    builder.ins().select(negative, minimum, maximum)
}

fn int_condition(operator: BinaryOp, ty: Type) -> Result<IntCC, Diagnostic> {
    let signed = ty.is_signed_integer();
    match operator {
        BinaryOp::Equal => Ok(IntCC::Equal),
        BinaryOp::NotEqual => Ok(IntCC::NotEqual),
        BinaryOp::Less if signed => Ok(IntCC::SignedLessThan),
        BinaryOp::Less => Ok(IntCC::UnsignedLessThan),
        BinaryOp::LessEqual if signed => Ok(IntCC::SignedLessThanOrEqual),
        BinaryOp::LessEqual => Ok(IntCC::UnsignedLessThanOrEqual),
        BinaryOp::Greater if signed => Ok(IntCC::SignedGreaterThan),
        BinaryOp::Greater => Ok(IntCC::UnsignedGreaterThan),
        BinaryOp::GreaterEqual if signed => Ok(IntCC::SignedGreaterThanOrEqual),
        BinaryOp::GreaterEqual => Ok(IntCC::UnsignedGreaterThanOrEqual),
        _ => Err(Diagnostic::codegen("invalid integer comparison")),
    }
}

fn float_condition(operator: BinaryOp) -> Result<FloatCC, Diagnostic> {
    match operator {
        BinaryOp::Equal => Ok(FloatCC::Equal),
        BinaryOp::NotEqual => Ok(FloatCC::NotEqual),
        BinaryOp::Less => Ok(FloatCC::LessThan),
        BinaryOp::LessEqual => Ok(FloatCC::LessThanOrEqual),
        BinaryOp::Greater => Ok(FloatCC::GreaterThan),
        BinaryOp::GreaterEqual => Ok(FloatCC::GreaterThanOrEqual),
        _ => Err(Diagnostic::codegen("invalid float comparison")),
    }
}

pub(super) fn compile_numeric_method(
    builder: &mut FunctionBuilder<'_>,
    method: NumericMethod,
    arguments: &[CompiledValue],
    result_type: Type,
    pointer_type: cranelift_codegen::ir::Type,
    panic_ref: FuncRef,
) -> Result<CompiledValue, Diagnostic> {
    if let NumericMethod::ArithmeticOverflow(op) = method {
        let left = numeric_argument(arguments, 0)?;
        let right = numeric_argument(arguments, 1)?;
        if !left.ty.is_integer() || right.ty != left.ty || result_type != Type::Bool {
            return Err(Diagnostic::codegen(
                "arithmetic overflow check requires matching integers",
            ));
        }
        let (_, overflow, zero) = integer_arithmetic(builder, op, left.value, right.value, left.ty);
        return Ok(CompiledValue::Boolean {
            value: builder.ins().bor(overflow, zero),
        });
    }
    let value = match method {
        NumericMethod::Abs => {
            let operand = numeric_argument(arguments, 0)?;
            if operand.ty != result_type {
                return Err(Diagnostic::codegen("abs result type does not match"));
            }
            if operand.ty.is_float() {
                builder.ins().fabs(operand.value)
            } else if operand.ty.is_signed_integer() {
                let clif = cranelift_type(operand.ty).map_err(|_| {
                    Diagnostic::codegen("abs integer has no runtime representation")
                })?;
                let minimum = builder.ins().iconst(clif, signed_minimum(operand.ty));
                let is_minimum = builder.ins().icmp(IntCC::Equal, operand.value, minimum);
                panic_if(
                    builder,
                    is_minimum,
                    b"absolute value overflow",
                    pointer_type,
                    panic_ref,
                );
                let zero = builder.ins().iconst(clif, 0);
                let negative = builder
                    .ins()
                    .icmp(IntCC::SignedLessThan, operand.value, zero);
                let negated = builder.ins().ineg(operand.value);
                builder.ins().select(negative, negated, operand.value)
            } else if operand.ty.is_integer() {
                operand.value
            } else {
                return Err(Diagnostic::codegen("abs requires a numeric value"));
            }
        }
        NumericMethod::Min | NumericMethod::Max => {
            let left = numeric_argument(arguments, 0)?;
            let right = numeric_argument(arguments, 1)?;
            if left.ty != right.ty || left.ty != result_type {
                return Err(Diagnostic::codegen("min/max operands have different types"));
            }
            if left.ty.is_float() {
                float_min_max(builder, method, left.value, right.value)
            } else if left.ty.is_integer() {
                let condition = if left.ty.is_signed_integer() {
                    IntCC::SignedLessThan
                } else {
                    IntCC::UnsignedLessThan
                };
                let condition = if method == NumericMethod::Max {
                    match condition {
                        IntCC::SignedLessThan => IntCC::SignedGreaterThan,
                        _ => IntCC::UnsignedGreaterThan,
                    }
                } else {
                    condition
                };
                let pick_left = builder.ins().icmp(condition, left.value, right.value);
                builder.ins().select(pick_left, left.value, right.value)
            } else {
                return Err(Diagnostic::codegen("min/max requires numeric values"));
            }
        }
        NumericMethod::IntegerCast => convert_integer(builder, arguments, result_type)?,
        NumericMethod::ArithmeticOverflow(_) => unreachable!(),
    };
    Ok(CompiledValue::Numeric {
        value,
        ty: result_type,
    })
}

fn numeric_argument(arguments: &[CompiledValue], index: usize) -> Result<NumericValue, Diagnostic> {
    arguments
        .get(index)
        .cloned()
        .ok_or_else(|| Diagnostic::codegen("numeric method is missing an operand"))
        .and_then(expect_numeric)
}

fn signed_minimum(ty: Type) -> i64 {
    match ty {
        Type::I8 => i8::MIN as i64,
        Type::I16 => i16::MIN as i64,
        Type::I32 => i32::MIN as i64,
        Type::I64 => i64::MIN,
        _ => 0,
    }
}

fn float_min_max(
    builder: &mut FunctionBuilder<'_>,
    method: NumericMethod,
    left: Value,
    right: Value,
) -> Value {
    let left_nan = builder.ins().fcmp(FloatCC::NotEqual, left, left);
    let right_nan = builder.ins().fcmp(FloatCC::NotEqual, right, right);
    let either_nan = builder.ins().bor(left_nan, right_nan);
    let nan_result = builder.ins().select(left_nan, left, right);
    let ordered_cc = if method == NumericMethod::Max {
        FloatCC::GreaterThan
    } else {
        FloatCC::LessThan
    };
    let pick_left = builder.ins().fcmp(ordered_cc, left, right);
    let ordered = builder.ins().select(pick_left, left, right);
    builder.ins().select(either_nan, nan_result, ordered)
}

fn convert_integer(
    builder: &mut FunctionBuilder<'_>,
    arguments: &[CompiledValue],
    result_type: Type,
) -> Result<Value, Diagnostic> {
    let operand = numeric_argument(arguments, 0)?;
    if !operand.ty.is_integer() || !result_type.is_integer() {
        return Err(Diagnostic::codegen("integer cast requires integer types"));
    }
    let source = cranelift_type(operand.ty)
        .map_err(|_| Diagnostic::codegen("cast source has no runtime representation"))?;
    let destination = cranelift_type(result_type)
        .map_err(|_| Diagnostic::codegen("cast target has no runtime representation"))?;
    Ok(if source == destination {
        operand.value
    } else if destination.bits() < source.bits() {
        builder.ins().ireduce(destination, operand.value)
    } else if operand.ty.is_signed_integer() {
        builder.ins().sextend(destination, operand.value)
    } else {
        builder.ins().uextend(destination, operand.value)
    })
}

fn panic_if(
    builder: &mut FunctionBuilder<'_>,
    condition: Value,
    message: &[u8],
    pointer_type: cranelift_codegen::ir::Type,
    panic_ref: FuncRef,
) {
    let panic_block = builder.create_block();
    let continue_block = builder.create_block();
    builder
        .ins()
        .brif(condition, panic_block, &[], continue_block, &[]);
    builder.switch_to_block(panic_block);
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        message.len() as u32,
        0,
    ));
    let pointer = builder.ins().stack_addr(pointer_type, slot, 0);
    for (index, byte) in message.iter().enumerate() {
        let value = builder.ins().iconst(types::I8, *byte as i64);
        builder
            .ins()
            .store(MemFlagsData::new(), value, pointer, index as i32);
    }
    let length = builder.ins().iconst(pointer_type, message.len() as i64);
    builder.ins().call(panic_ref, &[pointer, length]);
    builder.ins().trap(TrapCode::unwrap_user(1));
    builder.seal_block(panic_block);
    builder.switch_to_block(continue_block);
}
