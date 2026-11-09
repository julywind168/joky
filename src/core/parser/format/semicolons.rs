//! Only block-level separators with a syntax-only implicit-discard proof are
//! rewritten. The ordinary formatter still verifies the resulting token stream
//! and parse shape strictly; trailing and value-context semicolons stay intact.
use super::syntax::Syntax;
use std::{collections::BTreeMap, ops::Range};

pub(super) fn normalize(source: &str, syntax: &Syntax, skipped: &[Range<usize>]) -> Option<String> {
    let protected = |position| {
        let i = skipped.partition_point(|range| range.end <= position);
        skipped
            .get(i)
            .is_some_and(|range| range.contains(&position))
    };
    let ending = source.find(['\r', '\n']).map_or("\n", |i| {
        if source[i..].starts_with("\r\n") {
            "\r\n"
        } else if source[i..].starts_with('\r') {
            "\r"
        } else {
            "\n"
        }
    });
    let mut edits = BTreeMap::new();
    for separator in &syntax.separators {
        let tokens = &syntax.tokens[separator.tokens.clone()];
        let next = &syntax.tokens[separator.next];
        // Deleting a lone semicolon on a directive-looking comment's line must
        // not turn an ordinary inline comment into an active fmt directive.
        if source[tokens[0].span.start()..next.span.start()].contains("// fmt:") {
            continue;
        }
        if protected(next.span.start()) || tokens.iter().any(|token| protected(token.span.start()))
        {
            continue;
        }
        for token in tokens {
            edits.insert(token.span.start(), (token.span.end(), ""));
        }
        // Put the boundary before the next statement, after any trailing
        // comments, rather than detaching a comment from the preceding call.
        let previous = &syntax.tokens[separator.tokens.start - 1];
        if next.line == previous.end_line {
            edits.insert(next.span.start(), (next.span.start(), ending));
        }
    }
    if edits.is_empty() {
        return None;
    }
    let mut output = String::with_capacity(source.len());
    let mut end = 0;
    for (start, (next, replacement)) in edits {
        output.push_str(&source[end..start]);
        output.push_str(replacement);
        end = next;
    }
    output.push_str(&source[end..]);
    Some(output)
}
