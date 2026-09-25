use crate::diagnostic::LexError;
use crate::Span;

use super::token::{StringLiteral, Token, TokenKind};
use super::IntegerSuffix;

type Chars<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

pub(super) fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut chars = source.char_indices().peekable();
    let mut tokens = Vec::new();

    while let Some((start, character)) = chars.next() {
        let kind = match character {
            '\n' => TokenKind::Newline,
            '\r' => {
                if matches!(chars.peek().map(|(_, next)| *next), Some('\n')) {
                    chars.next();
                }
                TokenKind::Newline
            }
            character if character.is_whitespace() => continue,
            '/' if matches!(chars.peek().map(|(_, next)| *next), Some('/')) => {
                chars.next();
                while chars
                    .peek()
                    .is_some_and(|(_, next)| *next != '\n' && *next != '\r')
                {
                    chars.next();
                }
                continue;
            }
            '/' if matches!(chars.peek().map(|(_, next)| *next), Some('*')) => {
                chars.next();
                skip_block_comment(&mut chars, start)?;
                continue;
            }
            // `"…"`, `"""…"""`, `r"…"`, `r#"…"#`, `b"…"`, `br#"…"#` and their
            // multiline variants. `r` and `b` are only prefixes when followed by
            // `#`* plus a quote, otherwise they stay plain identifiers
            '"' | 'r' | 'b' if string_shape(source, start).is_some() => {
                tokens.push(lex_string(source, start, &mut chars)?);
                continue;
            }
            character if character.is_ascii_alphabetic() || character == '_' => {
                while chars
                    .peek()
                    .is_some_and(|(_, next)| next.is_ascii_alphanumeric() || *next == '_')
                {
                    chars.next();
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                let name = &source[start..end];
                let kind = match name {
                    "true" => TokenKind::True,
                    "false" => TokenKind::False,
                    "fn" => TokenKind::Fn,
                    "move" => TokenKind::Move,
                    "eff" => TokenKind::Eff,
                    "effects" => TokenKind::Effects,
                    "do" => TokenKind::Do,
                    "with" => TokenKind::With,
                    "parallel" => TokenKind::Parallel,
                    "race" => TokenKind::Race,
                    "branch" => TokenKind::Branch,
                    "region" => TokenKind::Region,
                    "struct" => TokenKind::Struct,
                    "class" => TokenKind::Class,
                    "enum" => TokenKind::Enum,
                    "trait" => TokenKind::Trait,
                    "impl" => TokenKind::Impl,
                    "for" => TokenKind::For,
                    "let" => TokenKind::Let,
                    "echo" => TokenKind::Echo,
                    "var" => TokenKind::Var,
                    "const" => TokenKind::Const,
                    "if" => TokenKind::If,
                    "match" => TokenKind::Match,
                    "else" => TokenKind::Else,
                    "while" => TokenKind::While,
                    "loop" => TokenKind::Loop,
                    "break" => TokenKind::Break,
                    "abort" => TokenKind::Abort,
                    "continue" => TokenKind::Continue,
                    "when" => TokenKind::When,
                    "import" => TokenKind::Import,
                    "pub" => TokenKind::Pub,
                    _ => TokenKind::Identifier(name.to_owned()),
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, end),
                });
                continue;
            }
            '+' | '-' | '*' | '/' | '%'
                if matches!(chars.peek().map(|(_, next)| *next), Some('?' | '%' | '|')) =>
            {
                use super::{ArithmeticMode, ArithmeticOp};
                let op = match character {
                    '+' => ArithmeticOp::Add,
                    '-' => ArithmeticOp::Subtract,
                    '*' => ArithmeticOp::Multiply,
                    '/' => ArithmeticOp::Divide,
                    '%' => ArithmeticOp::Remainder,
                    _ => unreachable!(),
                };
                let mode = match chars.next().unwrap().1 {
                    '?' => ArithmeticMode::Checked,
                    '%' => ArithmeticMode::Wrapping,
                    '|' => ArithmeticMode::Saturating,
                    _ => unreachable!(),
                };
                let assign = matches!(chars.peek().map(|(_, next)| *next), Some('='));
                if assign {
                    chars.next();
                }
                TokenKind::Arithmetic(op, mode, assign)
            }
            '+' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::PlusEqual
            }
            '+' => TokenKind::Plus,
            '%' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::PercentEqual
            }
            '%' => TokenKind::Percent,
            '^' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::CaretEqual
            }
            '^' => TokenKind::Caret,
            '~' => TokenKind::Tilde,
            '-' if matches!(chars.peek().map(|(_, next)| *next), Some('>')) => {
                chars.next();
                TokenKind::Arrow
            }
            '-' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::MinusEqual
            }
            '-' => TokenKind::Minus,
            '|' if matches!(chars.peek().map(|(_, next)| *next), Some('>')) => {
                chars.next();
                TokenKind::PipeGreater
            }
            '*' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::StarEqual
            }
            '*' => TokenKind::Star,
            '/' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::SlashEqual
            }
            '/' => TokenKind::Slash,
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '{' => TokenKind::LeftBrace,
            '}' => TokenKind::RightBrace,
            ':' => TokenKind::Colon,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semicolon,
            '=' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::EqualEqual
            }
            '=' if matches!(chars.peek().map(|(_, next)| *next), Some('>')) => {
                chars.next();
                TokenKind::FatArrow
            }
            '=' => TokenKind::Equal,
            '!' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::NotEqual
            }
            '!' => TokenKind::Bang,
            '&' if matches!(chars.peek().map(|(_, next)| *next), Some('&')) => {
                chars.next();
                TokenKind::AndAnd
            }
            '&' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::AmpEqual
            }
            '&' => TokenKind::Ampersand,
            '|' if matches!(chars.peek().map(|(_, next)| *next), Some('|')) => {
                chars.next();
                TokenKind::OrOr
            }
            '|' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::PipeEqual
            }
            '|' => TokenKind::Pipe,
            '?' => TokenKind::Question,
            '#' => TokenKind::Hash,
            '<' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::LessEqual
            }
            '<' if matches!(chars.peek().map(|(_, next)| *next), Some('<')) => {
                chars.next();
                if matches!(chars.peek().map(|(_, next)| *next), Some('=')) {
                    chars.next();
                    TokenKind::ShlEqual
                } else {
                    TokenKind::Shl
                }
            }
            '<' => TokenKind::Less,
            '>' if matches!(chars.peek().map(|(_, next)| *next), Some('=')) => {
                chars.next();
                TokenKind::GreaterEqual
            }
            '>' if matches!(chars.peek().map(|(_, next)| *next), Some('>')) => {
                chars.next();
                if matches!(chars.peek().map(|(_, next)| *next), Some('=')) {
                    chars.next();
                    TokenKind::ShrEqual
                } else {
                    TokenKind::Shr
                }
            }
            '>' => TokenKind::Greater,
            '.' if matches!(chars.peek().map(|(_, next)| *next), Some('.')) => {
                chars.next();
                if matches!(chars.peek().map(|(_, next)| *next), Some('=')) {
                    chars.next();
                    TokenKind::DotDotEqual
                } else {
                    TokenKind::DotDot
                }
            }
            '.' => TokenKind::Dot,
            '@' => TokenKind::At,
            '[' => TokenKind::LeftBracket,
            ']' => TokenKind::RightBracket,
            character if character.is_ascii_digit() => {
                let radix = if character == '0' {
                    match chars.peek().map(|(_, next)| *next) {
                        Some('x' | 'X') => Some(16),
                        Some('b' | 'B') => Some(2),
                        Some('o' | 'O') => Some(8),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(radix) = radix {
                    chars.next();
                    let (saw_digit, trailing_separator) = scan_separated_digits(&mut chars, radix);
                    let number_end = chars.peek().map_or(source.len(), |(index, _)| *index);
                    if !saw_digit || trailing_separator {
                        scan_numeric_tail(&mut chars);
                        let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                        return Err(LexError::InvalidNumericLiteral {
                            literal: source[start..end].to_owned(),
                            span: Span::new(start, end),
                        });
                    }
                    let suffix = lex_integer_suffix(source, start, &mut chars, false)?;
                    let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                    let digits: String = source[start + 2..number_end]
                        .chars()
                        .filter(|character| *character != '_')
                        .collect();
                    let value = u64::from_str_radix(&digits, radix).map_err(|_| {
                        LexError::IntegerOutOfRange {
                            literal: source[start..end].to_owned(),
                            span: Span::new(start, end),
                        }
                    })?;
                    tokens.push(Token {
                        kind: suffix.map_or(TokenKind::Integer(value), |suffix| {
                            TokenKind::TypedInteger(value, suffix)
                        }),
                        span: Span::new(start, end),
                    });
                    continue;
                }

                let (_, mut trailing_separator) = scan_separated_digits(&mut chars, 10);
                let mut is_float = false;
                if matches!(chars.peek().map(|(_, next)| *next), Some('.'))
                    && chars
                        .clone()
                        .nth(1)
                        .is_some_and(|(_, next)| next.is_ascii_digit())
                {
                    is_float = true;
                    chars.next();
                    trailing_separator = scan_separated_digits(&mut chars, 10).1;
                }
                if matches!(chars.peek().map(|(_, next)| *next), Some('e' | 'E')) {
                    is_float = true;
                    chars.next();
                    if matches!(chars.peek().map(|(_, next)| *next), Some('+' | '-')) {
                        chars.next();
                    }
                    if !chars.peek().is_some_and(|(_, next)| next.is_ascii_digit()) {
                        let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                        return Err(LexError::MissingExponentDigits {
                            span: Span::new(start, end),
                        });
                    }
                    trailing_separator = scan_separated_digits(&mut chars, 10).1;
                }
                if trailing_separator {
                    scan_numeric_tail(&mut chars);
                    let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                    return Err(LexError::InvalidNumericLiteral {
                        literal: source[start..end].to_owned(),
                        span: Span::new(start, end),
                    });
                }

                let mut duration_millis = None;
                let number_end = chars.peek().map_or(source.len(), |(index, _)| *index);
                let separated = source[start..number_end].contains('_');
                // Integer suffixes take precedence over decimal duration units.
                // Floats have no suffixes, but consume their tail for one diagnostic.
                let suffix = if is_float
                    || separated
                    || chars
                        .peek()
                        .is_some_and(|(_, next)| matches!(next, 'i' | 'u' | 'f'))
                {
                    lex_integer_suffix(source, start, &mut chars, is_float)?
                } else {
                    None
                };
                if !is_float
                    && !separated
                    && suffix.is_none()
                    && chars
                        .peek()
                        .is_some_and(|(_, next)| next.is_ascii_alphabetic())
                {
                    let mut total = 0_u64;
                    let mut segment = literal_value(
                        source,
                        start,
                        chars.peek().map_or(source.len(), |(index, _)| *index),
                    )?;
                    loop {
                        let unit_start = chars.peek().map_or(source.len(), |(index, _)| *index);
                        let Some((_, unit)) = chars.next() else {
                            return Err(LexError::InvalidDurationLiteral {
                                literal: source[start..].to_owned(),
                                span: Span::new(start, source.len()),
                            });
                        };
                        let multiplier = match unit {
                            'm' if chars.peek().is_some_and(|(_, next)| *next == 's') => {
                                chars.next();
                                1_u64
                            }
                            's' => 1_000,
                            'm' => 60_000,
                            'h' => 3_600_000,
                            _ => {
                                scan_numeric_tail(&mut chars);
                                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                                return Err(LexError::InvalidDurationLiteral {
                                    literal: source[start..end].to_owned(),
                                    span: Span::new(start, end.max(unit_start + unit.len_utf8())),
                                });
                            }
                        };
                        total = total
                            .checked_add(segment.checked_mul(multiplier).ok_or_else(|| {
                                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                                LexError::InvalidDurationLiteral {
                                    literal: source[start..end].to_owned(),
                                    span: Span::new(start, end),
                                }
                            })?)
                            .ok_or_else(|| {
                                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                                LexError::InvalidDurationLiteral {
                                    literal: source[start..end].to_owned(),
                                    span: Span::new(start, end),
                                }
                            })?;
                        let Some((digit_start, next)) = chars.peek().copied() else {
                            break;
                        };
                        if !next.is_ascii_digit() {
                            if next.is_ascii_alphabetic() || next == '_' {
                                scan_numeric_tail(&mut chars);
                                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                                return Err(LexError::InvalidDurationLiteral {
                                    literal: source[start..end].to_owned(),
                                    span: Span::new(start, end),
                                });
                            }
                            break;
                        }
                        chars.next();
                        while chars
                            .peek()
                            .is_some_and(|(_, character)| character.is_ascii_digit())
                        {
                            chars.next();
                        }
                        let digit_end = chars.peek().map_or(source.len(), |(index, _)| *index);
                        segment = source[digit_start..digit_end].parse::<u64>().map_err(|_| {
                            LexError::InvalidDurationLiteral {
                                literal: source[start..digit_end].to_owned(),
                                span: Span::new(start, digit_end),
                            }
                        })?;
                    }
                    duration_millis = Some(total);
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                let literal = &source[start..end];
                let kind = if let Some(value) = duration_millis {
                    TokenKind::Duration(value)
                } else if is_float {
                    let digits: String = source[start..number_end]
                        .chars()
                        .filter(|character| *character != '_')
                        .collect();
                    let value =
                        digits
                            .parse::<f64>()
                            .map_err(|_| LexError::InvalidFloatLiteral {
                                literal: literal.to_owned(),
                                span: Span::new(start, end),
                            })?;
                    if !value.is_finite() {
                        return Err(LexError::FloatOutOfRange {
                            literal: literal.to_owned(),
                            span: Span::new(start, end),
                        });
                    }
                    TokenKind::Float(value)
                } else {
                    let digits: String = source[start..number_end]
                        .chars()
                        .filter(|character| *character != '_')
                        .collect();
                    let value = digits
                        .parse::<u64>()
                        .map_err(|_| LexError::IntegerOutOfRange {
                            literal: literal.to_owned(),
                            span: Span::new(start, end),
                        })?;
                    suffix.map_or(TokenKind::Integer(value), |suffix| {
                        TokenKind::TypedInteger(value, suffix)
                    })
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, end),
                });
                continue;
            }
            unexpected => {
                let end = start + unexpected.len_utf8();
                return Err(LexError::UnexpectedCharacter {
                    character: unexpected,
                    span: Span::new(start, end),
                });
            }
        };

        let end = match kind {
            TokenKind::ShlEqual | TokenKind::ShrEqual => start + 3,
            TokenKind::EqualEqual
            | TokenKind::NotEqual
            | TokenKind::LessEqual
            | TokenKind::GreaterEqual
            | TokenKind::Shl
            | TokenKind::Shr
            | TokenKind::AndAnd
            | TokenKind::OrOr
            | TokenKind::PlusEqual
            | TokenKind::MinusEqual
            | TokenKind::StarEqual
            | TokenKind::SlashEqual
            | TokenKind::PercentEqual
            | TokenKind::AmpEqual
            | TokenKind::CaretEqual
            | TokenKind::PipeEqual
            | TokenKind::FatArrow
            | TokenKind::Arrow => start + character.len_utf8() * 2,
            _ => start + character.len_utf8(),
        };
        tokens.push(Token {
            kind,
            span: Span::new(start, end),
        });
    }

    Ok(tokens)
}

/// Shape of a string opening delimiter
struct StringShape {
    raw: bool,
    bytes: bool,
    hashes: usize,
    multiline: bool,
    /// Offset of the first byte after the opening delimiter
    content_start: usize,
}

/// Checks whether `start` begins a string literal. `r` and `b` only count as
/// prefixes when followed by `#`* plus a quote; otherwise returns `None` so the
/// main loop treats them as plain identifiers
fn string_shape(source: &str, start: usize) -> Option<StringShape> {
    let rest = source.get(start..)?;
    let (bytes, rest) = match rest.strip_prefix('b') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let (raw, rest) = match rest.strip_prefix('r') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let hashes = rest.bytes().take_while(|byte| *byte == b'#').count();
    let rest = &rest[hashes..];
    if !rest.starts_with('"') {
        return None;
    }
    let multiline = rest.starts_with(r#"""""#);
    Some(StringShape {
        raw,
        bytes,
        hashes,
        multiline,
        content_start: start
            + usize::from(bytes)
            + usize::from(raw)
            + hashes
            + if multiline { 3 } else { 1 },
    })
}

/// String value being decoded, with an offset back to the source for each byte
#[derive(Default)]
struct DecodedString {
    value: String,
    offsets: Vec<usize>,
    /// Byte positions in `value` of newlines that are literal in the source.
    /// Stripping indentation only recognizes these positions; newlines produced
    /// by the `\n` escape sequence are not physical line boundaries
    line_breaks: Vec<usize>,
}

impl DecodedString {
    fn push(&mut self, character: char, source_start: usize) {
        for _ in 0..character.len_utf8() {
            self.offsets.push(source_start);
        }
        self.value.push(character);
    }

    fn push_line_break(&mut self, source_start: usize) {
        self.line_breaks.push(self.value.len());
        self.push('\n', source_start);
    }

    fn finish(mut self, content_end: usize) -> (String, Vec<usize>, Vec<usize>) {
        self.offsets.push(content_end);
        (self.value, self.offsets, self.line_breaks)
    }
}

fn advance_to(chars: &mut Chars<'_>, target: usize) {
    while chars.peek().is_some_and(|(index, _)| *index < target) {
        chars.next();
    }
}

fn lex_string(source: &str, start: usize, chars: &mut Chars<'_>) -> Result<Token, LexError> {
    let shape = string_shape(source, start).expect("caller verified the opening delimiter");
    advance_to(chars, shape.content_start);
    if shape.multiline {
        skip_multiline_opener(source, start, chars)?;
    }

    let mut terminator = String::from(if shape.multiline { r#"""""# } else { "\"" });
    terminator.push_str(&"#".repeat(shape.hashes));

    let mut decoded = DecodedString::default();
    let content_end = loop {
        let Some((index, character)) = chars.peek().copied() else {
            return Err(LexError::UnterminatedString {
                span: Span::new(start, source.len()),
            });
        };
        if character == '"' && source[index..].starts_with(terminator.as_str()) {
            break index;
        }
        chars.next();
        if shape.bytes && !character.is_ascii() {
            return Err(LexError::NonAsciiByteString {
                span: Span::new(index, index + character.len_utf8()),
            });
        }
        match character {
            '\n' | '\r' if !shape.multiline => {
                return Err(LexError::UnterminatedString {
                    span: Span::new(start, index),
                });
            }
            '\n' => decoded.push_line_break(index),
            // Multiline strings always record line breaks as \n; handling Windows
            // CRLF does not change the string value
            '\r' => {
                if matches!(chars.peek(), Some((_, '\n'))) {
                    chars.next();
                }
                decoded.push_line_break(index);
            }
            '\\' if !shape.raw => {
                let Some((escape_index, escaped)) = chars.next() else {
                    return Err(LexError::UnterminatedString {
                        span: Span::new(start, source.len()),
                    });
                };
                let unescaped = match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => {
                        return Err(LexError::InvalidEscapeSequence {
                            sequence: escaped.to_string(),
                            span: Span::new(index, escape_index + escaped.len_utf8()),
                        });
                    }
                };
                decoded.push(unescaped, index);
            }
            character => decoded.push(character, index),
        }
    };

    let end = content_end + terminator.len();
    advance_to(chars, end);

    let (value, source_offsets, line_breaks) = decoded.finish(content_end);
    let (value, source_offsets) = if shape.multiline {
        dedent_multiline(value, source_offsets, &line_breaks)?
    } else {
        (value, source_offsets)
    };

    Ok(Token {
        kind: TokenKind::String(StringLiteral {
            value,
            raw: shape.raw,
            bytes: shape.bytes,
            source_offsets,
        }),
        span: Span::new(start, end),
    })
}

/// Only whitespace may follow the opening delimiter on the same line, then a
/// newline is required; that newline is not part of the content
fn skip_multiline_opener(
    source: &str,
    start: usize,
    chars: &mut Chars<'_>,
) -> Result<(), LexError> {
    loop {
        let Some((index, character)) = chars.next() else {
            return Err(LexError::UnterminatedString {
                span: Span::new(start, source.len()),
            });
        };
        match character {
            ' ' | '\t' => {}
            '\n' => return Ok(()),
            '\r' => {
                if matches!(chars.peek(), Some((_, '\n'))) {
                    chars.next();
                }
                return Ok(());
            }
            character => {
                return Err(LexError::MultilineStringOpenerNotAlone {
                    span: Span::new(start, index + character.len_utf8()),
                });
            }
        }
    }
}

fn is_blank(text: &str) -> bool {
    text.bytes().all(|byte| byte == b' ' || byte == b'\t')
}

/// Strips the shared prefix of every line, using the leading whitespace of the
/// closing delimiter's line as the baseline, and trims the offset table to match.
/// The newline before the closing delimiter is also not part of the content
fn dedent_multiline(
    value: String,
    offsets: Vec<usize>,
    line_breaks: &[usize],
) -> Result<(String, Vec<usize>), LexError> {
    // The last physical line holds the closing delimiter; its leading whitespace
    // is the baseline indentation
    let closer_start = line_breaks.last().map_or(0, |index| index + 1);
    let baseline = &value[closer_start..];
    if !is_blank(baseline) {
        return Err(LexError::MultilineStringCloserNotAlone {
            span: Span::new(offsets[closer_start], offsets[value.len()]),
        });
    }

    let mut result = String::new();
    let mut result_offsets = Vec::new();
    let mut line_start = 0;
    for (position, line_end) in line_breaks.iter().copied().enumerate() {
        let line = &value[line_start..line_end];
        if position > 0 {
            // The line separator itself sits at the end of the previous line
            result_offsets.push(offsets[line_start - 1]);
            result.push('\n');
        }
        // Whitespace-only lines are dropped entirely and emit an empty line;
        // every other line must match the baseline indentation character by character
        let kept = if is_blank(line) {
            ""
        } else {
            line.strip_prefix(baseline)
                .ok_or_else(|| LexError::MultilineStringIndentMismatch {
                    span: Span::new(offsets[line_start], offsets[line_end]),
                })?
        };
        result_offsets.extend(offsets[line_end - kept.len()..line_end].iter().copied());
        result.push_str(kept);
        line_start = line_end + 1;
    }
    result_offsets.push(offsets[closer_start.saturating_sub(1)]);

    Ok((result, result_offsets))
}

/// Consumes digits and `_` separators. Returns whether at least one digit was
/// seen, and whether it ended with `_`
fn scan_separated_digits(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    radix: u32,
) -> (bool, bool) {
    let mut saw_digit = false;
    let mut trailing_separator = false;
    while let Some((_, next)) = chars.peek().copied() {
        if next == '_' {
            chars.next();
            trailing_separator = true;
            continue;
        }
        if next.is_digit(radix) {
            chars.next();
            saw_digit = true;
            trailing_separator = false;
            continue;
        }
        break;
    }
    (saw_digit, trailing_separator)
}

fn scan_numeric_tail(chars: &mut Chars<'_>) {
    while chars
        .peek()
        .is_some_and(|(_, next)| next.is_ascii_alphanumeric() || *next == '_')
    {
        chars.next();
    }
}

fn lex_integer_suffix(
    source: &str,
    start: usize,
    chars: &mut Chars<'_>,
    is_float: bool,
) -> Result<Option<IntegerSuffix>, LexError> {
    let suffix_start = chars.peek().map_or(source.len(), |(index, _)| *index);
    scan_numeric_tail(chars);
    let end = chars.peek().map_or(source.len(), |(index, _)| *index);
    if suffix_start == end {
        return Ok(None);
    }
    let suffix = &source[suffix_start..end];
    // A digit left after a radix scan is invalid in that base.
    if suffix.as_bytes()[0].is_ascii_digit() {
        return Err(LexError::InvalidNumericLiteral {
            literal: source[start..end].to_owned(),
            span: Span::new(start, end),
        });
    }
    if !is_float {
        if let Some(suffix) = IntegerSuffix::parse(suffix) {
            return Ok(Some(suffix));
        }
    }
    Err(LexError::InvalidNumericSuffix {
        suffix: suffix.to_owned(),
        span: Span::new(start, end),
    })
}

fn literal_value(source: &str, start: usize, end: usize) -> Result<u64, LexError> {
    source[start..end]
        .parse::<u64>()
        .map_err(|_| LexError::IntegerOutOfRange {
            literal: source[start..end].to_owned(),
            span: Span::new(start, end),
        })
}

fn skip_block_comment(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    start: usize,
) -> Result<(), LexError> {
    let mut depth = 1;
    let mut end = start + 2;

    while let Some((index, character)) = chars.next() {
        end = index + character.len_utf8();
        match character {
            '/' if matches!(chars.peek().map(|(_, next)| *next), Some('*')) => {
                chars.next();
                depth += 1;
            }
            '*' if matches!(chars.peek().map(|(_, next)| *next), Some('/')) => {
                chars.next();
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }

    Err(LexError::UnterminatedBlockComment {
        span: Span::new(start, end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexer_tracks_source_spans() {
        let tokens = lex("12 + 3").unwrap();
        assert_eq!(tokens[0].span, Span::new(0, 2));
        assert_eq!(tokens[1].span, Span::new(3, 4));
        assert_eq!(tokens[2].span, Span::new(5, 6));
    }

    #[test]
    fn lexer_reports_the_invalid_character() {
        let error = lex("2 $ 3").unwrap_err();
        assert_eq!(error.span(), Span::new(2, 3));
        assert_eq!(error.message(), "unexpected character: '$'");
    }

    #[test]
    fn lexer_skips_line_and_nested_block_comments() {
        let tokens =
            lex("// file comment\n12 /* outer /* inner */ outer */ + 3 /// trailing\n").unwrap();
        let tokens: Vec<_> = tokens
            .into_iter()
            .filter(|token| !matches!(token.kind, TokenKind::Newline))
            .collect();
        assert_eq!(tokens.len(), 3);
        assert!(matches!(tokens[0].kind, TokenKind::Integer(12)));
        assert!(matches!(tokens[1].kind, TokenKind::Plus));
        assert!(matches!(tokens[2].kind, TokenKind::Integer(3)));
    }

    #[test]
    fn lexer_skips_module_and_function_doc_comment_forms() {
        let tokens = lex("//! module docs\n/// function docs\n12").unwrap();
        let tokens: Vec<_> = tokens
            .into_iter()
            .filter(|token| !matches!(token.kind, TokenKind::Newline))
            .collect();
        assert_eq!(tokens.len(), 1);
        assert!(matches!(tokens[0].kind, TokenKind::Integer(12)));
    }

    #[test]
    fn lexer_reports_unterminated_block_comments() {
        let error = lex("1 /* nested /* comment */").unwrap_err();
        assert_eq!(error.message(), "unterminated block comment");
        assert_eq!(error.span(), Span::new(2, 24));
    }

    #[test]
    fn lexer_reads_radix_and_separated_numeric_literals() {
        let tokens = lex("0x10 0b1010 0o10 1_000 1_000.5_0 0x_ff").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(16)));
        assert!(matches!(tokens[1].kind, TokenKind::Integer(10)));
        assert!(matches!(tokens[2].kind, TokenKind::Integer(8)));
        assert!(matches!(tokens[3].kind, TokenKind::Integer(1_000)));
        assert!(matches!(tokens[4].kind, TokenKind::Float(value) if value == 1000.5));
        assert!(matches!(tokens[5].kind, TokenKind::Integer(255)));
        assert!(matches!(
            lex("1_").unwrap_err(),
            LexError::InvalidNumericLiteral { .. }
        ));
        assert!(matches!(
            lex("0x").unwrap_err(),
            LexError::InvalidNumericLiteral { .. }
        ));
        let duration = lex("1s").unwrap();
        assert!(matches!(duration[0].kind, TokenKind::Duration(1_000)));
    }

    #[test]
    fn lexer_reads_integer_and_float_literals() {
        let tokens = lex("42 1.5 2e3 4.0e-2").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(42)));
        assert!(matches!(tokens[1].kind, TokenKind::Float(value) if value == 1.5));
        assert!(matches!(tokens[2].kind, TokenKind::Float(value) if value == 2000.0));
        assert!(matches!(tokens[3].kind, TokenKind::Float(value) if value == 0.04));
    }

    #[test]
    fn integer_suffixes_cover_every_width_radix_and_span() {
        for (suffix, kind) in [
            ("i8", IntegerSuffix::I8),
            ("i16", IntegerSuffix::I16),
            ("i32", IntegerSuffix::I32),
            ("i64", IntegerSuffix::I64),
            ("u8", IntegerSuffix::U8),
            ("u16", IntegerSuffix::U16),
            ("u32", IntegerSuffix::U32),
            ("u64", IntegerSuffix::U64),
        ] {
            for digits in [
                "123",
                "1_23",
                "0x7b",
                "0X7B",
                "0x_7b",
                "0b111_1011",
                "0B1111011",
                "0o173",
                "0O173",
            ] {
                let literal = format!("{digits}{suffix}");
                let tokens = lex(&literal).unwrap();
                assert_eq!(tokens.len(), 1, "{literal}");
                assert_eq!(
                    tokens[0].kind,
                    TokenKind::TypedInteger(123, kind),
                    "{literal}"
                );
                assert_eq!(tokens[0].span, Span::new(0, literal.len()));
            }
        }
        for literal in ["18446744073709551615u64", "0xffff_ffff_ffff_ffffu64"] {
            assert_eq!(
                lex(literal).unwrap()[0].kind,
                TokenKind::TypedInteger(u64::MAX, IntegerSuffix::U64)
            );
        }
    }

    #[test]
    fn integer_suffixes_reject_invalid_tails_as_one_literal() {
        for literal in [
            "1u",
            "1i",
            "1u7",
            "1i128",
            "1u8u16",
            "1u8foo",
            "1u8_",
            "0xffu8abc",
            "0o7u16x",
            "1.0u8",
            "1e2i16",
            "1f32",
            "1.5f64",
            "1_000u128",
        ] {
            let error = lex(literal).unwrap_err();
            assert!(
                matches!(error, LexError::InvalidNumericSuffix { .. }),
                "{literal}: {error:?}"
            );
            assert_eq!(error.span(), Span::new(0, literal.len()), "{literal}");
            assert!(error.message().contains("integer literals support"));
        }
    }

    #[test]
    fn integer_suffixes_reject_bad_radix_digits_and_separators() {
        for literal in [
            "0b102u8", "0o78i16", "0xu8", "0x_u8", "1_u8", "0xff_u8", "0b2u8",
        ] {
            let error = lex(literal).unwrap_err();
            assert!(
                matches!(error, LexError::InvalidNumericLiteral { .. }),
                "{literal}: {error:?}"
            );
            assert_eq!(error.span(), Span::new(0, literal.len()), "{literal}");
        }
        for literal in ["18446744073709551616u64", "0x10000000000000000u64"] {
            let error = lex(literal).unwrap_err();
            assert!(matches!(error, LexError::IntegerOutOfRange { .. }));
            assert_eq!(error.span(), Span::new(0, literal.len()));
        }
    }

    #[test]
    fn integer_suffixes_preserve_duration_units_and_token_boundaries() {
        let tokens = lex("1u8 100ms 1s 1h10m100s -128i8 1u8..2u8 1u8.max(2u8)").unwrap();
        assert_eq!(
            tokens[0].kind,
            TokenKind::TypedInteger(1, IntegerSuffix::U8)
        );
        assert_eq!(tokens[1].kind, TokenKind::Duration(100));
        assert_eq!(tokens[2].kind, TokenKind::Duration(1000));
        assert_eq!(tokens[3].kind, TokenKind::Duration(4300000));
        assert_eq!(tokens[4].kind, TokenKind::Minus);
        assert_eq!(
            tokens[5].kind,
            TokenKind::TypedInteger(128, IntegerSuffix::I8)
        );
        assert_eq!(tokens[7].kind, TokenKind::DotDot);
        assert_eq!(tokens[10].kind, TokenKind::Dot);
        for literal in ["1msu8", "1sfoo", "1h2mu8", "1s_"] {
            let error = lex(literal).unwrap_err();
            assert!(matches!(error, LexError::InvalidDurationLiteral { .. }));
            assert_eq!(error.span(), Span::new(0, literal.len()));
        }
    }

    #[test]
    fn lexer_reads_duration_literals_in_milliseconds() {
        let tokens = lex("1000ms 1s 2m 1h10m100s").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Duration(1_000)));
        assert!(matches!(tokens[1].kind, TokenKind::Duration(1_000)));
        assert!(matches!(tokens[2].kind, TokenKind::Duration(120_000)));
        assert!(matches!(tokens[3].kind, TokenKind::Duration(4_300_000)));
    }

    #[test]
    fn lexer_rejects_unknown_duration_units() {
        let error = lex("1d").unwrap_err();
        assert!(matches!(error, LexError::InvalidDurationLiteral { .. }));
    }

    #[test]
    fn lexer_preserves_newlines_as_expression_boundaries() {
        let tokens = lex("1\r\n2").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(1)));
        assert!(matches!(tokens[1].kind, TokenKind::Newline));
        assert!(matches!(tokens[2].kind, TokenKind::Integer(2)));
    }

    /// Extracts the string literal of the first token in the source
    fn string_literal(source: &str) -> StringLiteral {
        let tokens = lex(source).unwrap();
        assert_eq!(tokens.len(), 1);
        match &tokens[0].kind {
            TokenKind::String(literal) => literal.clone(),
            other => panic!("expected string token, got {other:?}"),
        }
    }

    #[test]
    fn lexer_handles_string_escapes() {
        let literal = string_literal(r#""hello\nworld\t\"quoted\"""#);
        assert_eq!(literal.value, "hello\nworld\t\"quoted\"");
        assert!(!literal.raw);
    }

    #[test]
    fn lexer_reports_unterminated_string() {
        let error = lex(r#""unterminated"#).unwrap_err();
        assert_eq!(error.message(), "unterminated string literal");
    }

    #[test]
    fn lexer_reports_a_newline_inside_a_single_line_string() {
        let error = lex("\"open\nclose\"").unwrap_err();
        assert_eq!(error.message(), "unterminated string literal");
        assert_eq!(error.span(), Span::new(0, 5));
    }

    #[test]
    fn lexer_keeps_raw_string_backslashes_and_braces() {
        let literal = string_literal(r###"r"\d+\.\d+ {x} C:\Users""###);
        assert_eq!(literal.value, r"\d+\.\d+ {x} C:\Users");
        assert!(literal.raw);
    }

    #[test]
    fn lexer_uses_hashes_to_raise_the_raw_string_delimiter() {
        assert_eq!(
            string_literal(r###"r#"{"k": "v"}"#"###).value,
            r#"{"k": "v"}"#
        );
        assert_eq!(
            string_literal(r###"r##"ends with "#"##"###).value,
            r##"ends with "#"##
        );
    }

    #[test]
    fn lexer_reads_an_empty_raw_string() {
        assert_eq!(string_literal(r###"r"""###).value, "");
    }

    #[test]
    fn lexer_treats_a_bare_r_as_an_identifier() {
        let tokens = lex("r + rank r#[1]").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Identifier(ref name) if name == "r"));
        assert!(matches!(tokens[1].kind, TokenKind::Plus));
        assert!(matches!(tokens[2].kind, TokenKind::Identifier(ref name) if name == "rank"));
        assert!(matches!(tokens[3].kind, TokenKind::Identifier(ref name) if name == "r"));
        assert!(matches!(tokens[4].kind, TokenKind::Hash));
        assert!(matches!(tokens[5].kind, TokenKind::LeftBracket));
    }

    #[test]
    fn lexer_reports_an_unterminated_raw_string() {
        let error = lex(r###"r#"open"###).unwrap_err();
        assert_eq!(error.message(), "unterminated string literal");
    }

    #[test]
    fn lexer_decodes_byte_string_escapes() {
        let literal = string_literal(r#"b"chunked\tcopy\n""#);
        assert_eq!(literal.value, "chunked\tcopy\n");
        assert!(literal.bytes);
        assert!(!literal.raw);
    }

    #[test]
    fn lexer_keeps_raw_byte_string_backslashes_and_braces() {
        let literal = string_literal(r###"br"\d+ {x}""###);
        assert_eq!(literal.value, r"\d+ {x}");
        assert!(literal.bytes);
        assert!(literal.raw);
        assert_eq!(string_literal(r###"b#""Hello""#"###).value, r#""Hello""#);
    }

    #[test]
    fn lexer_reports_non_ascii_characters_in_byte_strings() {
        let error = lex("b\"café\"").unwrap_err();
        assert_eq!(
            error.message(),
            "byte string literals may contain only ASCII characters"
        );
    }

    #[test]
    fn lexer_treats_a_bare_b_as_an_identifier() {
        let tokens = lex("b + bravo b#[1]").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Identifier(ref name) if name == "b"));
        assert!(matches!(tokens[1].kind, TokenKind::Plus));
        assert!(matches!(tokens[2].kind, TokenKind::Identifier(ref name) if name == "bravo"));
        assert!(matches!(tokens[3].kind, TokenKind::Identifier(ref name) if name == "b"));
        assert!(matches!(tokens[4].kind, TokenKind::Hash));
        assert!(matches!(tokens[5].kind, TokenKind::LeftBracket));
    }

    #[test]
    fn lexer_strips_multiline_indentation_to_the_closing_delimiter() {
        let literal = string_literal("\"\"\"\n    SELECT *\n      FROM t\n    \"\"\"");
        assert_eq!(literal.value, "SELECT *\n  FROM t");
    }

    #[test]
    fn lexer_keeps_a_trailing_newline_when_the_closer_follows_a_blank_line() {
        let literal = string_literal("\"\"\"\n    line\n\n    \"\"\"");
        assert_eq!(literal.value, "line\n");
    }

    #[test]
    fn lexer_blanks_out_whitespace_only_multiline_rows() {
        let literal = string_literal("\"\"\"\n    a\n  \n    b\n    \"\"\"");
        assert_eq!(literal.value, "a\n\nb");
    }

    #[test]
    fn lexer_reads_an_empty_multiline_string() {
        assert_eq!(string_literal("\"\"\"\n    \"\"\"").value, "");
    }

    #[test]
    fn lexer_normalizes_multiline_line_endings() {
        let literal = string_literal("\"\"\"\r\n  a\r\n  b\r  \"\"\"");
        assert_eq!(literal.value, "a\nb");
    }

    #[test]
    fn lexer_keeps_escapes_and_interpolation_braces_in_multiline_strings() {
        let literal = string_literal("\"\"\"\n  a\\tb {x}\n  \"\"\"");
        assert_eq!(literal.value, "a\tb {x}");
        assert!(!literal.raw);
    }

    #[test]
    fn lexer_combines_raw_and_multiline_strings() {
        let literal = string_literal("r\"\"\"\n  path ~ '\\d+' {x}\n  \"\"\"");
        assert_eq!(literal.value, r"path ~ '\d+' {x}");
        assert!(literal.raw);

        let hashed = string_literal("r#\"\"\"\n  says \"\"\" inside\n  \"\"\"#");
        assert_eq!(hashed.value, "says \"\"\" inside");
    }

    #[test]
    fn lexer_reports_content_after_the_multiline_opener() {
        let error = lex("\"\"\"oops\n\"\"\"").unwrap_err();
        assert_eq!(
            error.message(),
            "multiline string must start on the line after \"\"\""
        );
    }

    #[test]
    fn lexer_reports_a_multiline_closer_sharing_its_line() {
        let error = lex("\"\"\"\n  text\"\"\"").unwrap_err();
        assert_eq!(
            error.message(),
            "closing \"\"\" of a multiline string must be on its own line"
        );
    }

    #[test]
    fn lexer_reports_a_multiline_row_indented_less_than_the_closer() {
        let error = lex("\"\"\"\n    deep\nshallow\n    \"\"\"").unwrap_err();
        assert_eq!(
            error.message(),
            "line is less indented than the closing \"\"\""
        );
    }

    #[test]
    fn lexer_maps_multiline_offsets_back_to_the_source() {
        let source = "\"\"\"\n  ab\n  cd\n  \"\"\"";
        let literal = string_literal(source);
        assert_eq!(literal.value, "ab\ncd");
        assert_eq!(literal.source_offsets.len(), literal.value.len() + 1);
        // Every kept byte maps back to the same character in the source
        for (offset, byte) in literal.source_offsets.iter().zip(literal.value.bytes()) {
            assert_eq!(source.as_bytes()[*offset], byte);
        }
    }

    #[test]
    fn lexer_reports_invalid_escape_sequence() {
        let error = lex(r#""invalid\x""#).unwrap_err();
        assert!(error.message().contains("invalid escape sequence"));
    }

    #[test]
    fn lexer_handles_all_operators() {
        let tokens = lex("+ - * / == != < <= > >= ! && ||").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Plus));
        assert!(matches!(tokens[1].kind, TokenKind::Minus));
        assert!(matches!(tokens[2].kind, TokenKind::Star));
        assert!(matches!(tokens[3].kind, TokenKind::Slash));
        assert!(matches!(tokens[4].kind, TokenKind::EqualEqual));
        assert!(matches!(tokens[5].kind, TokenKind::NotEqual));
        assert!(matches!(tokens[6].kind, TokenKind::Less));
        assert!(matches!(tokens[7].kind, TokenKind::LessEqual));
        assert!(matches!(tokens[8].kind, TokenKind::Greater));
        assert!(matches!(tokens[9].kind, TokenKind::GreaterEqual));
        assert!(matches!(tokens[10].kind, TokenKind::Bang));
        assert!(matches!(tokens[11].kind, TokenKind::AndAnd));
        assert!(matches!(tokens[12].kind, TokenKind::OrOr));
    }

    #[test]
    fn lexer_handles_all_keywords() {
        let tokens = lex(
            "fn struct class enum let var const if match else while loop break continue true false",
        )
        .unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Fn));
        assert!(matches!(tokens[1].kind, TokenKind::Struct));
        assert!(matches!(tokens[2].kind, TokenKind::Class));
        assert!(matches!(tokens[3].kind, TokenKind::Enum));
        assert!(matches!(tokens[4].kind, TokenKind::Let));
        assert!(matches!(tokens[5].kind, TokenKind::Var));
        assert!(matches!(tokens[6].kind, TokenKind::Const));
        assert!(matches!(tokens[7].kind, TokenKind::If));
        assert!(matches!(tokens[8].kind, TokenKind::Match));
        assert!(matches!(tokens[9].kind, TokenKind::Else));
        assert!(matches!(tokens[10].kind, TokenKind::While));
        assert!(matches!(tokens[11].kind, TokenKind::Loop));
        assert!(matches!(tokens[12].kind, TokenKind::Break));
        assert!(matches!(tokens[13].kind, TokenKind::Continue));
        assert!(matches!(tokens[14].kind, TokenKind::True));
        assert!(matches!(tokens[15].kind, TokenKind::False));
    }

    #[test]
    fn lexer_distinguishes_identifiers_from_keywords() {
        let tokens = lex("letx fn_name while_loop").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Identifier(ref s) if s == "letx"));
        assert!(matches!(tokens[1].kind, TokenKind::Identifier(ref s) if s == "fn_name"));
        assert!(matches!(tokens[2].kind, TokenKind::Identifier(ref s) if s == "while_loop"));
    }

    #[test]
    fn lexer_handles_large_integers() {
        let tokens = lex("18446744073709551615").unwrap(); // u64::MAX
        assert!(matches!(
            tokens[0].kind,
            TokenKind::Integer(18446744073709551615)
        ));
    }

    #[test]
    fn lexer_reports_integer_overflow() {
        let error = lex("18446744073709551616").unwrap_err(); // u64::MAX + 1
                                                              // Just verify we get an error for too large integer
        assert!(matches!(error, LexError::IntegerOutOfRange { .. }));
    }

    #[test]
    fn lexer_handles_float_edge_cases() {
        let tokens = lex("0.0 1e308 1e-308").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Float(f) if f == 0.0));
        assert!(matches!(tokens[1].kind, TokenKind::Float(_)));
        assert!(matches!(tokens[2].kind, TokenKind::Float(_)));
    }

    #[test]
    fn lexer_handles_empty_input() {
        let tokens = lex("").unwrap();
        assert_eq!(tokens.len(), 0);
    }

    #[test]
    fn lexer_handles_whitespace_only() {
        let tokens = lex("   \t  \n  ").unwrap();
        // Should only have newline token
        assert_eq!(tokens.len(), 1);
        assert!(matches!(tokens[0].kind, TokenKind::Newline));
    }

    #[test]
    fn lexer_handles_mixed_line_endings() {
        let tokens = lex("1\n2\r\n3\r4").unwrap();
        let newline_count = tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Newline))
            .count();
        assert_eq!(newline_count, 3); // \n, \r\n, \r should all be newlines
    }

    #[test]
    fn lexer_handles_unicode_identifiers() {
        // Non-ASCII characters should be rejected
        let error = lex("变量").unwrap_err();
        assert!(error.message().contains("unexpected character"));
    }
}
