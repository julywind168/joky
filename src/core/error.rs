//! Structured errors shared by the public embedding boundary.
use crate::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorPhase {
    Parse,
    Resolve,
    TypeCheck,
    Compile,
    Runtime,
    Abi,
    Cli,
}

/// Stable machine-readable categories for structured diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Generic,
    InvalidImport,
    InvalidCall,
    InvalidAssignment,
    InvalidControlFlow,
    UnreachableCode,
    UnknownName,
    UnknownType,
    CyclicTypeAlias,
    RecursiveRecord,
    RecursiveType,
    InvalidPattern,
    NonExhaustiveMatch,
    TypeMismatch,
    DuplicateDefinition,
    ReservedName,
    InvalidEntryPoint,
    InvalidArgument,
    NotFound,
    InvalidState,
    AbiError,
    HostError,
    InteriorNul,
    InvalidUtf8,
    RuntimeError,
    ResourceLimit,
    IoError,
    LinkError,
    Panic,
    Cancelled,
}

impl ErrorCode {
    /// Stable lowercase identifier suitable for logs and embedding APIs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::InvalidImport => "invalid_import",
            Self::InvalidCall => "invalid_call",
            Self::InvalidAssignment => "invalid_assignment",
            Self::InvalidControlFlow => "invalid_control_flow",
            Self::UnreachableCode => "unreachable_code",
            Self::UnknownName => "unknown_name",
            Self::UnknownType => "unknown_type",
            Self::CyclicTypeAlias => "cyclic_type_alias",
            Self::RecursiveRecord => "recursive_record",
            Self::RecursiveType => "recursive_type",
            Self::InvalidPattern => "invalid_pattern",
            Self::NonExhaustiveMatch => "non_exhaustive_match",
            Self::TypeMismatch => "type_mismatch",
            Self::DuplicateDefinition => "duplicate_definition",
            Self::ReservedName => "reserved_name",
            Self::InvalidEntryPoint => "invalid_entry_point",
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::InvalidState => "invalid_state",
            Self::AbiError => "abi_error",
            Self::HostError => "host_error",
            Self::InteriorNul => "interior_nul",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::RuntimeError => "runtime_error",
            Self::ResourceLimit => "resource_limit",
            Self::IoError => "io_error",
            Self::LinkError => "link_error",
            Self::Panic => "panic",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for ErrorPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Parse => "parse",
            Self::Resolve => "resolve",
            Self::TypeCheck => "typecheck",
            Self::Compile => "compile",
            Self::Runtime => "runtime",
            Self::Abi => "abi",
            Self::Cli => "cli",
        };
        f.write_str(name)
    }
}

/// A source-level activation captured when the interpreter fails, innermost first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFrame {
    /// Source-level function name in the failing module version.
    pub function: String,
    /// Program counter in the final optimized bytecode, not the shared IR.
    pub pc: usize,
    /// Expression or call-site byte range in that module's original source.
    pub span: Span,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ErrorTrace {
    interpreter: Vec<RuntimeFrame>,
    native_call_sites: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    sources: std::sync::Arc<crate::SourceMap>,
    phase: ErrorPhase,
    code: ErrorCode,
    message: String,
    line: Option<usize>,
    span: Option<Span>,
    trace: Option<Box<ErrorTrace>>,
}

impl Error {
    pub fn new(phase: ErrorPhase, message: impl Into<String>) -> Self {
        Self {
            sources: Default::default(),
            phase,
            code: ErrorCode::Generic,
            message: message.into(),
            line: None,
            span: None,
            trace: None,
        }
    }

    pub fn new_with_code(phase: ErrorPhase, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            sources: Default::default(),
            phase,
            code,
            message: message.into(),
            line: None,
            span: None,
            trace: None,
        }
    }

    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    pub fn phase(&self) -> ErrorPhase {
        self.phase
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn line(&self) -> Option<usize> {
        self.line
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn sources(&self) -> &crate::SourceMap {
        &self.sources
    }

    pub fn with_sources(mut self, sources: &crate::SourceMap) -> Self {
        std::sync::Arc::make_mut(&mut self.sources).extend(sources);
        self
    }

    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// Interpreter activations, innermost first; empty for errors without a trace.
    /// Spans refer to the failing module version's source, including after reload.
    pub fn runtime_trace(&self) -> &[RuntimeFrame] {
        self.trace.as_ref().map_or(&[], |trace| &trace.interpreter)
    }

    /// Source call sites recorded as failures propagate through native synchronous
    /// calls, innermost first. These carry no bytecode PCs or native stack frames.
    pub fn native_call_sites(&self) -> &[Span] {
        self.trace
            .as_ref()
            .map_or(&[], |trace| &trace.native_call_sites)
    }

    pub(crate) fn push_native_call_site(&mut self, span: Span) {
        self.trace
            .get_or_insert_with(Default::default)
            .native_call_sites
            .push(span);
    }

    pub(crate) fn push_runtime_frame(&mut self, frame: RuntimeFrame) {
        if self.span.is_none() {
            self.span = Some(frame.span);
        }
        self.trace
            .get_or_insert_with(Default::default)
            .interpreter
            .push(frame);
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(line) = self.line {
            write!(f, "Line {line}: {}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for Error {}

impl From<super::parser::Diagnostic> for Error {
    fn from(diagnostic: super::parser::Diagnostic) -> Self {
        Self::new(ErrorPhase::Parse, diagnostic.message)
            .with_line(diagnostic.line)
            .with_span(diagnostic.span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_machine_readable_phase_and_line() {
        let error = Error::new(ErrorPhase::Parse, "unexpected token").with_line(4);
        assert_eq!(error.phase(), ErrorPhase::Parse);
        assert_eq!(error.code(), ErrorCode::Generic);
        assert_eq!(error.line(), Some(4));
        assert_eq!(error.span(), None);
        assert!(error.message().contains("unexpected token"));
        assert_eq!(error.to_string(), "Line 4: unexpected token");
    }

    #[test]
    fn preserves_machine_readable_error_code() {
        let error =
            Error::new_with_code(ErrorPhase::TypeCheck, ErrorCode::TypeMismatch, "wrong type");
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert_eq!(error.code().as_str(), "type_mismatch");
        assert_eq!(error.code().to_string(), "type_mismatch");
    }
}
