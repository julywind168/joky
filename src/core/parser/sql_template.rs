//! SQL templates retain holes until lexical SQL boundaries have been checked.
use super::*;
use lexer::InterpolationPart;

impl Parser<'_> {
    pub(super) fn parse_sql_template(
        &mut self,
        parts: Vec<InterpolationPart>,
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        // A NUL sentinel represents a hole. Actual SQL text cannot contain NUL.
        let mut text = String::new();
        let mut locations = Vec::new();
        let mut parameters = Vec::new();
        for part in &parts {
            let start = text.len();
            if matches!(part, InterpolationPart::Parameter(_)) {
                parameters.push(start);
            }
            let location = match part {
                InterpolationPart::Text(value, location) => {
                    if value.contains('\0') {
                        return Err(Diagnostic::at(
                            self.text[..location.start()]
                                .bytes()
                                .filter(|b| *b == b'\n')
                                .count()
                                + 1,
                            location.with_source(self.source),
                            "SQL text cannot contain NUL",
                        ));
                    }
                    text.push_str(value);
                    *location
                }
                InterpolationPart::Expression(tokens) | InterpolationPart::Parameter(tokens) => {
                    text.push('\0');
                    tokens.first().map_or(span, |token| token.span)
                }
            };
            locations.push((start, location));
        }
        if let Err((offset, message)) = validate(&text, &parameters) {
            let location = locations
                .iter()
                .rev()
                .find(|(start, _)| *start <= offset)
                .map_or(span, |(_, location)| *location)
                .with_source(self.source);
            let line = self.text[..location.start()]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            return Err(Diagnostic::at(line, location, message));
        }
        self.parse_template_parts(parts, span, true)
    }
}

fn validate(text: &str, parameters: &[usize]) -> Result<(), (usize, &'static str)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'\'' | b'"' => {
                let quote = bytes[i];
                let escaped = quote == b'\''
                    && i > 0
                    && matches!(bytes[i - 1], b'e' | b'E')
                    && (i == 1 || !token_continuation(bytes[i - 2]));
                i += 1;
                loop {
                    if i == bytes.len() {
                        return Err((start, "Unterminated SQL quoted text"));
                    }
                    hole_in_text(bytes, i)?;
                    if bytes[i] == quote {
                        i += 1;
                        if bytes.get(i) != Some(&quote) {
                            break;
                        }
                    } else if escaped && bytes[i] == b'\\' {
                        i += 1;
                        if i == bytes.len() {
                            return Err((start, "Unterminated SQL escape string"));
                        }
                        hole_in_text(bytes, i)?;
                    }
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
                    hole_in_text(bytes, i)?;
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1usize;
                while depth > 0 {
                    if i == bytes.len() {
                        return Err((start, "Unterminated SQL block comment"));
                    }
                    hole_in_text(bytes, i)?;
                    if bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            0 if parameters.binary_search(&i).is_ok() => {
                if i > 0 && token_continuation(bytes[i - 1])
                    || bytes.get(i + 1).is_some_and(|b| token_continuation(*b))
                {
                    return Err((
                        i,
                        "SQL parameter interpolation must occupy a separate SQL token",
                    ));
                }
                i += 1;
            }
            b'$' if i == 0 || !token_continuation(bytes[i - 1]) => {
                if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    return Err((
                        i,
                        "SQL templates cannot contain manual $N parameters; use ${expression}",
                    ));
                }
                let mut end = i + 1;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric()
                        || bytes[end] == b'_'
                        || bytes[end] >= 128)
                {
                    end += 1;
                }
                if bytes.get(end) == Some(&b'$') {
                    let delimiter = &bytes[i..=end];
                    i = end + 1;
                    loop {
                        if i == bytes.len() {
                            return Err((start, "Unterminated SQL dollar-quoted text"));
                        }
                        hole_in_text(bytes, i)?;
                        if bytes[i..].starts_with(delimiter) {
                            i += delimiter.len();
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

fn token_continuation(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | 0) || byte >= 128
}

fn hole_in_text(bytes: &[u8], at: usize) -> Result<(), (usize, &'static str)> {
    if bytes[at] == 0 {
        Err((at, "SQL interpolation is not allowed inside quotes or comments; put parameters outside quotes"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_templates_validate_postgres_lexical_boundaries() {
        for template in [
            r#"sql"SELECT ${1}, {name}, '$1', $$literal$$, $tag$literal$tag$""#,
            r#"sql"SELECT 'it''s a quote', E'it\\'s escaped', \"a\"\"b\"""#,
            r#"sql"SELECT /* outer /* inner */ end */ ${1} -- $2""#,
            r#"sql"SELECT '{{}}'::jsonb, ${ { let n = 1; n } }""#,
            r#"sql"SELECT foo$tag$bar, foo$1, ${1}::int""#,
        ] {
            let source = format!("fn test() {{ let q = {template} }}");
            parse(&source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        }
        for (template, expected) in [
            (r#"sql"SELECT '${1}'""#, "inside quotes"),
            (r#"sql"SELECT \"{name}\"""#, "inside quotes"),
            (r#"sql"SELECT $$${1}$$""#, "inside quotes"),
            (r#"sql"SELECT $tag$ {name} $tag$""#, "inside quotes"),
            (r#"sql"SELECT 1 -- ${1}""#, "inside quotes"),
            (r#"sql"SELECT /* {name} */ 1""#, "inside quotes"),
            (r#"sql"SELECT $1, ${1}""#, "manual $N"),
            (r#"sql"SELECT '\0'""#, "SQL text cannot contain NUL"),
            (r#"sql"SELECT 'open""#, "Unterminated SQL"),
            (r#"sql"SELECT /* open""#, "Unterminated SQL"),
            (r#"sql"SELECT $tag$open""#, "Unterminated SQL"),
            (r#"sql"SELECT ${}""#, "requires an expression"),
            (r#"sql"SELECT ${1}0""#, "separate SQL token"),
            (r#"sql"SELECT foo${1}""#, "separate SQL token"),
            (r#"sql"SELECT ${1}${2}""#, "separate SQL token"),
        ] {
            let source = format!("fn test() {{ let q = {template} }}");
            let error = parse(&source).unwrap_err();
            assert!(error.message.contains(expected), "{source}: {error:?}");
        }
        let source = "fn test() {\n    let value = 1\n    let q = sql\"SELECT '${value}'\"\n}";
        let id = crate::SourceId::from_raw(7);
        let error = super::super::parse_with_source(source, id).unwrap_err();
        assert_eq!(error.line, 3);
        assert_eq!(error.span.source_id(), id);
    }
}
