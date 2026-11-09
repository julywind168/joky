//! Structured groups and syntax-safe width breaks preserve expression boundaries.
use super::{
    is_close, is_open,
    syntax::Syntax,
    wrap::{after_operator, Wrap},
    Piece, Role, TokenKind,
};
use std::ops::Range;

struct Frame {
    open: usize,
    piece: usize,
    role: Role,
    base: usize,
    expanded: bool,
}

pub(super) fn render(
    source: &str,
    syntax: &Syntax,
    pieces: &[Piece<'_>],
    skipped: &[Range<usize>],
) -> String {
    let mut wrap = Wrap::new(syntax, pieces, skipped);
    loop {
        wrap.changed = false;
        let output = render_once(source, syntax, pieces, &mut wrap);
        if !wrap.changed {
            return output;
        }
    }
}

fn render_once(source: &str, syntax: &Syntax, pieces: &[Piece<'_>], wrap: &mut Wrap<'_>) -> String {
    let ending = source
        .find(['\r', '\n'])
        .map_or("\n", |i| line_ending(&source[i..]));
    let mut output = String::with_capacity(source.len());
    if source.starts_with('\u{feff}') {
        output.push('\u{feff}');
    }
    let mut frames: Vec<Frame> = Vec::new();
    let mut pending_break = false;
    let mut line_indent = 0;
    let mut next_token = 0;
    let mut indents = vec![0; syntax.tokens.len()];
    let mut column = 0;
    for (index, piece) in pieces.iter().enumerate() {
        let token = piece.token.map(|i| &syntax.tokens[i]);
        let role = piece.token.map_or(Role::Normal, |i| syntax.roles[i]);
        let anchor = piece
            .token
            .and_then(|i| syntax.anchors[i])
            .map(|i| indents[i]);
        let closing = token.is_some_and(|t| is_close(&t.kind, role));
        let empty_close = closing
            && frames.last().is_some_and(|frame| {
                // Comments are pieces too: a comment-only body is never collapsed.
                index > 0
                    && pieces[index - 1].token == Some(frame.open)
                    && syntax.pairs[frame.open] == piece.token
            });
        if index > 0 && !empty_close {
            copy_line_breaks(piece.gap, &mut output);
            if output.ends_with(['\r', '\n']) {
                column = 0;
            }
        }
        let trailing_comment = piece.token.is_none() && !piece.gap.contains(['\r', '\n']);
        let attached_separator =
            token.is_some_and(|t| matches!(t.kind, TokenKind::Semi | TokenKind::Comma));
        let expanded_close = closing && frames.last().is_some_and(|frame| frame.expanded);
        let space = index > 0 && space_between(syntax, &pieces[index - 1], piece);
        let width_break = !trailing_comment
            && !attached_separator
            && wrap.break_before(index, column + usize::from(space));
        // A width break inside an inline group makes that whole group vertical.
        // Re-render from the original pieces; each retry forces a new group, so
        // this converges without reparsing or guessing a fixed iteration count.
        if width_break {
            if let Some(frame) = frames.last().filter(|frame| !frame.expanded) {
                wrap.force(frame.piece);
            }
        }
        if (expanded_close
            || (pending_break && !trailing_comment && !attached_separator)
            || width_break)
            && !output.ends_with(['\r', '\n'])
        {
            output.push_str(ending);
            column = 0;
        }
        if index == 0 || output.ends_with(['\r', '\n']) {
            let base = frames
                .last()
                .map_or(0, |frame| frame.base + usize::from(!closing));
            let continuation = !closing
                && (syntax
                    .continuation
                    .get(piece.token.unwrap_or(next_token))
                    .copied()
                    .unwrap_or(false)
                    || next_token.checked_sub(1).is_some_and(|i| {
                        syntax.roles[i] == Role::Prefix
                            || after_operator(&syntax.tokens[i].kind, syntax.roles[i])
                    }));
            line_indent = anchor.unwrap_or(base + usize::from(continuation));
            output.extend(std::iter::repeat_n(' ', line_indent * 4));
            column += line_indent * 4;
            pending_break = false;
        } else if let Some(previous) = index.checked_sub(1).map(|i| &pieces[i]) {
            if space_between(syntax, previous, piece) {
                output.push(' ');
                column += 1;
            }
        }
        output.push_str(piece.text);
        if let Some(end) = piece.text.rfind(['\r', '\n']) {
            column = piece.text[end + 1..].chars().count();
        } else {
            column += piece.text.chars().count();
        }
        let Some(i) = piece.token else { continue };
        indents[i] = line_indent;
        next_token = i + 1;
        let kind = &syntax.tokens[i].kind;
        if closing {
            frames.pop();
        } else if is_open(kind, role) {
            let expanded = wrap.expand(index, column.saturating_sub(piece.text.chars().count()))
                || (matches!(
                    role,
                    Role::BlockOpen | Role::ArmsOpen | Role::DeclarationsOpen
                ) && syntax.pairs[i].is_some_and(|close| {
                    !source[syntax.tokens[i].span.end()..syntax.tokens[close].span.start()]
                        .trim()
                        .is_empty()
                }));
            let base = if matches!(
                role,
                Role::BlockOpen | Role::ArmsOpen | Role::DeclarationsOpen
            ) {
                anchor.unwrap_or(line_indent)
            } else {
                line_indent.max(frames.last().map_or(0, |frame| frame.base + 1))
            };
            frames.push(Frame {
                open: i,
                piece: index,
                role,
                base,
                expanded,
            });
            pending_break |= expanded;
        }
        pending_break |= syntax.break_after[i]
            || frames.last().is_some_and(|frame| {
                (matches!(frame.role, Role::BlockOpen | Role::DeclarationsOpen)
                    && *kind == TokenKind::Semi)
                    || (frame.expanded && *kind == TokenKind::Comma)
            });
    }
    if !pieces.is_empty() && !output.ends_with(['\r', '\n']) {
        output.push_str(ending);
    }
    output
}

fn line_ending(text: &str) -> &str {
    if text.starts_with("\r\n") {
        "\r\n"
    } else {
        &text[..1]
    }
}

fn copy_line_breaks(mut gap: &str, output: &mut String) {
    let mut count = 0;
    while let Some(index) = gap.find(['\r', '\n']) {
        if count == 2 {
            break;
        }
        let ending = line_ending(&gap[index..]);
        // Removing this separator would merge two distinct breaks into CRLF.
        if ending == "\n" && output.ends_with('\r') {
            output.push(' ');
        }
        output.push_str(ending);
        count += 1;
        gap = &gap[index + ending.len()..];
    }
}

pub(super) fn binary(kind: &TokenKind, role: Role) -> bool {
    role == Role::Normal
        && matches!(
            kind,
            TokenKind::Eq
                | TokenKind::EqEq
                | TokenKind::Ne
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge
                | TokenKind::Shl
                | TokenKind::Shr
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
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

pub(super) fn space_between(syntax: &Syntax, left: &Piece<'_>, right: &Piece<'_>) -> bool {
    let (Some(left), Some(right)) = (left.token, right.token) else {
        return true;
    };
    use TokenKind::*;
    let (a, b) = (&syntax.tokens[left].kind, &syntax.tokens[right].kind);
    let (ar, br) = (syntax.roles[left], syntax.roles[right]);
    // Do not merge borrows into &&, or integer member access into a float.
    if matches!((a, b), (Amp, Amp) | (IntLit(_), Dot) | (Minus, Minus)) {
        return true;
    }
    if ar == Role::ImportSlash
        || br == Role::ImportSlash
        || ar == Role::ImportOpen
        || br == Role::ImportClose
        || ar == Role::TypeOpen
        || br == Role::TypeClose
        || ar == Role::ClosureOpen
        || br == Role::ClosureClose
    {
        return false;
    }
    if br == Role::TypeOpen {
        return !matches!(a, Ident(_) | Impl | ColonColon)
            && !matches!(ar, Role::TypeClose | Role::Prefix);
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
    if binary(a, ar) || binary(b, br) {
        return true;
    }
    if ar == Role::Prefix || (matches!(b, Bang) && br != Role::Prefix) {
        return false;
    }
    if matches!(b, LParen | LBracket) {
        return matches!(
            a,
            If | While | Match | Return | In | Let | Var | For | Colon | Comma | Semi
        ) || matches!(a, Ident(name) if name == "when")
            || matches!((a, b), (StrLit(_), LParen));
    }
    true
}
