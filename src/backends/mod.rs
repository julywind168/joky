//! Execution backends built on the shared core IR.
pub mod aot;
pub(crate) mod interp;
pub(crate) mod jit;
mod native;
