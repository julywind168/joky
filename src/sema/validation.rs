//! Literal validation and helper functions

use crate::diagnostic::SemanticError;
use crate::syntax::{BinaryOp, Expr, ExprKind, IntegerSuffix, UnaryOp};
use crate::Span;

use super::types::{type_name, Type};

pub(super) fn integer_suffix_type(suffix: IntegerSuffix) -> Type {
    match suffix {
        IntegerSuffix::I8 => Type::I8,
        IntegerSuffix::I16 => Type::I16,
        IntegerSuffix::I32 => Type::I32,
        IntegerSuffix::I64 => Type::I64,
        IntegerSuffix::U8 => Type::U8,
        IntegerSuffix::U16 => Type::U16,
        IntegerSuffix::U32 => Type::U32,
        IntegerSuffix::U64 => Type::U64,
    }
}

/// Checks whether a positive integer literal is within the type's range
pub(super) fn check_positive_integer(
    value: u64,
    value_type: Type,
    span: Span,
) -> Result<(), SemanticError> {
    let maximum = match value_type {
        Type::I8 => i8::MAX as u64,
        Type::I16 => i16::MAX as u64,
        Type::I32 => i32::MAX as u64,
        Type::I64 => i64::MAX as u64,
        Type::U8 => u8::MAX as u64,
        Type::U16 => u16::MAX as u64,
        Type::U32 => u32::MAX as u64,
        Type::U64 => u64::MAX,
        _ => return Err(type_mismatch(value_type, Type::I32, span)),
    };
    if value > maximum {
        return Err(SemanticError::IntegerLiteralOutOfRange {
            type_name: type_name(value_type).to_owned(),
            span,
        });
    }
    Ok(())
}

/// Checks whether a negative integer literal is within the type's range
pub(super) fn check_negative_integer(
    value: u64,
    value_type: Type,
    span: Span,
) -> Result<(), SemanticError> {
    let maximum_magnitude = match value_type {
        Type::I8 => 1_u64 << 7,
        Type::I16 => 1_u64 << 15,
        Type::I32 => 1_u64 << 31,
        Type::I64 => 1_u64 << 63,
        _ => {
            return Err(SemanticError::CannotNegateType {
                type_name: type_name(value_type).to_owned(),
                span,
            });
        }
    };
    if value > maximum_magnitude {
        return Err(SemanticError::IntegerLiteralOutOfRange {
            type_name: type_name(value_type).to_owned(),
            span,
        });
    }
    Ok(())
}

/// Checks whether a float literal is within the type's range
pub(super) fn check_float(value: f64, value_type: Type, span: Span) -> Result<(), SemanticError> {
    if value_type == Type::F32 && !(value as f32).is_finite() {
        return Err(SemanticError::FloatLiteralOutOfRange { span });
    }
    Ok(())
}

/// Returns whether the expression is a numeric literal whose type can be inferred.
/// Explicit suffixes fix the type and must never be contextualized.
pub(super) fn is_numeric_literal(expression: &Expr) -> bool {
    matches!(expression.kind, ExprKind::Integer(_) | ExprKind::Float(_))
        || matches!(
            &expression.kind,
            ExprKind::Unary {
                op: UnaryOp::Negate,
                expression
            } if matches!(expression.kind, ExprKind::Integer(_) | ExprKind::Float(_))
        )
        || matches!(
            &expression.kind,
            ExprKind::Unary {
                op: UnaryOp::BitNot,
                expression
            } if matches!(expression.kind, ExprKind::Integer(_))
        )
}

/// Tries to evaluate a constant integer expression
pub(super) fn constant_integer(expression: &Expr) -> Option<i128> {
    match &expression.kind {
        ExprKind::Integer(value) | ExprKind::TypedInteger(value, _) => Some(*value as i128),
        ExprKind::Unary {
            op: UnaryOp::Negate,
            expression,
        } => constant_integer(expression)?.checked_neg(),
        ExprKind::Binary { op, left, right } => {
            let left = constant_integer(left)?;
            let right = constant_integer(right)?;
            match op {
                BinaryOp::Add => left.checked_add(right),
                BinaryOp::Subtract => left.checked_sub(right),
                BinaryOp::Multiply => left.checked_mul(right),
                BinaryOp::Divide if right != 0 => left.checked_div(right),
                BinaryOp::Divide => None,
                _ => None,
            }
        }
        _ => None,
    }
}

/// Builds a type mismatch error
pub(super) fn type_mismatch(expected: Type, actual: Type, span: Span) -> SemanticError {
    SemanticError::TypeMismatch {
        expected: type_name(expected).to_owned(),
        actual: type_name(actual).to_owned(),
        span,
    }
}
