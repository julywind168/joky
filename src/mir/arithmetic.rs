//! Integer arithmetic evaluated with the same modes as generated code.

use crate::sema::{integer_shape, Type};
use crate::syntax::{ArithmeticMode, ArithmeticOp};

pub(super) fn evaluate(
    op: ArithmeticOp,
    mode: ArithmeticMode,
    left: u64,
    right: u64,
    ty: Type,
) -> Option<u64> {
    let (width, signed) = integer_shape(ty)?;
    let mask = u64::MAX >> (64 - width);
    let (left, right) = (left & mask, right & mask);
    let decode = |bits: u64| {
        if signed {
            (((bits << (64 - width)) as i64) >> (64 - width)) as i128
        } else {
            bits as i128
        }
    };
    let (a, b) = (decode(left), decode(right));
    let (minimum, maximum) = if signed {
        (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
    } else {
        (0, mask as i128)
    };
    if b == 0 && matches!(op, ArithmeticOp::Divide | ArithmeticOp::Remainder) {
        return None;
    }
    let exact = match op {
        ArithmeticOp::Add => a.checked_add(b),
        ArithmeticOp::Subtract => a.checked_sub(b),
        ArithmeticOp::Multiply => a.checked_mul(b),
        ArithmeticOp::Divide => Some(a / b),
        ArithmeticOp::Remainder => Some(a % b),
    };
    let value = match mode {
        ArithmeticMode::Panic | ArithmeticMode::Checked => {
            let value = exact?;
            if !(minimum..=maximum).contains(&value) {
                return None;
            }
            value as u64
        }
        ArithmeticMode::Saturating => {
            // Only a UInt64 product can exceed i128; it exceeds the target maximum.
            exact.unwrap_or(i128::MAX).clamp(minimum, maximum) as u64
        }
        ArithmeticMode::Wrapping => match op {
            ArithmeticOp::Add => left.wrapping_add(right),
            ArithmeticOp::Subtract => left.wrapping_sub(right),
            ArithmeticOp::Multiply => left.wrapping_mul(right),
            ArithmeticOp::Divide | ArithmeticOp::Remainder => exact? as u64,
        },
    };
    Some(value & mask)
}
