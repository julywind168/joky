//! Execution backends built on the shared core IR.
//!
//! These modules are grouped here as the future backend crate boundary while
//! remaining part of the single `joky` crate for now.
pub mod aot;
pub(crate) mod interp;
pub(crate) mod jit;
