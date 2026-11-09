//! Standalone comment directives preserve complete source lines verbatim.
use super::{pieces, Diagnostic, Piece};
use crate::{core::parser::lexer::Lexer, Span};
use std::ops::Range;

pub(super) fn regions(source: &str, pieces: &[Piece<'_>]) -> Result<Vec<Range<usize>>, Diagnostic> {
    let mut result = Vec::new();
    let mut open = None;
    for piece in pieces.iter().filter(|p| p.token.is_none()) {
        let text = piece.text.trim_end();
        if !matches!(text, "// fmt: off" | "// fmt: on") {
            continue;
        }
        let start = piece.end - piece.text.len();
        let line = source[..start].rfind(['\r', '\n']).map_or(0, |i| i + 1);
        if !source[line..start]
            .trim_start_matches('\u{feff}')
            .trim()
            .is_empty()
        {
            continue;
        }
        let error = |message| {
            let span = Span::from(start..piece.end);
            Diagnostic::at(span.line_column(source).0, span, message)
        };
        if text == "// fmt: off" {
            if open.is_some() {
                return Err(error(
                    "Nested fmt: off; close the current region with '// fmt: on' first",
                ));
            }
            open = Some(line);
        } else {
            let Some(begin) = open.take() else {
                return Err(error("fmt: on has no matching '// fmt: off'"));
            };
            let mut end = piece.end;
            if source[end..].starts_with("\r\n") {
                end += 2
            } else if source[end..].starts_with(['\r', '\n']) {
                end += 1
            }
            result.push(begin..end);
        }
    }
    if let Some(start) = open {
        let span = Span::from(start..source.len());
        return Err(Diagnostic::at(
            span.line_column(source).0,
            span,
            "Unclosed fmt: off; add a standalone '// fmt: on'",
        ));
    }
    Ok(result)
}

pub(super) fn restore(
    source: &str,
    original: &[Range<usize>],
    output: String,
) -> Result<String, Diagnostic> {
    if original.is_empty() {
        return Ok(output);
    }
    let error = || {
        Diagnostic::at(
            1,
            0..source.len(),
            "Cannot preserve formatting-disabled regions; source left unchanged",
        )
    };
    let tokens = Lexer::tokenize(&output).map_err(|_| error())?;
    let formatted = regions(&output, &pieces(&output, &tokens)).map_err(|_| error())?;
    if original.len() != formatted.len() {
        return Err(error());
    }
    let mut restored = String::with_capacity(output.len());
    let mut end = 0;
    for (before, after) in original.iter().zip(formatted) {
        restored.push_str(&output[end..after.start]);
        restored.push_str(&source[before.clone()]);
        end = after.end;
    }
    restored.push_str(&output[end..]);
    Ok(restored)
}
