//! Lexer-backed spans with categories matching the VS Code TextMate grammar.
use super::super::{lexer::InterpolationPart, Diagnostic, Lexer, Token, TokenKind};
use crate::Span;

/// Classify a Joky fragment without parsing declarations or resolving names.
/// Returned spans are ordered, disjoint byte ranges into the original source.
/// Invalid lexical fragments return an error so renderers can show plain text.
pub fn syntax_spans(source: &str) -> Result<Vec<(Span, &'static str)>, Diagnostic> {
    let tokens = Lexer::tokenize(source)?;
    let mut spans = Vec::new();
    highlight(source, &tokens, 0, &mut spans);
    Ok(spans)
}

fn highlight(
    source: &str,
    tokens: &[Token],
    mut cursor: usize,
    spans: &mut Vec<(Span, &'static str)>,
) {
    use TokenKind::*;
    for (index, token) in tokens.iter().enumerate() {
        // The lexer omits trivia; any non-whitespace gap is a comment. Keeping
        // the gap intact also preserves nested block comments and line endings.
        if !source[cursor..token.span.start()].trim().is_empty() {
            spans.push((Span::new(cursor, token.span.start()), "comment"));
        }
        cursor = token.span.end();
        if let Interpolated(parts) | SqlTemplate(parts) = &token.kind {
            let mut start = token.span.start();
            let mut text_end = start;
            for part in parts {
                match part {
                    InterpolationPart::Text(_, span) => text_end = span.end(),
                    InterpolationPart::Expression(inner) | InterpolationPart::Parameter(inner) => {
                        // Text ends at the opening brace, and the nested EOF
                        // token carries the closing brace's original position.
                        let content_start = text_end + 1;
                        let open =
                            text_end - usize::from(matches!(part, InterpolationPart::Parameter(_)));
                        if start < open {
                            spans.push((Span::new(start, open), "string"));
                        }
                        spans.push((Span::new(open, content_start), "interpolation"));
                        highlight(source, inner, content_start, spans);
                        let close = inner.last().unwrap().span;
                        spans.push((close, "interpolation"));
                        start = close.end();
                    }
                }
            }
            spans.push((Span::new(start, token.span.end()), "string"));
            continue;
        }
        let previous = index.checked_sub(1).map(|i| &tokens[i].kind);
        let next = tokens.get(index + 1).map(|t| &t.kind);
        let category = match &token.kind {
            Eof => continue,
            StrLit(_) | CharLit(_) | ByteLit(_) => "string",
            IntLit(_) | FloatLit(_) | DurationLit(_) | TupleIndex(_) | True | False => "constant",
            At | Hash => "annotation",
            Ident(name) => {
                if matches!(previous, Some(At | Hash)) {
                    "annotation"
                } else if matches!(previous, Some(Fn)) {
                    "function"
                } else if matches!(previous, Some(Struct | Enum | Trait | Type)) || name == "Self" {
                    "type"
                } else if previous != Some(&Dot)
                    && (matches!(name.as_str(), "join" | "isolate" | "aliased")
                        && next == Some(&LBrace)
                        || name == "when"
                            && (matches!(next, Some(StrLit(_)))
                                || when_block(&tokens[index + 1..])))
                {
                    "control"
                } else if name == "as" {
                    "keyword"
                } else if next == Some(&LParen)
                    || next == Some(&ColonColon)
                        && tokens.get(index + 2).is_some_and(|t| t.kind == Lt)
                {
                    "function"
                } else if name.starts_with(|c: char| c.is_uppercase()) {
                    "type"
                } else {
                    "variable"
                }
            }
            TyFloat32 | TyFloat64 | TyInt32 | TyInt8 | TyInt16 | TyInt64 | TyUInt8 | TyUInt16
            | TyUInt32 | TyRune | TyUInt64 | TyString | TyPtr | TyBool | TyUnit => "type",
            If | Else | Match | While | For | In | Return | Break | Continue | Scope | Race
            | Branch => "control",
            Fn | Move | Pub | Extern | Unsafe | Import | Type | Struct | Enum | Impl | Trait
            | Dyn | Const | Let | Var | Mut => "keyword",
            Parallel => "deprecated",
            LBracket | RBracket | LParen | RParen | LBrace | RBrace | Comma | Dot | Colon
            | Semi => continue,
            _ => "operator",
        };
        spans.push((token.span, category));
    }
}

// `when (...) {}` is contextual; an ordinary `when(...)` call remains a call.
fn when_block(tokens: &[Token]) -> bool {
    if tokens.first().is_none_or(|t| t.kind != TokenKind::LParen) {
        return false;
    }
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return tokens
                        .get(index + 1)
                        .is_some_and(|t| t.kind == TokenKind::LBrace);
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_highlighting_matches_editor_categories_and_source_boundaries() {
        let source = "pub fn 计算<T>(value: UInt64) @suspends -> Bool {\n/* outer /* nested */ comment */\nlet raw = r#\"// <raw>\"#; task.sleep(1.5s); true }";
        let spans = syntax_spans(source).unwrap();
        for (text, class) in [
            ("pub", "keyword"),
            ("计算", "function"),
            ("UInt64", "type"),
            ("suspends", "annotation"),
            ("1.5s", "constant"),
            ("true", "constant"),
            ("r#\"// <raw>\"#", "string"),
        ] {
            assert!(
                spans
                    .iter()
                    .any(|(span, kind)| *kind == class && span.slice(source) == Some(text)),
                "{text}"
            );
        }
        assert!(spans.iter().any(|(span, kind)| *kind == "comment"
            && span.slice(source).unwrap().contains("/* nested */")));
        assert!(spans.windows(2).all(|w| w[0].0.end() <= w[1].0.start()));
        assert!(syntax_spans("\"unterminated").is_err());
    }

    #[test]
    fn documentation_highlighting_restores_code_inside_string_and_sql_interpolation() {
        let source = "let s = \"hi { /* note */ if true { \"inner {value}\" } else { \"\" } }!\"\nlet q = sql\"SELECT ${value + 1}, {name}\"\njoin { task.join(); isolate {} }";
        let spans = syntax_spans(source).unwrap();
        assert!(spans.windows(2).all(|w| w[0].0.end() <= w[1].0.start()));
        for (text, class) in [
            ("if", "control"),
            ("value", "variable"),
            ("1", "constant"),
            ("join", "control"),
            ("join", "function"),
            ("isolate", "control"),
            ("${", "interpolation"),
        ] {
            assert!(
                spans
                    .iter()
                    .any(|(span, kind)| *kind == class && span.slice(source) == Some(text)),
                "{text}: {class}"
            );
        }
    }
}
