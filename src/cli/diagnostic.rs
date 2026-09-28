//! Terminal rendering for source diagnostics.
use ariadne::{sources, Config, IndexType, Label, Report, ReportKind};
use joky::{Error, ErrorCode, Span};
use std::path::Path;

/// Render a structured compiler, resolver or runtime error with its source
/// context. Machine-readable error codes appear in the report header; errors
/// without a source span retain their stable text representation.
pub(super) fn render_error(path: &Path, source: &str, error: &Error) -> String {
    let Some(span) = error.span() else {
        return error.to_string();
    };
    let (path, source) = error
        .sources()
        .get(span.source_id())
        .map_or((path, source), |file| (Path::new(file.name()), file.text()));
    let code = error.code();
    let code = (code != ErrorCode::Generic).then(|| code.as_str());
    let rendered =
        render_span(path, source, span, error.message(), code).unwrap_or_else(|| error.to_string());
    let header = error.line().map_or_else(
        || error.message().to_owned(),
        |line| format!("Line {line}: {}", error.message()),
    );
    let mut output = format!("{header}\n{rendered}");
    for frame in error.runtime_trace() {
        use std::fmt::Write;
        let (path, source) = error
            .sources()
            .get(frame.span.source_id())
            .map_or((path, source), |file| (Path::new(file.name()), file.text()));
        let offset = frame.span.start().min(source.len());
        let line = source.as_bytes()[..offset]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1;
        let _ = write!(
            output,
            "\n  at {} ({}:{line}, pc {})",
            frame.function,
            path.display(),
            frame.pc
        );
    }
    output
}

fn render_span(
    path: &Path,
    source: &str,
    source_span: Span,
    message: &str,
    code: Option<&str>,
) -> Option<String> {
    let span = normalize_span(source, source_span);
    let file = path.display().to_string();
    let mut output = Vec::new();
    let mut report = Report::build(ReportKind::Error, (file.clone(), span.clone()))
        .with_message(message.to_owned());
    if let Some(code) = code {
        report = report.with_code(code);
    }
    let report = report
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

fn normalize_span(source: &str, span: Span) -> std::ops::Range<usize> {
    let len = source.len();
    let mut start = span.start().min(len);
    let mut end = span.end().min(len);
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
