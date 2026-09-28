//! Joky compilers, VM, and shared continuation runtime.
mod backends;
mod core;
mod modules;
pub mod runtime;
mod stdlib;
pub mod vm;

// Stable public entry points; implementation modules remain private.
pub use backends::aot;
pub use core::parser;
pub use core::{
    Error, ErrorCode, ErrorPhase, FunctionSignature, ModuleSources, RuntimeFrame, SourceFile,
    SourceId, SourceMap, Span, Type, TypeRegistry,
};
