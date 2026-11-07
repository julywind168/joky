//! Documentation is extracted from parsed declarations and lexer-established trivia.
//! It never changes the executable AST or its serialized representation.
use super::{ast::*, Diagnostic, Lexer, Token, TokenKind};
use crate::Span;

mod highlight;
pub use highlight::syntax_spans;

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub kind: &'static str,
    pub signature: String,
    pub docs: String,
    pub children: Vec<Entry>,
}
#[derive(Debug)]
pub struct Documentation {
    pub docs: String,
    pub entries: Vec<Entry>,
    pub imports: Vec<String>,
}

/// Parse documentation with the same syntax as compilation. Embedded source
/// modules expose unmarked declarations too, unlike ordinary user modules.
pub fn extract(source: &str, embedded: bool) -> Result<Documentation, Diagnostic> {
    let module = super::parse(source)?;
    let tokens = Lexer::tokenize(source)?;
    let comments = comments(source, &tokens);
    let item = |name: &str, kind, span: Span, body: Option<usize>| {
        let span = with_prefix(&tokens, span);
        Entry {
            name: name.into(),
            kind,
            signature: signature(source, &tokens, span, body),
            docs: attached(source, &comments, span.start()),
            children: vec![],
        }
    };
    let visible = |name: &str, public: bool| !name.starts_with('_') && (embedded || public);
    let mut entries = Vec::new();
    for f in &module.items {
        if let Item::FnDef(f) = f {
            if visible(&f.name, f.public) && !f.is_test() {
                entries.push(item(&f.name, "fn", f.span, Some(f.body.span().start())));
            }
        }
    }
    for t in &module.type_aliases {
        if visible(&t.name, t.public) {
            entries.push(item(&t.name, "type", t.span, None));
        }
    }
    for c in &module.constants {
        if visible(&c.name, c.public) {
            entries.push(item(&c.name, "const", c.span, None));
        }
    }
    for r in &module.records {
        if visible(&r.name, r.public) {
            let mut e = item(
                &r.name,
                "struct",
                with_attributes(r.span, &r.attributes),
                brace(&tokens, r.span),
            );
            for f in &r.fields {
                if r.public_fields.contains(&f.name) {
                    e.children.push(item(&f.name, "field", f.span, None));
                }
            }
            entries.push(e);
        }
    }
    for e in &module.enums {
        if visible(&e.name, e.public) {
            let mut entry = item(
                &e.name,
                "enum",
                with_attributes(e.span, &e.attributes),
                brace(&tokens, e.span),
            );
            for v in &e.variants {
                entry.children.push(item(&v.name, "variant", v.span, None));
            }
            entries.push(entry);
        }
    }
    for t in &module.traits {
        if visible(&t.name, t.public) {
            let mut e = item(&t.name, "trait", t.span, brace(&tokens, t.span));
            for a in &t.associated_types {
                e.children.push(item(
                    &a.name,
                    "type",
                    associated_span(&tokens, a.span, None),
                    None,
                ));
            }
            for f in &t.methods {
                e.children
                    .push(item(&f.name, "method", f.signature_span, None));
            }
            entries.push(e);
        }
    }
    for i in &module.implementations {
        // A private local type does not acquire a public documentation page
        // merely because one of its methods implements a public trait.
        if module
            .records
            .iter()
            .any(|r| r.name == i.target_name && !visible(&r.name, r.public))
            || module
                .enums
                .iter()
                .any(|e| e.name == i.target_name && !visible(&e.name, e.public))
        {
            continue;
        }
        let mut e = item(&i.target_name, "impl", i.span, brace(&tokens, i.span));
        for f in &i.methods {
            if visible(&f.name, f.public) || i.trait_name.is_some() {
                e.children
                    .push(item(&f.name, "method", f.span, Some(f.body.span().start())));
            }
        }
        for c in &i.constants {
            if visible(&c.name, c.public) {
                e.children.push(item(&c.name, "const", c.span, None));
            }
        }
        for a in &i.associated_types {
            e.children.push(item(
                &a.name,
                "type",
                associated_span(&tokens, a.span, Some(a.target_span.end())),
                None,
            ));
        }
        if !e.children.is_empty() {
            // A type may spread one impl signature across multiple source blocks.
            // Present it once so method links do not depend on block ordering.
            if let Some(existing) = entries
                .iter_mut()
                .find(|entry| entry.kind == "impl" && entry.signature == e.signature)
            {
                existing.children.extend(e.children);
                if !e.docs.is_empty() {
                    if !existing.docs.is_empty() {
                        existing.docs.push_str("\n\n");
                    }
                    existing.docs.push_str(&e.docs);
                }
            } else {
                entries.push(e);
            }
        }
    }
    Ok(Documentation {
        docs: comments
            .iter()
            .filter(|c| c.module && c.start < tokens[0].span.start())
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        entries,
        imports: module
            .imports
            .iter()
            .filter(|i| {
                !i.path.contains("/internal/")
                    && i.span
                        .slice(source)
                        .is_some_and(|s| s.starts_with("import"))
            })
            .map(|i| i.span.slice(source).unwrap_or("").trim().to_owned())
            .collect(),
    })
}
// Render imported declarations with the public facade's selected names.
pub(crate) fn rename_entry(
    entry: &mut Entry,
    renames: &std::collections::BTreeMap<&str, &str>,
) -> Result<(), Diagnostic> {
    if renames.is_empty() {
        return Ok(());
    }
    if let Some(name) = renames.get(entry.name.as_str()) {
        entry.name = (*name).into();
    }
    // Token boundaries avoid changing longer identifiers or string constants.
    for token in Lexer::tokenize(&entry.signature)?.into_iter().rev() {
        if let TokenKind::Ident(name) = token.kind {
            if let Some(alias) = renames.get(name.as_str()) {
                entry
                    .signature
                    .replace_range(token.span.start()..token.span.end(), alias);
            }
        }
    }
    for child in &mut entry.children {
        rename_entry(child, renames)?;
    }
    Ok(())
}

fn with_prefix(tokens: &[Token], span: Span) -> Span {
    let start = tokens
        .iter()
        .rev()
        .find(|t| t.span.end() <= span.start())
        .filter(|t| t.kind == TokenKind::Pub)
        .map_or(span.start(), |t| t.span.start());
    Span::new(start, span.end())
}
fn with_attributes(span: Span, attributes: &[Attribute]) -> Span {
    Span::new(
        attributes.first().map_or(span.start(), |a| a.span.start()),
        span.end(),
    )
}
fn associated_span(tokens: &[Token], name: Span, end: Option<usize>) -> Span {
    let index = tokens.partition_point(|t| t.span.start() < name.start());
    // Associated-type AST spans identify the name for diagnostics. Recover the
    // declaration extent from its grammar, retaining written bounds and aliases.
    let start = tokens[index - 1].span.start();
    let end = end.unwrap_or_else(|| {
        tokens[index + 1..]
            .iter()
            .find(|t| {
                matches!(
                    t.kind,
                    TokenKind::Semi | TokenKind::Fn | TokenKind::Type | TokenKind::RBrace
                )
            })
            .map_or(name.end(), |t| t.span.start())
    });
    Span::new(start, end)
}
fn brace(tokens: &[Token], span: Span) -> Option<usize> {
    tokens
        .iter()
        .find(|t| {
            t.span.start() >= span.start()
                && t.span.end() <= span.end()
                && t.kind == TokenKind::LBrace
        })
        .map(|t| t.span.start())
}
fn signature(source: &str, tokens: &[Token], span: Span, end: Option<usize>) -> String {
    let end = end.filter(|end| *end > span.start()).unwrap_or(span.end());
    // Intrinsic binding attributes are compiler metadata, not call syntax.
    let mut start = span.start();
    let declaration: Vec<_> = tokens
        .iter()
        .filter(|t| t.span.start() >= start && t.span.end() <= end)
        .collect();
    if declaration
        .first()
        .is_some_and(|t| t.kind == TokenKind::Hash)
        && declaration
            .get(1)
            .is_some_and(|t| t.span.slice(source) == Some("intrinsic"))
    {
        if let Some(t) = declaration
            .iter()
            .find(|t| matches!(t.kind, TokenKind::Pub | TokenKind::Fn))
        {
            start = t.span.start();
        }
    }
    let mut out = String::new();
    let mut previous = "";
    for token in tokens
        .iter()
        .filter(|t| t.span.start() >= start && t.span.end() <= end && t.span.start() < end)
    {
        let text = token.span.slice(source).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let tight = matches!(
            text,
            "," | ")" | "]" | ">" | ":" | "." | "::" | "(" | "<" | ";"
        ) || matches!(previous, "(" | "[" | "<" | "." | "::" | "&" | "@" | "#");
        if !out.is_empty() && !tight {
            out.push(' ');
        }
        out.push_str(text);
        previous = text;
    }
    out.trim_end_matches(';').to_owned()
}
struct Comment {
    start: usize,
    end: usize,
    text: String,
    module: bool,
}
fn comments(source: &str, tokens: &[Token]) -> Vec<Comment> {
    let mut result = Vec::new();
    let mut previous = 0;
    for token in tokens {
        let mut at = previous;
        let end = token.span.start();
        while at < end {
            if source[at..].starts_with("/*") {
                let mut depth = 1;
                at += 2;
                while at < end && depth > 0 {
                    if source[at..].starts_with("/*") {
                        depth += 1;
                        at += 2;
                    } else if source[at..].starts_with("*/") {
                        depth -= 1;
                        at += 2;
                    } else {
                        at += source[at..].chars().next().unwrap().len_utf8();
                    }
                }
            } else if source[at..].starts_with("//") {
                let stop = source[at..end].find(['\r', '\n']).map_or(end, |n| at + n);
                let line = &source[at..stop];
                if line.starts_with("//!") || line.starts_with("///") && !line.starts_with("////") {
                    let text = line[3..].strip_prefix(' ').unwrap_or(&line[3..]).to_owned();
                    result.push(Comment {
                        start: at,
                        end: stop,
                        text,
                        module: line.starts_with("//!"),
                    });
                }
                at = stop;
            } else {
                at += source[at..].chars().next().unwrap().len_utf8();
            }
        }
        previous = token.span.end();
    }
    result
}
fn attached(source: &str, comments: &[Comment], start: usize) -> String {
    let mut end = start;
    let mut lines = Vec::new();
    for c in comments.iter().rev().filter(|c| c.end <= start) {
        if c.module || !source[c.end..end].trim().is_empty() {
            break;
        }
        lines.push(c.text.as_str());
        end = c.start;
    }
    lines.reverse();
    lines.join("\n")
}

#[cfg(test)]
mod tests;
