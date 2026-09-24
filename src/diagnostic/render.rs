use std::io::{self, Write};

use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};

use super::{Diagnostic, Stage};

pub fn write_diagnostic(
    diagnostic: &Diagnostic,
    filename: &str,
    source: &str,
    writer: impl Write,
) -> io::Result<()> {
    let span = diagnostic.span().map(|span| {
        let start = span.start().min(source.len());
        let end = span.end().min(source.len()).max(start);
        start..end
    });
    let report_span = span.clone().unwrap_or(0..0);
    let message = diagnostic.to_string();
    let title = span.as_ref().map_or_else(
        // MIR diagnostics can identify a module without an exact source span.
        || format!("{filename}: {message}"),
        |_| format!("{} error", stage_name(diagnostic.stage())),
    );
    let mut report = Report::build(ReportKind::Error, (filename, report_span))
        .with_config(Config::default().with_index_type(IndexType::Byte))
        .with_message(title);

    if let Some(span) = span {
        report = report.with_label(
            Label::new((filename, span))
                .with_color(Color::Red)
                .with_message(message),
        );
    }

    report
        .finish()
        .write((filename, Source::from(source)), writer)
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Lex => "lexical",
        Stage::Parse => "syntax",
        Stage::Semantic => "semantic",
        Stage::Codegen => "code generation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn strip_ansi(input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut chars = input.chars();
        while let Some(character) = chars.next() {
            if character != '\u{1b}' {
                output.push(character);
                continue;
            }
            if chars.next() != Some('[') {
                continue;
            }
            for character in chars.by_ref() {
                if character.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        output
    }

    #[test]
    fn renders_and_clamps_a_source_span() {
        let diagnostic = Diagnostic::semantic("invalid value", Span::new(4, usize::MAX));
        let mut output = Vec::new();

        write_diagnostic(&diagnostic, "example.jk", "let x", &mut output).unwrap();

        let output = strip_ansi(&String::from_utf8(output).unwrap());
        assert!(output.contains("invalid value"));
        assert!(output.contains("example.jk"));
        assert!(output.contains("let x"));
    }
}
