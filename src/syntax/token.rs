use crate::Span;

/// Decoded result of a string literal
///
/// The lexer records `source_offsets` while scanning, letting the parser map
/// interpolated expression spans back to the source without re-deriving them from delimiter shapes
#[derive(Clone)]
pub(super) struct StringLiteral {
    /// Value after decoding escape sequences and stripping indentation
    pub value: String,
    /// Raw string: no escape decoding and no interpolation
    pub raw: bool,
    /// Byte string (`b"…"`): typed `Bytes`, never interpolated
    pub bytes: bool,
    /// Source byte offset for each byte offset in `value`, length `value.len() + 1`;
    /// the last entry marks the end of the content (start of the closing delimiter)
    pub source_offsets: Vec<usize>,
}

/// Print only the value so token snapshots are unaffected by the offset table
impl std::fmt::Debug for StringLiteral {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.bytes {
            write!(formatter, "bytes {:?}", self.value)?;
        } else if self.raw {
            write!(formatter, "raw {:?}", self.value)?;
        } else {
            write!(formatter, "{:?}", self.value)?;
        }
        Ok(())
    }
}

/// The offset table is derived data and does not participate in equality comparison
impl PartialEq for StringLiteral {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.raw == other.raw && self.bytes == other.bytes
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum TokenKind {
    Integer(u64),
    TypedInteger(u64, super::IntegerSuffix),
    Float(f64),
    Duration(u64),
    Identifier(String),
    String(StringLiteral),
    True,
    False,
    Fn,
    Move,
    Eff,
    Effects,
    Do,
    With,
    Parallel,
    Race,
    Branch,
    Region,
    Struct,
    Class,
    Enum,
    Trait,
    Impl,
    For,
    Let,
    Echo,
    Var,
    Const,
    If,
    Match,
    Else,
    While,
    Loop,
    Break,
    Abort,
    Continue,
    When,
    Import,
    Pub,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Arithmetic(super::ArithmeticOp, super::ArithmeticMode, bool),
    Shl,
    Shr,
    Caret,
    Tilde,
    PlusEqual,
    MinusEqual,
    StarEqual,
    SlashEqual,
    PercentEqual,
    ShlEqual,
    ShrEqual,
    AmpEqual,
    CaretEqual,
    PipeEqual,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Colon,
    Comma,
    Semicolon,
    Newline,
    Equal,
    EqualEqual,
    NotEqual,
    Bang,
    AndAnd,
    Ampersand,
    OrOr,
    Pipe,
    Question,
    Hash,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Arrow,       // ->
    FatArrow,    // =>
    PipeGreater, // |>
    Dot,
    DotDot,
    DotDotEqual,
    At,
    LeftBracket,
    RightBracket,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
