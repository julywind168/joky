//! Joky parser.
//!
//! Recursive descent from tokens to a Module AST.
pub mod ast;
mod atoms;
mod attributes;
mod control_flow;
mod declarations;
mod expressions;
mod interpolation;
mod lexer;
mod module;
mod patterns;
#[cfg(test)]
pub(crate) mod test_support;
mod types;
mod when;

use self::ast::*;
use self::lexer::{Lexer, Token, TokenKind};
use crate::core::types::{CallableBound, CallableKind};
use crate::Span;

/// A lexical or syntactic error at a one-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    fn at(line: usize, span: impl Into<Span>, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            span: span.into(),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for Diagnostic {}

struct Parser {
    source: crate::SourceId,
    tokens: Vec<Token>,
    pos: usize,
    last_span: Span,
    /// End line of the last consumed token, including multiline interpolation.
    last_line: usize,
    /// Parentheses ignore newlines; nested brace bodies restore separation.
    newline_sensitive: bool,
    allow_record: bool,
    // An ungrouped pipe ends a join or race branch.
    allow_pipe: bool,
    type_params: Vec<std::sync::Arc<TypeParameter>>,
    type_depth: usize,
    pattern_depth: usize,
    impl_target: Option<Type>,
    impl_trait: Option<String>,
    interpolation: bool,
    arrays: bool,
    iterators: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser {
            source: tokens
                .first()
                .map_or(crate::SourceId::ANONYMOUS, |token| token.span.source_id()),
            tokens,
            pos: 0,
            last_span: Span::empty(0),
            last_line: 0,
            newline_sensitive: true,
            allow_record: true,
            allow_pipe: true,
            type_params: Vec::new(),
            type_depth: 0,
            pattern_depth: 0,
            impl_target: None,
            impl_trait: None,
            interpolation: false,
            arrays: false,
            iterators: false,
        }
    }

    /// Create a parser from source text.
    fn from_source(source: &str) -> Result<Self, Diagnostic> {
        Ok(Parser::new(Lexer::tokenize(source)?))
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start, end).with_source(self.source)
    }

    // ── Parser helpers ──────────────────────────────────────────────────────────

    fn error(&self, message: String) -> Diagnostic {
        Diagnostic::at(self.peek_line(), self.peek_span(), message)
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_line(&self) -> usize {
        self.tokens[self.pos].line
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn at_expression_break(&self) -> bool {
        self.newline_sensitive && self.peek_line() > self.last_line
    }

    fn advance(&mut self) -> &TokenKind {
        let tok = &self.tokens[self.pos];
        self.last_span = tok.span;
        self.last_line = tok.end_line;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        &tok.kind
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), Diagnostic> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("Expected {:?}, got {:?}", expected, self.peek())))
        }
    }

    fn expect_ident(&mut self) -> Result<String, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Ident(s) => {
                if s == "Self" && self.impl_target.is_some() {
                    return Err(self.error("Self is reserved inside impl".into()));
                }
                self.advance();
                Ok(s)
            }
            other => Err(self.error(format!("Expected an identifier, got {:?}", other))),
        }
    }

    fn check_binding_name(&self, name: &str) -> Result<(), Diagnostic> {
        if self
            .type_params
            .iter()
            .any(|p| p.name == name && p.kind == ParameterKind::Const)
        {
            return Err(self.error(format!("Binding `{name}` shadows a const parameter")));
        }
        Ok(())
    }

    /// Continue an already-consumed identifier through `.nested` path segments.
    fn parse_dotted_name(&mut self, mut name: String) -> Result<String, Diagnostic> {
        while self.peek() == &TokenKind::Dot {
            self.advance();
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
    }

    /// Require commas between list elements while allowing a trailing comma.
    fn expect_list_separator(&mut self) -> Result<(), Diagnostic> {
        match self.peek() {
            TokenKind::Comma => {
                self.advance();
                Ok(())
            }
            TokenKind::RParen => Ok(()),
            other => Err(self.error(format!("Expected ',' or ')', got {other:?}"))),
        }
    }

    /// Brace-delimited lists (record fields, enum variants, field
    /// initializers) separate on a comma or a real newline. The newline is
    /// measured between the previous item's last token and the next token,
    /// so multi-line types and initializers never split an item.
    pub(super) fn expect_braced_separator(&mut self, what: &str) -> Result<(), Diagnostic> {
        match self.peek() {
            TokenKind::Comma => {
                self.advance();
                if self.peek() == &TokenKind::Comma {
                    return Err(self.error(format!("{what} cannot be empty")));
                }
                Ok(())
            }
            TokenKind::RBrace => Ok(()),
            _ if self.peek_line() > self.last_line => Ok(()),
            _ => Err(self.error(format!("Expected ',' or a newline between {what}"))),
        }
    }
}

// ── Convenience API ──────────────────────────────────────────────────────────────

/// Parse a source module or return a diagnostic.
pub fn parse(source: &str) -> Result<Module, Diagnostic> {
    Parser::from_source(source)?.parse_module()
}

/// Parse one named source while preserving file identity in every diagnostic span.
pub fn parse_with_source(source: &str, id: crate::SourceId) -> Result<Module, Diagnostic> {
    let mut tokens = Lexer::tokenize(source).map_err(|mut error| {
        error.span = error.span.with_source(id);
        error
    })?;
    for token in &mut tokens {
        token.span = token.span.with_source(id);
    }
    Parser::new(tokens).parse_module()
}

#[cfg(test)]
mod tests;
