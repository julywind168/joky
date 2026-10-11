//! Core language representation and shared compilation pipeline.
pub mod error;
pub mod ffi;
pub(crate) mod float;
pub(crate) mod imports;
pub(crate) mod instance;
pub(crate) mod integer;
pub(crate) mod ir;
mod json_derive;
pub mod parser;
pub(crate) mod prepared;
pub(crate) mod resolver;
pub(crate) mod signature;
pub mod span;
pub(crate) mod state_domain;
pub mod types;

pub use error::{Error, ErrorCode, ErrorPhase, RuntimeFrame};
pub use parser::ast::Type;
pub use signature::{EntryAbi, FunctionSignature};
pub use span::Span;
mod source;
pub use source::{ModuleSources, SourceFile, SourceId, SourceMap};
pub use types::TypeRegistry;
