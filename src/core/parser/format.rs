//! Syntax-aware formatting over original tokens, comments and literal spellings.
mod layout;
mod skip;
mod syntax;
mod wrap;
use super::{Diagnostic, Token, TokenKind};
use syntax::Syntax;

/// Format valid syntax without resolving imports, checking types or running code.
/// Nonempty blocks expand, expression continuations indent, and syntax roles
/// distinguish operators from generic, import and closure punctuation.
/// Long lists and expressions use a soft 100-character width. Standalone
/// `// fmt: off` / `// fmt: on` comments preserve complete source regions.
pub fn format(source: &str) -> Result<String, Diagnostic> {
    let syntax = Syntax::parse(source)?;
    let pieces = pieces(source, &syntax.tokens);
    let skipped = skip::regions(source, &pieces)?;
    let output = layout::render(source, &syntax, &pieces, &skipped);
    let output = skip::restore(source, &skipped, output)?;
    verify(source, &syntax, &output)?;
    Ok(output)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Role {
    #[default]
    Normal,
    TypeOpen,
    TypeClose,
    Prefix,
    ClosureOpen,
    ClosureClose,
    EmptyClosure,
    ImportSlash,
    ImportOpen,
    ImportClose,
    BlockOpen,
    BlockClose,
    ArmsOpen,
    ArmsClose,
    DeclarationsOpen,
    DeclarationsClose,
}

fn is_open(kind: &TokenKind, role: Role) -> bool {
    role == Role::TypeOpen
        || matches!(
            kind,
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket
        )
}

fn is_close(kind: &TokenKind, role: Role) -> bool {
    role == Role::TypeClose
        || matches!(
            kind,
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket
        )
}

struct Piece<'a> {
    text: &'a str,
    gap: &'a str,
    end: usize,
    token: Option<usize>,
}

/// The lexer has already established that gaps contain only whitespace/comments.
/// Scan just these gaps, never strings or interpolation, to retain comment bytes.
fn pieces<'a>(source: &'a str, tokens: &[Token]) -> Vec<Piece<'a>> {
    let mut result = Vec::new();
    let mut end = 0;
    for (index, token) in tokens.iter().enumerate() {
        let start = token.span.start();
        let mut cursor = end;
        while cursor < start {
            let rest = &source[cursor..start];
            if rest.starts_with("//") || rest.starts_with("/*") {
                let length = if rest.starts_with("//") {
                    rest.find(['\r', '\n']).unwrap_or(rest.len())
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
                token: Some(index),
            });
            end = token.span.end();
        }
    }
    result
}

fn verify(source: &str, original: &Syntax, output: &str) -> Result<(), Diagnostic> {
    let error = || {
        Diagnostic::at(
            1,
            0..source.len(),
            "Formatting would change source structure; source left unchanged",
        )
    };
    let formatted = Syntax::parse(output).map_err(|_| error())?;
    if original.tokens.len() != formatted.tokens.len() || !original.same_structure(&formatted) {
        return Err(error());
    }
    for (before, after) in original.tokens.iter().zip(&formatted.tokens) {
        if before.kind != TokenKind::Eof
            && (std::mem::discriminant(&before.kind) != std::mem::discriminant(&after.kind)
                || before.span.slice(source) != after.span.slice(output))
        {
            return Err(error());
        }
    }
    if comments(source, &original.tokens) != comments(output, &formatted.tokens) {
        return Err(error());
    }
    Ok(())
}

fn comments<'a>(source: &'a str, tokens: &[Token]) -> Vec<&'a str> {
    pieces(source, tokens)
        .into_iter()
        .filter(|piece| piece.token.is_none())
        .map(|piece| piece.text)
        .collect()
}

#[cfg(test)]
mod tests;
