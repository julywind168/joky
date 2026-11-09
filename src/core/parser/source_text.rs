//! Source characters whose visual order or line breaks can obscure token boundaries.
use super::Diagnostic;
use crate::Span;
use std::{borrow::Cow, fmt::Write};

fn bidi_control(ch: char) -> bool {
    // Unicode Bidi_Control, including the directional marks and isolates.
    matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub(super) fn ambiguous_line_break(ch: char) -> bool {
    matches!(
        ch,
        '\u{000b}' | '\u{000c}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

pub(super) fn check_bidi(source: &str) -> Result<(), Diagnostic> {
    if let Some((at, ch)) = source.char_indices().find(|&(_, ch)| bidi_control(ch)) {
        let span = Span::new(at, at + ch.len_utf8());
        let (line, _) = span.line_column(source);
        return Err(Diagnostic::at(line, span, format!(
            "Literal bidirectional control U+{0:04X} in source. Remove it or use '\\u{{{0:X}}}' in an escaped literal.", ch as u32
        )));
    }
    Ok(())
}

/// Make source controls visible when displaying diagnostics, including excerpts
/// from otherwise valid literals. Ordinary text can be displayed unchanged.
pub fn escape_diagnostic_text(text: &str) -> Cow<'_, str> {
    let needs_escape = |ch| bidi_control(ch) || ambiguous_line_break(ch);
    let Some(first) = text.find(needs_escape) else {
        return Cow::Borrowed(text);
    };
    let mut out = String::from(&text[..first]);
    for ch in text[first..].chars() {
        if needs_escape(ch) {
            write!(out, "\\u{{{:X}}}", ch as u32).unwrap();
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use crate::parser::{format, parse_with_source};
    use crate::SourceFile;

    fn rejects(source: &str, ch: char) {
        let file = SourceFile::new("example.jk", source);
        let parsed = parse_with_source(source, file.id()).unwrap_err();
        let formatted = format(source).unwrap_err();
        assert_eq!(parsed.span.source_id(), file.id());
        assert_eq!(parsed.span.range(), formatted.span.range());
        assert_eq!(parsed.span.slice(source), Some(ch.to_string().as_str()));
        assert_eq!(parsed.message, formatted.message);
        assert!(parsed.message.contains(&format!("U+{:04X}", ch as u32)));
        assert_eq!(parsed.line, parsed.span.line_column(source).0);
    }

    #[test]
    fn bidi_controls_are_rejected_verbatim_in_all_source_contexts() {
        for ch in ['\u{061c}', '\u{200e}', '\u{200f}']
            .into_iter()
            .chain('\u{202a}'..='\u{202e}')
            .chain('\u{2066}'..='\u{2069}')
        {
            rejects(&format!("// 注\r// text{ch}"), ch);
        }
        for text in [
            "let @x = 1",
            "/*a/*@*/b*/",
            "let s = \"@\"",
            "let s = r#\"@\"#",
            "let s = \"\"\"\n @\n \"\"\"",
            "let r = '@'",
            "let s = \"{1 /*@*/}\"",
        ] {
            rejects(
                &format!("fn f() {{ {} }}", text.replace('@', "\u{202e}")),
                '\u{202e}',
            );
        }
    }

    #[test]
    fn visual_line_separators_are_rejected_in_code_and_comment_trivia() {
        for ch in ['\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}'] {
            for text in [
                "fn@main() {}",
                "// x@fn main() {}",
                "/* /*x@*/ */",
                "fn f() { \"{1@+2}\" }",
            ] {
                rejects(&text.replace('@', &ch.to_string()), ch);
            }
            let text = format!("fn f() {{ r#\"{ch}\"# }}\n");
            assert_eq!(
                format(&text).unwrap(),
                format!("fn f() {{\n    r#\"{ch}\"#\n}}\n")
            );
        }
    }
}
