mod error;
mod render;

use std::fmt;

use crate::Span;

pub use error::{CodegenError, LexError, ParseError, SemanticError};
pub use render::write_diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Lex,
    Parse,
    Semantic,
    Codegen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    stage: Stage,
    message: String,
    span: Option<Span>,
}

impl Diagnostic {
    pub(crate) fn lex(message: impl Into<String>, span: Span) -> Self {
        Self::new(Stage::Lex, message, Some(span))
    }

    pub(crate) fn parse(message: impl Into<String>, span: Option<Span>) -> Self {
        Self::new(Stage::Parse, message, span)
    }

    pub(crate) fn semantic(message: impl Into<String>, span: Span) -> Self {
        Self::new(Stage::Semantic, message, Some(span))
    }

    pub(crate) fn codegen(message: impl Into<String>) -> Self {
        Self::new(Stage::Codegen, message, None)
    }

    fn new(stage: Stage, message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            stage,
            message: message.into(),
            span,
        }
    }

    pub const fn stage(&self) -> Stage {
        self.stage
    }

    pub const fn span(&self) -> Option<Span> {
        self.span
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

// Convert specialized error types to the unified Diagnostic

impl From<LexError> for Diagnostic {
    fn from(error: LexError) -> Self {
        Self::lex(error.message(), error.span())
    }
}

impl From<ParseError> for Diagnostic {
    fn from(error: ParseError) -> Self {
        Self::parse(error.message(), error.span())
    }
}

impl From<SemanticError> for Diagnostic {
    fn from(error: SemanticError) -> Self {
        match error.span() {
            Some(span) => Self::semantic(error.message(), span),
            None => Self::new(Stage::Semantic, error.message(), None),
        }
    }
}

impl From<CodegenError> for Diagnostic {
    fn from(error: CodegenError) -> Self {
        Self::codegen(error.message())
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}
