//! Core language representation and shared compilation pipeline.
//!
//! This module is the future `joky-core` crate boundary. It currently remains
//! inside the single `joky` crate so the public API and build remain stable.
pub(crate) mod compiler;
pub mod error;
pub mod parser;
pub(crate) mod resolver;
pub(crate) mod signature;
pub mod span;
pub mod types;

pub use error::{Error, ErrorCode, ErrorPhase, RuntimeFrame};
pub use parser::ast::Type;
pub use signature::FunctionSignature;
pub use span::Span;
pub use types::TypeRegistry;
