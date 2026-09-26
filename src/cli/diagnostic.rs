//! Terminal rendering for source diagnostics.
use ariadne::{sources, Config, IndexType, Label, Report, ReportKind};
use joky::{parser::Diagnostic, Error};
use std::path::Path;

/// Render a parser diagnostic while keeping the stable textual line header
/// used by existing CLI consumers.
pub(super) fn render(path: &Path, source: &str, diagnostic: &Diagnostic) -> String {
    let rendered = render_span(path, source, diagnostic.span.clone(), &diagnostic.message)
        .unwrap_or_else(|| diagnostic.to_string());
    format!(
        "Line {}: {}\n{rendered}",
        diagnostic.line, diagnostic.message
    )
}

/// Render a structured compiler or resolver error when it carries a source
/// span; unlocated errors retain their stable text representation.
pub(super) fn render_error(path: &Path, source: &str, error: &Error) -> String {
    let Some(span) = error.span() else {
        return error.to_string();
    };
    let rendered =
        render_span(path, source, span, error.message()).unwrap_or_else(|| error.to_string());
    let header = error.line().map_or_else(
        || error.message().to_owned(),
        |line| format!("Line {line}: {}", error.message()),
    );
    format!("{header}\n{rendered}")
}

fn render_span(
    path: &Path,
    source: &str,
    source_span: std::ops::Range<usize>,
    message: &str,
) -> Option<String> {
    let span = normalize_span(source, &source_span);
    let file = path.display().to_string();
    let mut output = Vec::new();
    let report = Report::build(ReportKind::Error, (file.clone(), span.clone()))
        .with_message(message.to_owned())
        .with_label(Label::new((file.clone(), span)).with_message(message.to_owned()))
        // Lexer spans are UTF-8 byte offsets. Ariadne defaults to character
        // offsets, which would misplace labels after a non-ASCII character.
        .with_config(Config::default().with_index_type(IndexType::Byte))
        .finish();
    report
        .write(sources([(file, source)]), &mut output)
        .ok()
        .and_then(|_| String::from_utf8(output).ok())
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
