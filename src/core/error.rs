//! Structured errors shared by the public embedding boundary.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorPhase {
    Parse,
    Resolve,
    TypeCheck,
    Compile,
    Runtime,
    Abi,
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
    message: String,
    line: Option<usize>,
}

impl Error {
    pub fn new(phase: ErrorPhase, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
            line: None,
        }
    }

    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    pub fn phase(&self) -> ErrorPhase {
        self.phase
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn line(&self) -> Option<usize> {
        self.line
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
        assert_eq!(error.line(), Some(4));
        assert!(error.contains("unexpected token"));
        assert_eq!(error.to_string(), "Line 4: unexpected token");
    }
}
