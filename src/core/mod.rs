//! Core language representation and shared compilation pipeline.
pub mod error;
pub mod ffi;
pub(crate) mod imports;
pub(crate) mod integer;
pub(crate) mod ir;
pub mod parser;
pub(crate) mod prepared;
pub(crate) mod resolver;
pub(crate) mod signature;
pub mod span;
pub mod types;

pub use error::{Error, ErrorCode, ErrorPhase, RuntimeFrame};
pub use parser::ast::Type;
pub use signature::FunctionSignature;
pub use span::Span;
mod source;
pub use source::{ModuleSources, SourceFile, SourceId, SourceMap};
pub use types::TypeRegistry;
