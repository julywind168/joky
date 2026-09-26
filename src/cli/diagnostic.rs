//! Terminal rendering for source diagnostics.
use ariadne::{sources, Config, IndexType, Label, Report, ReportKind};
use joky::parser::Diagnostic;
use std::path::Path;

/// Render a parser diagnostic while keeping the stable textual line header
/// used by existing CLI consumers.
pub(super) fn render(path: &Path, source: &str, diagnostic: &Diagnostic) -> String {
    let span = normalize_span(source, &diagnostic.span);
    let file = path.display().to_string();
    let mut output = Vec::new();
    let report = Report::build(ReportKind::Error, (file.clone(), span.clone()))
        .with_message(diagnostic.message.clone())
        .with_label(Label::new((file.clone(), span)).with_message(diagnostic.message.clone()))
        // Lexer spans are UTF-8 byte offsets. Ariadne defaults to character
        // offsets, which would misplace labels after a non-ASCII character.
        .with_config(Config::default().with_index_type(IndexType::Byte))
        .finish();
    let rendered = report
        .write(sources([(file, source)]), &mut output)
        .ok()
        .and_then(|_| String::from_utf8(output).ok())
        .unwrap_or_else(|| diagnostic.to_string());
    format!(
        "Line {}: {}\n{rendered}",
        diagnostic.line, diagnostic.message
    )
}

fn normalize_span(source: &str, span: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    let len = source.len();
    let mut start = span.start.min(len);
    let mut end = span.end.min(len);
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    if end < start {
        end = start;
    }
    if start == end && start < len {
        end = source[start..]
            .chars()
            .next()
            .map_or(start, |ch| start + ch.len_utf8());
    }
    start..end
}
