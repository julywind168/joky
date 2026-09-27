//! Joky compilers, VM, and shared continuation runtime.
mod backends;
mod core;
mod modules;
pub mod runtime;
mod stdlib;
pub mod vm;

// Compatibility aliases keep the current internal module paths stable while
// the source tree follows the future crate boundaries.
pub use backends::aot;
pub(crate) use backends::{interp, jit};
pub(crate) use core::compiler;
pub use core::parser;
pub(crate) use core::resolver;
pub use core::{Error, ErrorCode, ErrorPhase, FunctionSignature, Span, Type, TypeRegistry};
