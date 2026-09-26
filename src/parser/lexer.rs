//! Joky-Lite lexer.
use super::Diagnostic;
use std::fmt;

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords.
    Fn,
    Extern,
    Let,
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
    IntLit(i32),
    StrLit(String), // Raw string with interpolation placeholders preserved.

    // Punctuation.
    LParen, // (
    RParen, // )
    LBrace, // {
    RBrace, // }
    Comma,  // ,
    Colon,  // :
    Arrow,  // ->
    Plus,   // +
    Minus,  // -
    Star,   // *
    Slash,  // /
    Pipe,   // |
    At,     // @
    Eq,     // =

    // End of input.
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Ident(s) => write!(f, "ident `{}`", s),
            TokenKind::IntLit(n) => write!(f, "integer `{}`", n),
            TokenKind::StrLit(s) => write!(f, "string `\"{}\"`", s),
            other => write!(f, "{:?}", other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    /// One-based source line number.
    pub line: usize,
}

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    line: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Lexer {
            source: source.chars().collect(),
            pos: 0,
            line: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.source.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.source.get(self.pos).copied()?;
        self.pos += 1;
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
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        match s.as_str() {
            "fn" => TokenKind::Fn,
            "extern" => TokenKind::Extern,
            "let" => TokenKind::Let,
            "scope" => TokenKind::Scope,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "Int32" => TokenKind::TyInt32,
            "String" => TokenKind::TyString,
            "Bool" => TokenKind::TyBool,
            "Unit" => TokenKind::TyUnit,
            _ => TokenKind::Ident(s),
        }
    }

    fn read_number(&mut self) -> Result<TokenKind, Diagnostic> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        let n: i32 = s
            .parse()
            .map_err(|_| Diagnostic::new(self.line, "Integer literal is outside Int32 range"))?;
        Ok(TokenKind::IntLit(n))
    }

    /// Read a quoted string; the parser splits interpolation expressions.
    fn read_string(&mut self) -> Result<TokenKind, Diagnostic> {
        let start_line = self.line;
        // Skip the opening quote.
        self.advance();
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(Diagnostic::new(start_line, "Unterminated string literal")),
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
                            return Err(Diagnostic::new(start_line, "Unterminated string escape"))
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
        let line = self.line;
        let kind = match self.peek() {
            None => TokenKind::Eof,
            Some(c) if c.is_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            Some(c) if c.is_ascii_digit() => self.read_number()?,
            Some('"') => self.read_string()?,
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
            Some(',') => {
                self.advance();
                TokenKind::Comma
            }
            Some(':') => {
                self.advance();
                TokenKind::Colon
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
            Some('|') => {
                self.advance();
                TokenKind::Pipe
            }
            Some('@') => {
                self.advance();
                TokenKind::At
            }
            Some('=') => {
                self.advance();
                TokenKind::Eq
            }
            Some(c) => {
                return Err(Diagnostic::new(line, format!("Unexpected character {c:?}")));
            }
        };
        Ok(Token { kind, line })
    }

    /// Tokenize the entire source.
    pub fn tokenize(source: &str) -> Result<Vec<Token>, Diagnostic> {
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
