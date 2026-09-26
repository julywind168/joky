//! Core language representation and shared compilation pipeline.
//!
//! This module is the future `joky-core` crate boundary. It currently remains
//! inside the single `joky` crate so the public API and build remain stable.
pub(crate) mod compiler;
pub mod parser;
pub(crate) mod signature;

pub use parser::ast::Type;
pub use signature::FunctionSignature;
