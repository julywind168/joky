//! Joky parser.
//!
//! Recursive descent from tokens to a Module AST.
pub mod ast;
mod atoms;
mod attributes;
mod control_flow;
mod declarations;
mod expressions;
mod format;
pub use format::format;
mod interpolation;
mod lexer;
mod module;
mod patterns;
mod source_text;
mod sql_template;
pub use source_text::escape_diagnostic_text;
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
    pub code: crate::ErrorCode,
    pub line: usize,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    fn at(line: usize, span: impl Into<Span>, message: impl Into<String>) -> Self {
        Self {
            code: crate::ErrorCode::Generic,
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

pub(crate) const MAX_EXPRESSION_DEPTH: usize = 32;

// A parenthesized first item can be a group or a tuple. Keep the first
// line-leading subtraction until the delimiter tells us which one it is.
enum Newlines {
    Separate,
    Joined { minus: Option<usize> },
}

struct Parser<'src> {
    text: &'src str,
    source: crate::SourceId,
    tokens: Vec<Token>,
    pos: usize,
    last_span: Span,
    /// End line of the last consumed token, including multiline interpolation.
    last_line: usize,
    /// Parentheses ignore newlines; nested brace bodies restore separation.
    newlines: Newlines,
    allow_record: bool,
    type_params: Vec<std::sync::Arc<TypeParameter>>,
    expression_depth: usize,
    type_depth: usize,
    pattern_depth: usize,
    impl_target: Option<Type>,
    impl_trait: Option<String>,
    interpolation: bool,
    sql_templates: bool,
    durations: bool,
    arrays: bool,
    iterators: bool,
}

impl<'src> Parser<'src> {
    fn new(tokens: Vec<Token>, text: &'src str) -> Self {
        Parser {
            text,
            source: tokens
                .first()
                .map_or(crate::SourceId::ANONYMOUS, |token| token.span.source_id()),
            tokens,
            pos: 0,
            last_span: Span::empty(0),
            last_line: 0,
            newlines: Newlines::Separate,
            allow_record: true,
            type_params: Vec::new(),
            expression_depth: 0,
            type_depth: 0,
            pattern_depth: 0,
            impl_target: None,
            impl_trait: None,
            interpolation: false,
            sql_templates: false,
            durations: false,
            arrays: false,
            iterators: false,
        }
    }

    /// Create a parser from source text.
    fn from_source(source: &'src str) -> Result<Self, Diagnostic> {
        Ok(Parser::new(Lexer::tokenize(source)?, source))
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start, end).with_source(self.source)
    }

    // ── Parser helpers ──────────────────────────────────────────────────────────

    fn error(&self, message: String) -> Diagnostic {
        if self.peek() == &TokenKind::Eof {
            let mut closed = 0;
            for token in self.tokens[..self.pos].iter().rev() {
                match token.kind {
                    TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen => closed += 1,
                    TokenKind::LBrace if closed == 0 => {
                        return Diagnostic::at(
                            token.line,
                            token.span,
                            format!("{message} (unclosed `{{`)"),
                        )
                    }
                    TokenKind::LParen | TokenKind::LBracket if closed == 0 => break,
                    TokenKind::LBrace | TokenKind::LBracket | TokenKind::LParen => closed -= 1,
                    _ => {}
                }
            }
        }
        Diagnostic::at(self.peek_line(), self.peek_span(), message)
    }

    fn check_expression_depth(&self, depth: usize, span: Span) -> Result<(), Diagnostic> {
        if depth <= MAX_EXPRESSION_DEPTH {
            return Ok(());
        }
        Err(self.expression_depth_error(span))
    }

    fn expression_depth_error(&self, span: Span) -> Diagnostic {
        let index = self
            .tokens
            .partition_point(|t| t.span.start() < span.start());
        let mut error = Diagnostic::at(
            self.tokens[index.min(self.tokens.len() - 1)].line,
            span,
            format!("Expression nesting exceeds {MAX_EXPRESSION_DEPTH} levels"),
        );
        error.code = crate::ErrorCode::ResourceLimit;
        error
    }

    fn nested_expression(
        &mut self,
        parse: fn(&mut Self) -> Result<Expr, Diagnostic>,
    ) -> Result<Expr, Diagnostic> {
        self.check_expression_depth(self.expression_depth + 1, self.peek_span())?;
        self.expression_depth += 1;
        let result = parse(self);
        self.expression_depth -= 1;
        let expr = result?;
        // Postfix and generated syntax can build deep trees without parser recursion.
        // Bound them before cloning, resolving, or destroying a larger enclosing tree.
        if let Some(span) = expr.excessive_depth(MAX_EXPRESSION_DEPTH) {
            return Err(self.expression_depth_error(span));
        }
        Ok(expr)
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

    fn check_list_item(&self) -> Result<(), Diagnostic> {
        if let Newlines::Joined { minus: Some(pos) } = self.newlines {
            let token = &self.tokens[pos];
            return Err(Diagnostic::at(token.line, token.span,
                "Ambiguous line-leading '-'. Add a comma before a negative item, or move the subtraction operator to the preceding line."));
        }
        Ok(())
    }

    fn at_expression_break(&self) -> bool {
        matches!(self.newlines, Newlines::Separate) && self.peek_line() > self.last_line
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

    fn found_token(&self) -> String {
        if self.peek() == &TokenKind::Eof {
            return "end of input".into();
        }
        let text = self.peek_span().slice(self.text).expect("lexer span");
        format!("`{text}`")
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), Diagnostic> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("Expected {}, got {}", expected, self.found_token())))
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
            _ => Err(self.error(format!(
                "Expected an identifier, got {}",
                self.found_token()
            ))),
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
            _ => Err(self.error(format!("Expected ',' or ')', got {}", self.found_token()))),
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
    Parser::new(tokens, source).parse_module()
}

#[cfg(test)]
mod tests;
