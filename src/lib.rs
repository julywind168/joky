mod codegen;
mod compiler;
mod diagnostic;
mod frontend;
mod hir;
mod linker;
mod linker_types;
mod mir;
pub mod module;
mod sema;
mod source;
pub mod syntax;
mod target;
#[cfg(test)]
mod test_metrics;

pub use compiler::{AotBuildOptions, AotProviderMetadata, AotSourceRecord, Compiler};
pub use diagnostic::{write_diagnostic, Diagnostic, Stage};
pub use frontend::{CheckReport, CheckResult, Frontend, FrontendDiagnostic, SourceFile};
pub use source::Span;
pub use target::AotTarget;
