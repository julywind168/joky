//! Structural and type verification of the typed MIR.

mod function;

pub(crate) use function::statement_operands;
pub(super) use function::verify;
#[cfg(test)]
pub(crate) use function::{verify_function, MirSignature};
