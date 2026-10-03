//! Joky compilers, VM, and shared continuation runtime.
mod backends;
mod core;
mod modules;
pub mod runtime;
mod stdlib;
pub mod vm;

#[cfg(test)]
#[path = "../tests/support/string_collections.rs"]
mod string_collection_model;

#[cfg(test)]
#[path = "../tests/support/string_queries.rs"]
mod string_query_model;

// Stable public entry points; implementation modules remain private.
pub use backends::aot;
pub use core::ffi;
pub use core::parser;
pub use core::types::memory_layout;
pub use core::{
    EntryAbi, Error, ErrorCode, ErrorPhase, FunctionSignature, ModuleSources, RuntimeFrame,
    SourceFile, SourceId, SourceMap, Span, Type, TypeRegistry,
};

#[cfg(test)]
#[path = "../tests/support/text_host.rs"]
mod text_host_model;
