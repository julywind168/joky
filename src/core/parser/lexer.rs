//! Joky lexer.
mod duration;
mod strings;
use super::ast::BinOp;
use super::Diagnostic;
use crate::Span;

/// Token kinds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    // Keywords.
    Fn,
    Move,
    Pub,
    Extern,
    Unsafe,
    Import,
    Type,
    Struct,
    Enum,
    Impl,
    Trait,
    Dyn,
    For,
    In,
    Const,
    Match,
    Let,
    Var,
    Mut,
    While,
    If,
    Else,
    Return,
    Break,
    Continue,
    Parallel, // Legacy keyword retained only for migration diagnostics.
    Scope,
    Race,
    Branch,
    True,
    False,

    // Type keywords.
    TyFloat32,
    TyFloat64,
    TyInt32,
    TyInt8,
    TyInt16,
    TyInt64,
    TyUInt8,
    TyUInt16,
    TyUInt32,
    TyRune,
    TyUInt64,
    TyString,
    TyPtr,
    TyBool,
    TyUnit,

    // Identifiers and literals.
    Ident(String),
    // Unsigned magnitude permits the special spelling of the signed minimum.
    IntLit(u64),
    DurationLit(u64),
    FloatLit(String),
    /// Decimal member index following a dot, never a floating literal.
    TupleIndex(u64),
    StrLit(String), // Decoded UTF-8 text; braces have no special meaning.
    Interpolated(Vec<InterpolationPart>),
    CharLit(char),

    // Punctuation.
    LBracket,   // [
    RBracket,   // ]
    LParen,     // (
    RParen,     // )
    LBrace,     // {
    RBrace,     // }
    Comma,      // ,
    Dot,        // .
    DotDot,     // ..
    DotDotEq,   // ..=
    Colon,      // :
    ColonColon, // ::
    Semi,       // ;
    Arrow,      // ->
    FatArrow,   // =>
    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /
    Percent,    // %
    Pipe,       // |
    Amp,        // &
    Caret,      // ^
    Tilde,      // ~
    Shl,        // <<
    Shr,        // >>
    AndAnd,     // &&
    OrOr,       // ||
    At,         // @
    Hash,       // #
    Eq,         // =
    EqEq,       // ==
    Ne,         // !=
    Bang,       // !
    Question,   // ?
    Lt,         // <
    Le,         // <=
    Gt,         // >
    Ge,         // >=

    /// Arithmetic or bitwise assignment.
    CompoundAssign(BinOp),

    // End of input.
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    /// One-based source line number.
    pub(crate) line: usize,
    /// One-based ending line, which can differ for multiline or interpolated strings.
    pub(crate) end_line: usize,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InterpolationPart {
    Text(String, Span),
    Expression(Vec<Token>),
}

pub(crate) struct Lexer<'a> {
    source: &'a str,
    byte_pos: usize,
    line: usize,
    interpolation_depth: usize,
    after_dot: bool,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Lexer {
            source,
            byte_pos: 0,
            line: 1,
            interpolation_depth: 0,
            after_dot: false,
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

    fn skip_whitespace_and_comments(&mut self) -> Result<(), Diagnostic> {
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
            } else if self.peek() == Some('/') && self.peek2() == Some('*') {
                let start = self.byte_pos;
                let line = self.line;
                self.advance();
                self.advance();
                let mut depth = 1usize;
                while depth > 0 {
                    match (self.peek(), self.peek2()) {
                        (Some('/'), Some('*')) => {
                            self.advance();
                            self.advance();
                            depth += 1;
                        }
                        (Some('*'), Some('/')) => {
                            self.advance();
                            self.advance();
                            depth -= 1;
                        }
                        (Some(_), _) => {
                            self.advance();
                        }
                        (None, _) => {
                            return Err(Diagnostic::at(
                                line,
                                start..self.byte_pos,
                                "Unterminated block comment",
                            ));
                        }
                    }
                }
            } else {
                return Ok(());
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
            "move" => TokenKind::Move,
            "pub" => TokenKind::Pub,
            "extern" => TokenKind::Extern,
            "unsafe" => TokenKind::Unsafe,
            "import" => TokenKind::Import,
            "type" => TokenKind::Type,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "impl" => TokenKind::Impl,
            "trait" => TokenKind::Trait,
            "dyn" => TokenKind::Dyn,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "const" => TokenKind::Const,
            "match" => TokenKind::Match,
            "let" => TokenKind::Let,
            "var" => TokenKind::Var,
            "mut" => TokenKind::Mut,
            "while" => TokenKind::While,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "return" => TokenKind::Return,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "parallel" => TokenKind::Parallel,
            "scope" => TokenKind::Scope,
            "race" => TokenKind::Race,
            "branch" => TokenKind::Branch,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "Float32" => TokenKind::TyFloat32,
            "Float64" => TokenKind::TyFloat64,
            "Int32" => TokenKind::TyInt32,
            "Int8" => TokenKind::TyInt8,
            "Int16" => TokenKind::TyInt16,
            "Int64" => TokenKind::TyInt64,
            "UInt8" => TokenKind::TyUInt8,
            "UInt16" => TokenKind::TyUInt16,
            "UInt32" => TokenKind::TyUInt32,
            "Rune" => TokenKind::TyRune,
            "UInt64" => TokenKind::TyUInt64,
            "String" => TokenKind::TyString,
            "Ptr" => TokenKind::TyPtr,
            "Bool" => TokenKind::TyBool,
            "Unit" => TokenKind::TyUnit,
            name => TokenKind::Ident(name.into()),
        }
    }

    fn read_number(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        // Consume the entire numeric word so invalid digits, suffixes and
        // misplaced separators cannot silently become adjacent expressions.
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.advance();
        }
        if self.after_dot {
            let spelling = &self.source[start..self.byte_pos];
            if spelling.bytes().all(|c| c.is_ascii_digit()) {
                if let Ok(index) = spelling.parse() {
                    return Ok(TokenKind::TupleIndex(index));
                }
            }
            return Err(Diagnostic::at(
                self.line,
                start..self.byte_pos,
                "Tuple member index requires decimal digits within UInt64 range",
            ));
        }
        if !self.source[start..self.byte_pos].starts_with("0x")
            && !self.source[start..self.byte_pos].starts_with("0b")
            && self.peek() == Some('.')
            && self.peek2().is_some_and(|c| c.is_ascii_digit())
        {
            self.advance();
            while self.peek().is_some_and(|c| {
                c.is_alphanumeric()
                    || c == '_'
                    || ((c == '+' || c == '-')
                        && self.source[..self.byte_pos].ends_with(['e', 'E']))
            }) {
                self.advance();
            }
            let spelling = &self.source[start..self.byte_pos];
            if let Some(result) = duration::parse(spelling) {
                return result
                    .map(TokenKind::DurationLit)
                    .map_err(|message| Diagnostic::at(self.line, start..self.byte_pos, message));
            }
            let invalid = || {
                Diagnostic::at(self.line, start..self.byte_pos, "Invalid floating literal: expected decimal digits on both sides of the dot and an optional signed exponent")
            };
            let separated_digits = |part: &str| {
                !part.is_empty()
                    && part.bytes().enumerate().all(|(i, c)| {
                        c.is_ascii_digit()
                            || (c == b'_'
                                && i > 0
                                && i + 1 < part.len()
                                && part.as_bytes()[i - 1].is_ascii_digit()
                                && part.as_bytes()[i + 1].is_ascii_digit())
                    })
            };
            let (mantissa, exponent) = spelling
                .split_once(['e', 'E'])
                .map_or((spelling, None), |(m, e)| (m, Some(e)));
            let (whole, fraction) = mantissa.split_once('.').ok_or_else(invalid)?;
            if !separated_digits(whole)
                || !separated_digits(fraction)
                || exponent
                    .is_some_and(|e| !separated_digits(e.strip_prefix(['+', '-']).unwrap_or(e)))
            {
                return Err(invalid());
            }
            return Ok(TokenKind::FloatLit(spelling.replace('_', "")));
        }
        let spelling = &self.source[start..self.byte_pos];
        if !spelling.starts_with("0x") && !spelling.starts_with("0b") {
            if let Some(result) = duration::parse(spelling) {
                return result
                    .map(TokenKind::DurationLit)
                    .map_err(|message| Diagnostic::at(self.line, start..self.byte_pos, message));
            }
        }
        let (radix, digits) = if let Some(digits) = spelling.strip_prefix("0x") {
            (16, digits)
        } else if let Some(digits) = spelling.strip_prefix("0b") {
            (2, digits)
        } else {
            (10, spelling)
        };
        let malformed = || {
            Diagnostic::at(self.line, start..self.byte_pos,
            "Invalid integer literal: use decimal, 0x hexadecimal or 0b binary digits, with single underscores between digits")
        };
        let mut magnitude = Some(0u64);
        let mut previous_digit = false;
        for ch in digits.chars() {
            if ch == '_' {
                if !previous_digit {
                    return Err(malformed());
                }
                previous_digit = false;
            } else {
                let digit = ch
                    .to_digit(radix)
                    .filter(|_| ch.is_ascii())
                    .ok_or_else(malformed)?;
                magnitude = magnitude
                    .and_then(|n| n.checked_mul(u64::from(radix)))
                    .and_then(|n| n.checked_add(u64::from(digit)));
                previous_digit = true;
            }
        }
        if !previous_digit {
            return Err(malformed());
        }
        magnitude.map(TokenKind::IntLit).ok_or_else(|| {
            Diagnostic::at(
                self.line,
                start..self.byte_pos,
                "Integer literal is outside UInt64 magnitude range",
            )
        })
    }

    /// Decode a Unicode escape after its `u`, retaining the original byte span.
    fn read_unicode_escape(&mut self, start: usize, line: usize) -> Result<char, Diagnostic> {
        let mut value = 0u32;
        let mut digits = 0usize;
        let mut valid = self.peek() == Some('{');
        let mut closed = false;
        if valid {
            self.advance();
            while let Some(ch) = self.peek() {
                if ch == '}' {
                    self.advance();
                    closed = true;
                    break;
                }
                if matches!(ch, '"' | '\'' | '\n' | '\r') {
                    break;
                }
                self.advance();
                digits += 1;
                if let Some(digit) = ch.to_digit(16).filter(|_| ch.is_ascii()) {
                    // Keep scanning malformed escapes for a complete diagnostic,
                    // but never let an arbitrarily long spelling overflow.
                    if digits <= 6 {
                        value = value * 16 + digit;
                    }
                } else {
                    valid = false;
                }
            }
        }
        if valid && closed && (1..=6).contains(&digits) {
            if let Some(ch) = char::from_u32(value) {
                return Ok(ch);
            }
        }
        Err(Diagnostic::at(
            line,
            start..self.byte_pos,
            "Invalid Unicode escape: expected \\u{H...} with 1–6 hexadecimal digits encoding a Unicode scalar",
        ))
    }

    fn read_char(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        let line = self.line;
        self.advance();
        let value = match self.advance() {
            Some('\\') => {
                let escape_start = self.byte_pos - 1;
                match self.advance() {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('0') => '\0',
                    Some('\'') => '\'',
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('u') => self.read_unicode_escape(escape_start, line)?,
                    _ => {
                        return Err(Diagnostic::at(
                            line,
                            escape_start..self.byte_pos,
                            "Invalid character escape",
                        ))
                    }
                }
            }
            Some('\n' | '\r' | '\'') | None => {
                return Err(Diagnostic::at(
                    line,
                    start..self.byte_pos,
                    "Character literal must contain exactly one Unicode scalar",
                ))
            }
            Some(ch) => ch,
        };
        if self.peek() != Some('\'') {
            // Include all extra scalars and the closing quote in the diagnostic.
            while let Some(ch) = self.peek() {
                if matches!(ch, '\n' | '\r') {
                    break;
                }
                self.advance();
                if ch == '\'' {
                    break;
                }
            }
            return Err(Diagnostic::at(
                line,
                start..self.byte_pos,
                "Character literal must contain exactly one Unicode scalar",
            ));
        }
        self.advance();
        Ok(TokenKind::CharLit(value))
    }

    /// Read the next token.
    fn next_token(&mut self) -> Result<Token, Diagnostic> {
        self.skip_whitespace_and_comments()?;
        let start = self.byte_pos;
        let line = self.line;
        let kind = match self.peek() {
            None => TokenKind::Eof,
            // `f"` used to opt into interpolation; ordinary strings now interpolate.
            Some('f') if self.peek2() == Some('"') => {
                return Err(Diagnostic::at(
                    line,
                    start..start + 2,
                    "Interpolation is the default in string literals; remove the `f` prefix",
                ))
            }
            Some('r') if self.raw_string_prefix() => self.read_string(start)?,
            Some(c) if c.is_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            Some(c) if c.is_ascii_digit() => self.read_number(start)?,
            Some('"') => self.read_string(start)?,
            Some('\'') => self.read_char(start)?,
            Some('[') => {
                self.advance();
                TokenKind::LBracket
            }
            Some(']') => {
                self.advance();
                TokenKind::RBracket
            }
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
                if self.peek() == Some('.') {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::DotDotEq
                    } else {
                        TokenKind::DotDot
                    }
                } else {
                    TokenKind::Dot
                }
            }
            Some(',') => {
                self.advance();
                TokenKind::Comma
            }
            Some(':') => {
                self.advance();
                if self.peek() == Some(':') {
                    self.advance();
                    TokenKind::ColonColon
                } else {
                    TokenKind::Colon
                }
            }
            Some(';') => {
                self.advance();
                TokenKind::Semi
            }
            Some(ch @ ('+' | '-' | '*' | '/' | '%' | '&' | '|' | '^'))
                if self.peek2() == Some('=') =>
            {
                self.advance();
                self.advance();
                TokenKind::CompoundAssign(match ch {
                    '+' => BinOp::Add,
                    '-' => BinOp::Sub,
                    '*' => BinOp::Mul,
                    '/' => BinOp::Div,
                    '%' => BinOp::Rem,
                    '&' => BinOp::BitAnd,
                    '|' => BinOp::BitOr,
                    '^' => BinOp::BitXor,
                    _ => unreachable!(),
                })
            }
            Some('%') => {
                self.advance();
                TokenKind::Percent
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
            Some('&') => {
                self.advance();
                TokenKind::Amp
            }
            Some('^') => {
                self.advance();
                TokenKind::Caret
            }
            Some('~') => {
                self.advance();
                TokenKind::Tilde
            }
            Some('<') if self.peek2() == Some('<') => {
                self.advance();
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::CompoundAssign(BinOp::Shl)
                } else {
                    TokenKind::Shl
                }
            }
            Some('>') if self.peek2() == Some('>') => {
                self.advance();
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::CompoundAssign(BinOp::Shr)
                } else {
                    TokenKind::Shr
                }
            }
            Some('|') => {
                self.advance();
                TokenKind::Pipe
            }
            Some('@') => {
                self.advance();
                TokenKind::At
            }
            Some('#') => {
                self.advance();
                TokenKind::Hash
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
            Some('?') => {
                self.advance();
                TokenKind::Question
            }
            Some('!') => {
                self.advance();
                TokenKind::Bang
            }
            Some('>') => {
                self.advance();
                TokenKind::Gt
            }
            Some('=') if self.peek2() == Some('>') => {
                self.advance();
                self.advance();
                TokenKind::FatArrow
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
        self.after_dot = kind == TokenKind::Dot;
        Ok(Token {
            kind,
            line,
            end_line: self.line,
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
    fn range_tokens_distinguish_floats_tuple_members_and_integer_bounds() {
        let tokens = Lexer::tokenize(
            "1..2 1..=2 1.5..=2.5 x.0..x.1 0xffffffffffffffff..=0xffffffffffffffff",
        )
        .unwrap();
        let kinds: Vec<_> = tokens.into_iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::IntLit(1),
                TokenKind::DotDot,
                TokenKind::IntLit(2),
                TokenKind::IntLit(1),
                TokenKind::DotDotEq,
                TokenKind::IntLit(2),
                TokenKind::FloatLit("1.5".into()),
                TokenKind::DotDotEq,
                TokenKind::FloatLit("2.5".into()),
                TokenKind::Ident("x".into()),
                TokenKind::Dot,
                TokenKind::TupleIndex(0),
                TokenKind::DotDot,
                TokenKind::Ident("x".into()),
                TokenKind::Dot,
                TokenKind::TupleIndex(1),
                TokenKind::IntLit(u64::MAX),
                TokenKind::DotDotEq,
                TokenKind::IntLit(u64::MAX),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn block_comments_preserve_token_boundaries_lines_and_utf8_spans() {
        let source = "a/**/b/* outer 🙂\r\n/* inner\n*/ end */42/*tail*/";
        let tokens = Lexer::tokenize(source).unwrap();
        for (token, kind, text, line) in [
            (&tokens[0], TokenKind::Ident("a".into()), "a", 1),
            (&tokens[1], TokenKind::Ident("b".into()), "b", 1),
            (&tokens[2], TokenKind::IntLit(42), "42", 3),
            (&tokens[3], TokenKind::Eof, "", 3),
        ] {
            assert_eq!(token.kind, kind);
            assert_eq!(token.span.slice(source), Some(text));
            assert_eq!(token.line, line);
            assert_eq!(token.end_line, line);
        }
        assert_eq!(tokens[3].span.start(), source.len());
    }

    #[test]
    fn block_comments_mix_with_line_comments_and_operators() {
        let tokens = Lexer::tokenize(
            "// /* ignored\n/* // \" ' } ignored */ /**//**/ / /*gap*/* /= /*gap*/ *= // end",
        )
        .unwrap();
        assert_eq!(
            tokens
                .into_iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                TokenKind::Slash,
                TokenKind::Star,
                TokenKind::CompoundAssign(BinOp::Div),
                TokenKind::CompoundAssign(BinOp::Mul),
                TokenKind::Eof,
            ]
        );
        for source in ["/**/", "/*/**/*/", "/* ***/", "/* // */"] {
            assert_eq!(Lexer::tokenize(source).unwrap()[0].kind, TokenKind::Eof);
        }
    }

    #[test]
    fn block_comments_nest_without_recursion() {
        let source = format!("{}{}42", "/*".repeat(4096), "*/".repeat(4096));
        assert_eq!(
            Lexer::tokenize(&source).unwrap()[0].kind,
            TokenKind::IntLit(42)
        );
    }

    #[test]
    fn unterminated_block_comments_report_the_outer_opening_and_complete_span() {
        for comment in [
            "/*",
            "/*/",
            "/* text *",
            "/* 🙂\n/* nested */",
            "/* /* */ /*",
        ] {
            let source = format!("// é\n  {comment}");
            let error = Lexer::tokenize(&source).unwrap_err();
            assert_eq!(error.message, "Unterminated block comment");
            assert_eq!(error.line, 2);
            assert_eq!(error.span.slice(&source), Some(comment));
        }
    }

    #[test]
    fn block_comments_only_apply_inside_interpolation_expressions() {
        let source = r##""/* text */" r#"/* raw */"# '/' '*' "{/* } \" /* nested */ */ 42}""##;
        let tokens = Lexer::tokenize(source).unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StrLit("/* text */".into()));
        assert_eq!(tokens[1].kind, TokenKind::StrLit("/* raw */".into()));
        assert_eq!(tokens[2].kind, TokenKind::CharLit('/'));
        assert_eq!(tokens[3].kind, TokenKind::CharLit('*'));
        let TokenKind::Interpolated(parts) = &tokens[4].kind else {
            panic!("Expected interpolation parts");
        };
        let InterpolationPart::Expression(expression) = &parts[1] else {
            panic!("Expected interpolation expression");
        };
        assert_eq!(expression[0].kind, TokenKind::IntLit(42));
        assert_eq!(expression[1].kind, TokenKind::Eof);
        let error = Lexer::tokenize(r#""{/* unterminated }""#).unwrap_err();
        assert_eq!(error.message, "Unterminated block comment");
    }

    #[test]
    fn radix_literals_keep_magnitudes_and_complete_utf8_spans() {
        for (text, value) in [
            ("0", 0),
            ("000_042", 42),
            ("1_000", 1000),
            ("0xff", 255),
            ("0xA_BcD", 0xabcd),
            ("0b1010_0011", 163),
            ("18_446_744_073_709_551_615", u64::MAX),
            ("0xffff_ffff_ffff_ffff", u64::MAX),
            (
                "0b1111111111111111111111111111111111111111111111111111111111111111",
                u64::MAX,
            ),
        ] {
            let source = format!("// 🙂\n{text}");
            let tokens = Lexer::tokenize(&source).unwrap();
            assert_eq!(tokens[0].kind, TokenKind::IntLit(value));
            assert_eq!(tokens[0].span.slice(&source), Some(text));
            assert_eq!(tokens[0].line, 2);
        }
        for text in [
            "0x", "0b", "0x_ff", "0b_1", "0xff_", "1_", "1__0", "0b102", "0xfg", "123abc", "1u32",
            "0Xff", "0B10", "0o77", "1é", "1_é",
        ] {
            let source = format!("// 🙂\n{text}");
            let error = Lexer::tokenize(&source).unwrap_err();
            assert_eq!(error.span.slice(&source), Some(text), "{text}");
            assert_eq!(error.line, 2);
            assert!(error.message.contains("Invalid integer literal"));
        }
        for text in [
            "18_446_744_073_709_551_616",
            "0x1_0000_0000_0000_0000",
            "0b10000000000000000000000000000000000000000000000000000000000000000",
        ] {
            let source = format!("// 🙂\n{text}");
            let error = Lexer::tokenize(&source).unwrap_err();
            assert_eq!(error.span.slice(&source), Some(text));
            assert!(error.message.contains("outside UInt64 magnitude range"));
        }
        let tokens = Lexer::tokenize("~ & && | || ^ << >> <= >=").unwrap();
        assert_eq!(
            tokens.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(),
            [
                TokenKind::Tilde,
                TokenKind::Amp,
                TokenKind::AndAnd,
                TokenKind::Pipe,
                TokenKind::OrOr,
                TokenKind::Caret,
                TokenKind::Shl,
                TokenKind::Shr,
                TokenKind::Le,
                TokenKind::Ge,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn plain_strings_interpolate_by_default_and_reject_the_f_prefix() {
        let tokens = Lexer::tokenize(r#""a{x}b""#).unwrap();
        let TokenKind::Interpolated(parts) = &tokens[0].kind else {
            panic!("Expected interpolation parts")
        };
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], InterpolationPart::Text(text, _) if text == "a"));
        assert!(matches!(&parts[1], InterpolationPart::Expression(_)));
        assert!(matches!(&parts[2], InterpolationPart::Text(text, _) if text == "b"));
        // Hole-free literals keep the plain string token kind.
        let tokens = Lexer::tokenize(r#""{{}}""#).unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StrLit("{}".into()));
        let error = Lexer::tokenize(r#"f"x""#).unwrap_err();
        assert!(error.message.contains("remove the `f` prefix"));
        let error = Lexer::tokenize(r#""}""#).unwrap_err();
        assert!(error.message.contains("Unmatched '}'"));
    }

    #[test]
    fn string_escapes_decode_once_without_changing_source_offsets() {
        let source = "// é\r\n\"hé🙂{{value}}\\n\\r\\t\\0\\\"\\\\end\" \"\\\\n\" \"\"\nnext";
        let tokens = Lexer::tokenize(source).unwrap();
        for (token, expected) in tokens.iter().zip(["hé🙂{value}\n\r\t\0\"\\end", "\\n", ""]) {
            assert_eq!(token.kind, TokenKind::StrLit(expected.into()));
            assert_eq!(token.line, 2);
            assert!(token.span.slice(source).unwrap().starts_with('"'));
        }
        assert_eq!(tokens[0].span.start(), source.find('"').unwrap());
        assert_eq!(
            tokens[0].span.slice(source),
            Some(r#""hé🙂{{value}}\n\r\t\0\"\\end""#)
        );
        assert_eq!(tokens[3].line, 3);
        assert_eq!(tokens[3].span.slice(source), Some("next"));
    }

    #[test]
    fn unicode_string_escapes_validate_scalars_and_preserve_source_spans() {
        for scalar in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
        ] {
            let literal = format!(r#""中\u{{{scalar:06X}}}""#);
            let source = format!("// 🙂\n{literal}");
            let tokens = Lexer::tokenize(&source).unwrap();
            assert_eq!(
                tokens[0].kind,
                TokenKind::StrLit(format!("中{}", char::from_u32(scalar).unwrap()))
            );
            assert_eq!(tokens[0].span.slice(&source), Some(literal.as_str()));
            assert_eq!(tokens[0].line, 2);
        }
        let tokens = Lexer::tokenize(r#""\u{41}\u{a}\u{0}Z\\u{{41}}""#).unwrap();
        assert_eq!(tokens[0].kind, TokenKind::StrLit("A\n\0Z\\u{41}".into()));
        for escape in [
            r"\u{}",
            r"\u{0000000}",
            r"\u{D800}",
            r"\u{DFFF}",
            r"\u{110000}",
            r"\u{FFFFFF}",
            r"\u{1_2}",
            r"\u{g}",
            r"\u{中}",
            r"\u{ 41}",
            r"\u{41",
            r"\u",
            r"\u41",
        ] {
            let source = format!("// 🙂\n\"中{escape}\"");
            let error = Lexer::tokenize(&source).unwrap_err();
            let marked = if escape == r"\u41" { r"\u" } else { escape };
            assert_eq!(error.span.slice(&source), Some(marked), "{source:?}");
            assert_eq!(error.line, 2);
            assert!(error.message.contains("Invalid Unicode escape"));
        }
        for source in ["\"中\\u{41\nnext\"", "\"中\\u{41\rnext\"", "\"中\\u{41"] {
            let error = Lexer::tokenize(source).unwrap_err();
            assert_eq!(error.span.slice(source), Some(r"\u{41"));
        }
    }

    #[test]
    fn character_literals_cover_scalar_boundaries_and_original_byte_spans() {
        for scalar in [
            0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
        ] {
            let literal = format!("'\\u{{{scalar:06X}}}'");
            let source = format!("// 中🙂\n{literal}");
            let tokens = Lexer::tokenize(&source).unwrap();
            assert_eq!(
                tokens[0].kind,
                TokenKind::CharLit(char::from_u32(scalar).unwrap())
            );
            assert_eq!(tokens[0].span.slice(&source), Some(literal.as_str()));
            assert_eq!(tokens[0].line, 2);
        }
        for literal in ["'ab'", "'中🙂'", "'e\u{301}'", "''", "'中"] {
            let source = format!("// 中🙂\n{literal}");
            let error = Lexer::tokenize(&source).unwrap_err();
            assert_eq!(error.span.slice(&source), Some(literal));
            assert_eq!(error.line, 2);
        }
    }

    #[test]
    fn invalid_strings_report_precise_utf8_byte_ranges() {
        for (literal, marked, message) in [
            (r#""hé\q""#, "\\q", "Unknown string escape"),
            (r#""hé\🙂""#, "\\🙂", "Unknown string escape"),
            (r#""\x00""#, "\\x", "Unknown string escape"),
            (r#""\'""#, "\\'", "Unknown string escape"),
            ("\"hé\nnext\"", "\n", "Line break in string literal"),
            ("\"hé\r\nnext\"", "\r", "Line break in string literal"),
            ("\"hé\rnext\"", "\r", "Line break in string literal"),
            ("\"hé\\\nnext\"", "\\\n", "Line continuation is not allowed"),
            (
                "\"hé\\\r\nnext\"",
                "\\\r",
                "Line continuation is not allowed",
            ),
            ("\"hé", "\"hé", "Unterminated string literal"),
            ("\"hé\\", "\"hé\\", "Unterminated string escape"),
        ] {
            let source = format!("// 🙂\n{literal}");
            let error = Lexer::tokenize(&source).unwrap_err();
            assert_eq!(error.line, 2, "{source:?}");
            assert_eq!(error.span.slice(&source), Some(marked), "{source:?}");
            assert!(error.message.contains(message), "{source:?}: {error}");
            let start = source.rfind(marked).unwrap();
            assert_eq!(error.span, Span::new(start, start + marked.len()));
        }
    }

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
            ("// é\n18446744073709551616", "18446744073709551616"),
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
        let src = r#""Hello, {{name}}!""#;
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
