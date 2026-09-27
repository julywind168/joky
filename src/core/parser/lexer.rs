//! Joky lexer.
use super::Diagnostic;
use crate::Span;

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    // Keywords.
    Fn,
    Extern,
    Import,
    Let,
    Var,
    While,
    If,
    Else,
    Return,
    Break,
    Continue,
    Scope,
    True,
    False,

    // Type keywords.
    TyInt32,
    TyString,
    TyBool,
    TyUnit,

    // Identifiers and literals.
    Ident(String),
    // Unsigned magnitude permits the special spelling -2147483648.
    IntLit(u32),
    StrLit(String), // Decoded UTF-8 text; braces have no special meaning.

    // Punctuation.
    LParen, // (
    RParen, // )
    LBrace, // {
    RBrace, // }
    Comma,  // ,
    Dot,    // .
    Colon,  // :
    Semi,   // ;
    Arrow,  // ->
    Plus,   // +
    Minus,  // -
    Star,   // *
    Slash,  // /
    Pipe,   // |
    AndAnd, // &&
    OrOr,   // ||
    At,     // @
    Eq,     // =
    EqEq,   // ==
    Ne,     // !=
    Bang,   // !
    Lt,     // <
    Le,     // <=
    Gt,     // >
    Ge,     // >=

    // End of input.
    Eof,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    /// One-based source line number.
    pub(crate) line: usize,
    pub(crate) span: Span,
}

pub(crate) struct Lexer<'a> {
    source: &'a str,
    byte_pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Lexer {
            source,
            byte_pos: 0,
            line: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.byte_pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.source[self.byte_pos..].chars().nth(1)
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.byte_pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace.
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.advance();
            }
            // Skip line comments.
            if self.peek() == Some('/') && self.peek2() == Some('/') {
                while !matches!(self.peek(), Some('\n') | None) {
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn read_ident_or_keyword(&mut self) -> TokenKind {
        let start = self.byte_pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.advance();
            } else {
                break;
            }
        }
        match &self.source[start..self.byte_pos] {
            "fn" => TokenKind::Fn,
            "extern" => TokenKind::Extern,
            "import" => TokenKind::Import,
            "let" => TokenKind::Let,
            "var" => TokenKind::Var,
            "while" => TokenKind::While,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "return" => TokenKind::Return,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "scope" => TokenKind::Scope,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "Int32" => TokenKind::TyInt32,
            "String" => TokenKind::TyString,
            "Bool" => TokenKind::TyBool,
            "Unit" => TokenKind::TyUnit,
            name => TokenKind::Ident(name.into()),
        }
    }

    fn read_number(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
        }
        let n = self.source[start..self.byte_pos]
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= i32::MAX as u32 + 1)
            .ok_or_else(|| {
                Diagnostic::at(
                    self.line,
                    start..self.byte_pos,
                    "Integer literal is outside Int32 range",
                )
            })?;
        Ok(TokenKind::IntLit(n))
    }

    /// Read a quoted string and decode supported escapes.
    fn read_string(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        let start_line = self.line;
        // Skip the opening quote.
        self.advance();
        let mut s = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(Diagnostic::at(
                        start_line,
                        start..self.byte_pos,
                        "Unterminated string literal",
                    ))
                }
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.advance() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('"') => s.push('"'),
                        Some('\\') => s.push('\\'),
                        Some(c) => {
                            s.push('\\');
                            s.push(c);
                        }
                        None => {
                            return Err(Diagnostic::at(
                                start_line,
                                start..self.byte_pos,
                                "Unterminated string escape",
                            ))
                        }
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }
        Ok(TokenKind::StrLit(s))
    }

    /// Read the next token.
    fn next_token(&mut self) -> Result<Token, Diagnostic> {
        self.skip_whitespace_and_comments();
        let start = self.byte_pos;
        let line = self.line;
        let kind = match self.peek() {
            None => TokenKind::Eof,
            Some(c) if c.is_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            Some(c) if c.is_ascii_digit() => self.read_number(start)?,
            Some('"') => self.read_string(start)?,
            Some('(') => {
                self.advance();
                TokenKind::LParen
            }
            Some(')') => {
                self.advance();
                TokenKind::RParen
            }
            Some('{') => {
                self.advance();
                TokenKind::LBrace
            }
            Some('}') => {
                self.advance();
                TokenKind::RBrace
            }
            Some('.') => {
                self.advance();
                TokenKind::Dot
            }
            Some(',') => {
                self.advance();
                TokenKind::Comma
            }
            Some(':') => {
                self.advance();
                TokenKind::Colon
            }
            Some(';') => {
                self.advance();
                TokenKind::Semi
            }
            Some('-') if self.peek2() == Some('>') => {
                self.advance();
                self.advance();
                TokenKind::Arrow
            }
            Some('-') => {
                self.advance();
                TokenKind::Minus
            }
            Some('+') => {
                self.advance();
                TokenKind::Plus
            }
            Some('*') => {
                self.advance();
                TokenKind::Star
            }
            Some('/') => {
                self.advance();
                TokenKind::Slash
            }
            Some('&') if self.peek2() == Some('&') => {
                self.advance();
                self.advance();
                TokenKind::AndAnd
            }
            Some('|') if self.peek2() == Some('|') => {
                self.advance();
                self.advance();
                TokenKind::OrOr
            }
            Some('|') => {
                self.advance();
                TokenKind::Pipe
            }
            Some('@') => {
                self.advance();
                TokenKind::At
            }
            Some(c @ ('=' | '!' | '<' | '>')) if self.peek2() == Some('=') => {
                self.advance();
                self.advance();
                match c {
                    '=' => TokenKind::EqEq,
                    '!' => TokenKind::Ne,
                    '<' => TokenKind::Le,
                    '>' => TokenKind::Ge,
                    _ => unreachable!(),
                }
            }
            Some('<') => {
                self.advance();
                TokenKind::Lt
            }
            Some('!') => {
                self.advance();
                TokenKind::Bang
            }
            Some('>') => {
                self.advance();
                TokenKind::Gt
            }
            Some('=') => {
                self.advance();
                TokenKind::Eq
            }
            Some(c) => {
                return Err(Diagnostic::at(
                    line,
                    start..start + c.len_utf8(),
                    format!("Unexpected character {c:?}"),
                ));
            }
        };
        Ok(Token {
            kind,
            line,
            span: Span::new(start, self.byte_pos),
        })
    }

    /// Tokenize the entire source.
    pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, Diagnostic> {
        let mut lexer = Lexer::new(source);
        let mut tokens = Vec::new();
        loop {
            let tok = lexer.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_source_preserves_unicode_offsets_and_lookahead() {
        let source = "// 🙂\r\n\u{2003}let café = \"hé🙂\"; café != 12";
        let tokens = Lexer::tokenize(source).unwrap();
        let spellings: Vec<_> = tokens
            .iter()
            .map(|token| token.span.slice(source).unwrap())
            .collect();
        assert_eq!(
            spellings,
            ["let", "café", "=", "\"hé🙂\"", ";", "café", "!=", "12", ""]
        );
        assert!(tokens.iter().all(|token| token.line == 2));
        assert!(matches!(&tokens[1].kind, TokenKind::Ident(name) if name == "café"));
        assert!(matches!(&tokens[3].kind, TokenKind::StrLit(text) if text == "hé🙂"));
        assert_eq!(tokens[6].kind, TokenKind::Ne);
        assert_eq!(tokens[7].kind, TokenKind::IntLit(12));
        assert_eq!(tokens.last().unwrap().span, Span::empty(source.len()));
    }

    #[test]
    fn borrowed_source_reports_errors_and_eof_after_multibyte_text() {
        for (source, marked) in [
            ("// é\n2147483649", "2147483649"),
            ("// é\n\"🙂", "\"🙂"),
            ("// é\n🙂", "🙂"),
        ] {
            let error = Lexer::tokenize(source).unwrap_err();
            assert_eq!(error.line, 2);
            assert_eq!(error.span.slice(source), Some(marked));
        }
        for source in ["", "/", "//🙂", "!=", "<", "é"] {
            let tokens = Lexer::tokenize(source).unwrap();
            assert_eq!(tokens.last().unwrap().span, Span::empty(source.len()));
            assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
        }
    }

    #[test]
    fn tokenizes_function_definition() {
        let src = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
        let tokens = Lexer::tokenize(src).unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.kind).collect();
        assert!(matches!(kinds[0], TokenKind::Fn));
        assert!(matches!(kinds[1], TokenKind::Ident(s) if s == "add"));
    }

    #[test]
    fn tokenizes_string_literal() {
        let src = r#""Hello, {name}!""#;
        let tokens = Lexer::tokenize(src).unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::StrLit(s) if s == "Hello, {name}!"));
    }

    #[test]
    fn tokenizes_extern_declaration() {
        let src = "extern fn host_multiply(x: Int32, y: Int32) -> Int32";
        let tokens = Lexer::tokenize(src).unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Extern));
        assert!(matches!(tokens[1].kind, TokenKind::Fn));
    }
}
