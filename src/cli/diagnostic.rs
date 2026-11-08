//! Terminal rendering for source diagnostics.
use ariadne::{sources, Color, Config, IndexType, Label, Report, ReportKind};
use joky::{parser::escape_diagnostic_text, Error, ErrorCode, ErrorLabelKind, Span};
use std::borrow::Cow;
use std::path::Path;

/// Render a structured compiler, resolver or runtime error with its source
/// context. Machine-readable error codes appear in the report header; errors
/// without a source span retain their stable text representation.
pub(super) fn render_error(path: &Path, source: &str, error: &Error) -> String {
    let Some(span) = diagnostic_span(error) else {
        return escape_diagnostic_text(&error.to_string()).into_owned();
    };
    let (path, source) = error
        .sources()
        .get(span.source_id())
        .map_or((path, source), |file| (Path::new(file.name()), file.text()));
    let code = error.code();
    let code = (code != ErrorCode::Generic).then(|| code.as_str());
    // Keep the complete diagnostic in the header once, including runtime value details.
    let message = if error.span() != Some(span) {
        error.message().to_owned()
    } else {
        error.line().map_or_else(
            || error.message().to_owned(),
            |line| format!("Line {line}: {}", error.message()),
        )
    };
    let mut output =
        render_span(path, source, span, &message, code, error).unwrap_or_else(|| error.to_string());
    for run in error.runtime_trace().chunk_by(|left, right| left == right) {
        use std::fmt::Write;
        let frame = &run[0];
        let (path, source) = error
            .sources()
            .get(frame.span.source_id())
            .map_or((path, source), |file| (Path::new(file.name()), file.text()));
        let (line, _) = frame.span.line_column(source);
        let _ = write!(
            output,
            "\n  at {} ({}:{line}, pc {})",
            frame.function,
            path.display(),
            frame.pc
        );
        if run.len() > 1 {
            let _ = write!(output, " (repeated {} times)", run.len());
        }
    }
    for span in error.native_call_sites() {
        use std::fmt::Write;
        let (path, source) = error
            .sources()
            .get(span.source_id())
            .map_or((path, source), |file| (Path::new(file.name()), file.text()));
        let (line, _) = span.line_column(source);
        let _ = write!(output, "\n  called at {}:{line}", path.display());
    }
    if let Cow::Owned(escaped) = escape_diagnostic_text(&output) {
        return escaped;
    }
    output
}

// Comparison assertions are ordinary library calls. Highlight their invocation
// while preserving the original error and complete trace for embedding/debugging.
fn diagnostic_span(error: &Error) -> Option<Span> {
    let span = error.span()?;
    let Some(file) = error.sources().get(span.source_id()) else {
        return Some(span);
    };
    if file.name() != "joky/assertions.jk"
        || error.code() != ErrorCode::RuntimeError
        || !span
            .slice(file.text())
            .is_some_and(|text| text.starts_with("check.panic("))
    {
        return Some(span);
    }
    error
        .native_call_sites()
        .iter()
        .copied()
        .chain(error.runtime_trace().iter().map(|frame| frame.span))
        .find(|caller| caller.source_id() != span.source_id())
        .or(Some(span))
}

fn render_span(
    path: &Path,
    source: &str,
    source_span: Span,
    message: &str,
    code: Option<&str>,
    error: &Error,
) -> Option<String> {
    let (escaped, location, span) = display_span(source, source_span)?;
    let file = path.display().to_string();
    let mut output = Vec::new();
    let mut cache = std::collections::BTreeMap::from([(file.clone(), escaped.into_owned())]);
    let mut report = Report::build(ReportKind::Error, (file.clone(), location..location))
        .with_message(message.to_owned());
    if let Some(code) = code {
        report = report.with_code(code);
    }
    if !error
        .labels()
        .iter()
        .any(|label| label.kind == ErrorLabelKind::Primary && label.span == source_span)
    {
        report = report.with_label(Label::new((file, span)).with_color(Color::Red));
    }
    for label in error.labels() {
        let (label_path, label_source) =
            if let Some(file) = error.sources().get(label.span.source_id()) {
                (file.name().to_owned(), file.text())
            } else if label.span.source_id() == source_span.source_id() {
                (path.display().to_string(), source)
            } else {
                continue;
            };
        let (escaped, _, span) = display_span(label_source, label.span)?;
        cache.insert(label_path.clone(), escaped.into_owned());
        let color = match label.kind {
            ErrorLabelKind::Primary => Color::Red,
            ErrorLabelKind::Secondary => Color::Yellow,
            ErrorLabelKind::Context => Color::Cyan,
        };
        report = report.with_label(
            Label::new((label_path, span))
                .with_message(label.message.clone())
                .with_color(color),
        );
    }
    report
        .with_config(Config::default().with_index_type(IndexType::Byte))
        .finish()
        .write(sources(cache), &mut output)
        .ok()?;
    let mut rendered = String::from_utf8(output).ok()?;
    for help in error.help() {
        use std::fmt::Write;
        let _ = writeln!(rendered, "  help: {help}");
    }
    Some(rendered)
}

fn display_span(
    source: &str,
    source_span: Span,
) -> Option<(Cow<'_, str>, usize, std::ops::Range<usize>)> {
    let span = normalize_span(source, source_span);
    let display = source.strip_prefix('\u{feff}').unwrap_or(source);
    let skipped = source.len() - display.len();
    let span = span.start.saturating_sub(skipped)..span.end.saturating_sub(skipped);
    let escaped = escape_diagnostic_text(display);
    // Ariadne takes separate offsets for the file location and the underline.
    // Keep the original scalar column in the header while highlighting the
    // entire visible escape in the rendered line.
    let (location, span) = if let Cow::Owned(_) = &escaped {
        let prefix = escape_diagnostic_text(&display[..span.start]);
        let start = prefix.len();
        let end = start + escape_diagnostic_text(&display[span.clone()]).len();
        let column = display[..span.start]
            .rsplit(['\r', '\n'])
            .next()?
            .chars()
            .count();
        let line = prefix.rsplit(['\r', '\n']).next()?;
        let location = start - line.len()
            + line
                .char_indices()
                .nth(column)
                .map_or(line.len(), |(i, _)| i);
        (location, start..end)
    } else {
        (span.start, span)
    };
    Some((escaped, location, span))
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
