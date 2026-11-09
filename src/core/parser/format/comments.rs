//! Only the indentation before conventional block-comment stars is adjustable.
use std::borrow::Cow;

pub(super) fn reindent(text: &str, column: usize) -> Cow<'_, str> {
    if !text.starts_with("/*") || text[2..].contains("/*") {
        return Cow::Borrowed(text);
    }
    let Some(first) = text.find(['\r', '\n']) else {
        return Cow::Borrowed(text);
    };
    let mut lines = Vec::new();
    let mut rest = &text[first..];
    while !rest.is_empty() {
        let length = if rest.starts_with("\r\n") { 2 } else { 1 };
        let ending = &rest[..length];
        rest = &rest[length..];
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let line = rest[..end].trim_start_matches([' ', '\t']);
        if !(line == "*" || line.starts_with("* ") || line.starts_with("*\t") || line == "*/")
            || (end < rest.len() && line.contains("*/"))
        {
            return Cow::Borrowed(text);
        }
        lines.push((ending, line));
        rest = &rest[end..];
    }
    if lines.last().is_none_or(|(_, line)| *line != "*/") {
        return Cow::Borrowed(text);
    }
    let mut output = text[..first].to_owned();
    for (ending, line) in lines {
        output.push_str(ending);
        output.extend(std::iter::repeat_n(' ', column + 1));
        output.push_str(line);
    }
    Cow::Owned(output)
}
