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
}

/// Stable machine-readable categories for structured diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Generic,
    InvalidImport,
    InvalidCall,
    UnknownName,
    UnknownType,
    TypeMismatch,
    DuplicateDefinition,
    ReservedName,
    InvalidEntryPoint,
}

impl ErrorCode {
    /// Stable lowercase identifier suitable for logs and embedding APIs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::InvalidImport => "invalid_import",
            Self::InvalidCall => "invalid_call",
            Self::UnknownName => "unknown_name",
            Self::UnknownType => "unknown_type",
            Self::TypeMismatch => "type_mismatch",
            Self::DuplicateDefinition => "duplicate_definition",
            Self::ReservedName => "reserved_name",
            Self::InvalidEntryPoint => "invalid_entry_point",
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
        };
        f.write_str(name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    phase: ErrorPhase,
    code: ErrorCode,
    message: String,
    line: Option<usize>,
    span: Option<Span>,
}

impl Error {
    pub fn new(phase: ErrorPhase, message: impl Into<String>) -> Self {
        Self {
            phase,
            code: ErrorCode::Generic,
            message: message.into(),
            line: None,
            span: None,
        }
    }

    pub fn new_with_code(phase: ErrorPhase, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            phase,
            code,
            message: message.into(),
            line: None,
            span: None,
        }
    }

    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = code;
        self
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

    pub fn span(&self) -> Option<Span> {
        self.span
    }

    /// Compatibility helper for callers that previously inspected String errors.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
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

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::new(ErrorPhase::Compile, message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::new(ErrorPhase::Compile, message)
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.to_string()
    }
}

impl PartialEq<String> for Error {
    fn eq(&self, other: &String) -> bool {
        self.line.is_none() && self.message == *other
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
        assert!(error.contains("unexpected token"));
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
