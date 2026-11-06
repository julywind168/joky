//! String delimiters, interpolation and physical-line indentation.
use super::{Diagnostic, InterpolationPart, Lexer, Span, Token, TokenKind};

/// Only literal source newlines split rows. Escaped newlines and expression
/// tokens stay opaque, preserving nested expression syntax and source spans.
struct StringRow {
    parts: Vec<InterpolationPart>,
    span: Span,
    line: usize,
    newline: Span,
}

fn blank(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

fn dedent(mut rows: Vec<StringRow>) -> Result<Vec<InterpolationPart>, Diagnostic> {
    let closer = rows.pop().expect("a string has a closing row");
    let baseline = match closer.parts.as_slice() {
        [InterpolationPart::Text(text, _)] if blank(text) => text,
        _ => {
            return Err(Diagnostic::at(
                closer.line,
                closer.span,
                "Closing \"\"\" of a multiline string must be on its own line",
            ))
        }
    };
    let count = rows.len();
    let mut result = Vec::new();
    for (index, mut row) in rows.into_iter().enumerate() {
        if let [InterpolationPart::Text(text, _)] = row.parts.as_mut_slice() {
            if blank(text) {
                text.clear();
            } else if text.starts_with(baseline) {
                text.drain(..baseline.len());
            } else {
                return Err(indent_error(&row));
            }
        } else if let Some(InterpolationPart::Text(text, _)) = row.parts.first_mut() {
            if !text.starts_with(baseline) {
                return Err(indent_error(&row));
            }
            text.drain(..baseline.len());
        }
        if index + 1 < count {
            row.parts
                .push(InterpolationPart::Text("\n".into(), row.newline));
        }
        for part in row.parts {
            // Keep adjacent physical rows in one text segment, so multiline
            // layout does not introduce extra runtime builder operations.
            match (result.last_mut(), part) {
                (
                    Some(InterpolationPart::Text(previous, span)),
                    InterpolationPart::Text(text, next),
                ) => {
                    previous.push_str(&text);
                    *span = Span::new(span.start(), next.end());
                }
                (_, part) => result.push(part),
            }
        }
    }
    Ok(result)
}

fn indent_error(row: &StringRow) -> Diagnostic {
    Diagnostic::at(
        row.line,
        row.span,
        "Multiline string indentation must match the closing delimiter's whitespace prefix",
    )
}

impl Lexer<'_> {
    pub(super) fn raw_string_prefix(&self) -> bool {
        self.source[self.byte_pos..]
            .strip_prefix('r')
            .is_some_and(|rest| rest.trim_start_matches('#').starts_with('"'))
    }

    /// Normalize LF, CRLF and bare CR, counting each as one source line.
    fn string_newline(&mut self) {
        if self.advance() == Some('\r') && self.peek() == Some('\n') {
            self.advance();
        }
    }

    fn skip_multiline_opener(&mut self, start: usize, line: usize) -> Result<(), Diagnostic> {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.advance();
        }
        match self.peek() {
            Some('\n' | '\r') => {
                self.string_newline();
                Ok(())
            }
            None => Err(Diagnostic::at(
                line,
                start..self.byte_pos,
                "Unterminated string literal",
            )),
            Some(ch) => Err(Diagnostic::at(
                line,
                start..self.byte_pos + ch.len_utf8(),
                "Multiline string must start on the line after \"\"\"",
            )),
        }
    }

    /// Raw literals skip escapes and holes; hole-free strings fold to `StrLit`.
    pub(super) fn read_string(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        self.read_template(start, false)
    }

    pub(super) fn read_sql_string(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        self.read_template(start, true)
    }

    fn read_template(&mut self, start: usize, sql: bool) -> Result<TokenKind, Diagnostic> {
        if self.interpolation_depth >= 64 {
            return Err(Diagnostic::at(
                self.line,
                start..self.byte_pos + 1,
                "Interpolation nesting exceeds 64 levels",
            ));
        }
        self.interpolation_depth += 1;
        let result = self.string_parts(start, sql);
        self.interpolation_depth -= 1;
        let parts = result?;
        if sql {
            return Ok(TokenKind::SqlTemplate(parts));
        }
        if parts
            .iter()
            .any(|part| matches!(part, InterpolationPart::Expression(_)))
        {
            return Ok(TokenKind::Interpolated(parts));
        }
        let mut text = String::new();
        for part in parts {
            let InterpolationPart::Text(part, _) = part else {
                unreachable!("hole-free strings only contain text parts")
            };
            text.push_str(&part);
        }
        Ok(TokenKind::StrLit(text))
    }

    fn string_parts(
        &mut self,
        start: usize,
        sql: bool,
    ) -> Result<Vec<InterpolationPart>, Diagnostic> {
        let line = self.line;
        let raw = self.peek() == Some('r');
        let mut hashes = 0;
        if raw {
            self.advance();
            while self.peek() == Some('#') {
                hashes += 1;
                self.advance();
            }
        }
        let multiline = self.source[self.byte_pos..].starts_with("\"\"\"");
        let quotes = if multiline { 3 } else { 1 };
        self.byte_pos += quotes;
        let terminator = format!("{}{}", "\"".repeat(quotes), "#".repeat(hashes));
        if multiline {
            self.skip_multiline_opener(start, line)?;
        }
        let mut rows = Vec::new();
        let mut row_start = self.byte_pos;
        let mut row_line = self.line;
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut segment = self.byte_pos;
        loop {
            if self.source[self.byte_pos..].starts_with(&terminator) {
                parts.push(InterpolationPart::Text(
                    text,
                    Span::new(segment, self.byte_pos),
                ));
                let end = self.byte_pos;
                self.byte_pos += terminator.len();
                if !multiline {
                    return Ok(parts);
                }
                rows.push(StringRow {
                    parts,
                    span: Span::new(row_start, end),
                    line: row_line,
                    newline: Span::empty(end),
                });
                return dedent(rows);
            }
            match self.peek() {
                None => {
                    return Err(Diagnostic::at(
                        line,
                        start..self.byte_pos,
                        "Unterminated string literal",
                    ))
                }
                Some('{') if !raw && self.peek2() == Some('{') => {
                    self.advance();
                    self.advance();
                    text.push('{');
                }
                Some('}') if !raw && self.peek2() == Some('}') => {
                    self.advance();
                    self.advance();
                    text.push('}');
                }
                Some('}') if !raw => {
                    return Err(Diagnostic::at(
                        self.line,
                        self.byte_pos..self.byte_pos + 1,
                        "Unmatched '}' in string literal; use '}}' for a literal brace",
                    ))
                }
                Some(c) if !raw && (c == '{' || sql && c == '$' && self.peek2() == Some('{')) => {
                    let parameter = c == '$';
                    if parameter {
                        self.advance();
                    }
                    parts.push(InterpolationPart::Text(
                        std::mem::take(&mut text),
                        Span::new(segment, self.byte_pos),
                    ));
                    self.advance();
                    let mut tokens = Vec::new();
                    let mut depth = 0usize;
                    loop {
                        let token = self.next_token()?;
                        match token.kind {
                            TokenKind::Eof => {
                                return Err(Diagnostic::at(
                                    line,
                                    start..self.byte_pos,
                                    "Unterminated interpolation expression",
                                ))
                            }
                            TokenKind::LBrace => depth += 1,
                            TokenKind::RBrace if depth == 0 => {
                                tokens.push(Token {
                                    kind: TokenKind::Eof,
                                    ..token
                                });
                                break;
                            }
                            TokenKind::RBrace => depth -= 1,
                            _ => {}
                        }
                        tokens.push(token);
                    }
                    parts.push(if parameter {
                        InterpolationPart::Parameter(tokens)
                    } else {
                        InterpolationPart::Expression(tokens)
                    });
                    segment = self.byte_pos;
                }
                Some('\\') if !raw => {
                    // Keep the same escape rules for text and hole segments.
                    let escape_start = self.byte_pos;
                    let escape_line = self.line;
                    self.advance();
                    match self.advance() {
                        Some('n') => text.push('\n'),
                        Some('r') => text.push('\r'),
                        Some('t') => text.push('\t'),
                        Some('0') => text.push('\0'),
                        Some('"') => text.push('"'),
                        Some('\\') => text.push('\\'),
                        Some('u') => {
                            text.push(self.read_unicode_escape(escape_start, escape_line)?)
                        }
                        Some('\n' | '\r') => {
                            return Err(Diagnostic::at(
                                escape_line,
                                escape_start..self.byte_pos,
                                "Line continuation is not allowed in string literals",
                            ))
                        }
                        Some(c) => {
                            return Err(Diagnostic::at(
                                self.line,
                                escape_start..self.byte_pos,
                                format!("Unknown string escape '\\{c}'"),
                            ))
                        }
                        None => {
                            return Err(Diagnostic::at(
                                line,
                                start..self.byte_pos,
                                "Unterminated string escape",
                            ))
                        }
                    }
                }
                Some('\n' | '\r') if multiline => {
                    let end = self.byte_pos;
                    self.string_newline();
                    parts.push(InterpolationPart::Text(
                        std::mem::take(&mut text),
                        Span::new(segment, end),
                    ));
                    rows.push(StringRow {
                        parts: std::mem::take(&mut parts),
                        span: Span::new(row_start, end),
                        line: row_line,
                        newline: Span::new(end, self.byte_pos),
                    });
                    row_start = self.byte_pos;
                    row_line = self.line;
                    segment = self.byte_pos;
                }
                Some('\n' | '\r') => {
                    return Err(Diagnostic::at(
                        self.line,
                        self.byte_pos..self.byte_pos + 1,
                        "Line break in string literal; use '\\n' or '\\r'",
                    ))
                }
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
