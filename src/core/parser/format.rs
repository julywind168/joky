//! Conservative source formatting over tokens and their original trivia.
//!
//! Comments and literals remain verbatim, and no expression newline is moved.
//! The parser validates input; a second lexical pass guards token boundaries.
use super::{Diagnostic, Lexer, Parser, Token, TokenKind};

/// Format one syntactically valid module without resolving imports or types.
///
/// Uses four-space delimiter indentation and conservative horizontal spacing.
/// Existing newlines, comments and literal spellings are preserved. In particular,
/// interpolation bodies and multiline string indentation are not rewritten.
pub fn format(source: &str) -> Result<String, Diagnostic> {
    let tokens = Lexer::tokenize(source)?;
    Parser::new(tokens.clone()).parse_module()?;
    let pieces = pieces(source, &tokens);
    let mut output = String::with_capacity(source.len());
    let mut depth = 0usize;
    let mut previous_token: Option<&Token> = None;
    let mut previous_prefix = false;
    for (index, piece) in pieces.iter().enumerate() {
        let kind = piece.token.map(|token| &token.kind);
        let prefix = piece.token.is_some_and(|token| {
            is_prefix(
                &token.kind,
                previous_token
                    .filter(|previous| previous.end_line == token.line)
                    .map(|previous| &previous.kind),
            )
        });
        let indent = depth.saturating_sub(usize::from(kind.is_some_and(is_close)));
        if piece.gap.contains('\n') {
            // Preserve each physical newline, including CRLF, but discard spaces
            // on blank lines and trailing spaces outside comments and literals.
            let bytes = piece.gap.as_bytes();
            for (i, byte) in bytes.iter().enumerate() {
                if *byte == b'\n' {
                    if i > 0 && bytes[i - 1] == b'\r' {
                        output.push('\r');
                    }
                    output.push('\n');
                }
            }
        }
        if output.is_empty() || output.ends_with('\n') {
            output.extend(std::iter::repeat_n(' ', indent * 4));
        } else if let Some(previous) = index.checked_sub(1).map(|i| &pieces[i]) {
            if space_between(previous, piece, previous_prefix, prefix) {
                output.push(' ');
            }
        }
        output.push_str(piece.text);
        if let Some(kind) = kind {
            if is_close(kind) {
                depth = depth.saturating_sub(1);
            } else if matches!(
                kind,
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket
            ) {
                depth += 1;
            }
            previous_token = piece.token;
            previous_prefix = prefix;
        }
    }
    // Preserve trailing blank lines without retaining their horizontal spaces.
    let trailing = pieces.last().map_or(source, |piece| &source[piece.end..]);
    for (i, byte) in trailing.bytes().enumerate() {
        if byte == b'\n' {
            if i > 0 && trailing.as_bytes()[i - 1] == b'\r' {
                output.push('\r');
            }
            output.push('\n');
        }
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        });
    }
    verify_tokens(source, &tokens, &output)?;
    Ok(output)
}

struct Piece<'a> {
    text: &'a str,
    gap: &'a str,
    end: usize,
    token: Option<&'a Token>,
}

/// The lexer has already established that gaps contain only whitespace/comments.
/// Scan just these gaps, never strings or interpolation, to retain comment bytes.
fn pieces<'a>(source: &'a str, tokens: &'a [Token]) -> Vec<Piece<'a>> {
    let mut result = Vec::new();
    let mut end = 0;
    for token in tokens {
        let start = token.span.start();
        let mut cursor = end;
        while cursor < start {
            let rest = &source[cursor..start];
            if rest.starts_with("//") || rest.starts_with("/*") {
                let length = if rest.starts_with("//") {
                    rest.find('\n').unwrap_or(rest.len())
                } else {
                    let mut depth = 1;
                    let mut i = 2;
                    while depth > 0 {
                        if rest[i..].starts_with("/*") {
                            depth += 1;
                            i += 2;
                        } else if rest[i..].starts_with("*/") {
                            depth -= 1;
                            i += 2;
                        } else {
                            i += rest[i..].chars().next().unwrap().len_utf8();
                        }
                    }
                    i
                };
                result.push(Piece {
                    text: &source[cursor..cursor + length],
                    gap: &source[end..cursor],
                    end: cursor + length,
                    token: None,
                });
                cursor += length;
                end = cursor;
            } else {
                cursor += rest.chars().next().unwrap().len_utf8();
            }
        }
        if token.kind != TokenKind::Eof {
            result.push(Piece {
                text: &source[start..token.span.end()],
                gap: &source[end..start],
                end: token.span.end(),
                token: Some(token),
            });
            end = token.span.end();
        }
    }
    result
}

fn is_close(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket
    )
}

fn ends_operand(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::IntLit(_)
            | TokenKind::FloatLit(_)
            | TokenKind::TupleIndex(_)
            | TokenKind::StrLit(_)
            | TokenKind::Interpolated(_)
            | TokenKind::CharLit(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Question
            | TokenKind::Bang
    )
}

fn is_prefix(kind: &TokenKind, previous: Option<&TokenKind>) -> bool {
    matches!(
        kind,
        TokenKind::Minus | TokenKind::Star | TokenKind::Amp | TokenKind::Bang | TokenKind::Tilde
    ) && !previous.is_some_and(ends_operand)
}

fn binary(kind: &TokenKind, prefix: bool) -> bool {
    !prefix
        && matches!(
            kind,
            TokenKind::Eq
                | TokenKind::EqEq
                | TokenKind::Ne
                | TokenKind::Le
                | TokenKind::Ge
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Percent
                | TokenKind::Amp
                | TokenKind::Pipe
                | TokenKind::Caret
                | TokenKind::AndAnd
                | TokenKind::OrOr
                | TokenKind::Arrow
                | TokenKind::FatArrow
                | TokenKind::CompoundAssign(_)
        )
}

fn space_between(
    left: &Piece<'_>,
    right: &Piece<'_>,
    left_prefix: bool,
    right_prefix: bool,
) -> bool {
    let (Some(left), Some(right_token)) = (left.token, right.token) else {
        // Keep comments separated from code and one another, including `/* */ /`.
        return true;
    };
    use TokenKind::*;
    let (a, b) = (&left.kind, &right_token.kind);
    // Generic brackets vs comparisons/shifts and imports vs division need more
    // syntax context. Preserve their existing separation rather than guess.
    if matches!(a, Lt | Gt | Shl | Shr | Slash) || matches!(b, Lt | Gt | Shl | Shr | Slash) {
        return !right.gap.is_empty();
    }
    // Removing this separator can turn member access into a floating literal.
    if matches!(a, IntLit(_)) && matches!(b, Dot) {
        return true;
    }
    // References must not turn into a single logical-and token.
    if matches!((a, b), (Amp, Amp)) {
        return true;
    }
    if matches!(
        b,
        Comma | Semi | Colon | RParen | RBracket | Dot | ColonColon | Question
    ) || matches!(a, LParen | LBracket | Dot | ColonColon | At | Hash)
    {
        return false;
    }
    if matches!((a, b), (LBrace, RBrace)) {
        return false;
    }
    if binary(a, left_prefix) || binary(b, right_prefix) {
        return true;
    }
    if left_prefix || (matches!(b, Bang) && !right_prefix) {
        return false;
    }
    if matches!(b, LParen | LBracket) {
        return matches!(a, If | While | Match | Return | In | Colon | Comma | Semi)
            || matches!(a, Ident(name) if name == "when");
    }
    true
}

fn verify_tokens(source: &str, original: &[Token], output: &str) -> Result<(), Diagnostic> {
    let error = || {
        Diagnostic::at(
            1,
            0..source.len(),
            "Formatting would change token boundaries; source left unchanged",
        )
    };
    let formatted = Lexer::tokenize(output).map_err(|_| error())?;
    if original.len() != formatted.len() {
        return Err(error());
    }
    for (before, after) in original.iter().zip(&formatted) {
        if before.kind == TokenKind::Eof {
            continue;
        }
        if std::mem::discriminant(&before.kind) != std::mem::discriminant(&after.kind)
            || before.span.slice(source) != after.span.slice(output)
            || before.line != after.line
            || before.end_line != after.end_line
        {
            return Err(error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
