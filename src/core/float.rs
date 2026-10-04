//! IEEE binary arithmetic, without algebraic rewrites or fast math.
use crate::parser::ast::BinOp;
pub(crate) enum FloatOutcome<T> {
    Value(T),
    Bool(bool),
}
macro_rules! kernel {
    ($name:ident, $ty:ty) => {
        pub(crate) fn $name(op: &BinOp, a: $ty, b: $ty) -> FloatOutcome<$ty> {
            match op {
                BinOp::Add => FloatOutcome::Value(a + b),
                BinOp::Sub => FloatOutcome::Value(a - b),
                BinOp::Mul => FloatOutcome::Value(a * b),
                BinOp::Div => FloatOutcome::Value(a / b),
                BinOp::Eq => FloatOutcome::Bool(a == b),
                BinOp::Ne => FloatOutcome::Bool(a != b),
                BinOp::Lt => FloatOutcome::Bool(a < b),
                BinOp::Le => FloatOutcome::Bool(a <= b),
                BinOp::Gt => FloatOutcome::Bool(a > b),
                BinOp::Ge => FloatOutcome::Bool(a >= b),
                _ => unreachable!("invalid floating operation after type checking"),
            }
        }
    };
}
kernel!(apply_f32, f32);
kernel!(apply_f64, f64);
