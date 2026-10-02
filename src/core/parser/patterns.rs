//! Patterns syntax.
use super::*;

impl Parser {
    pub(super) fn parse_binding_pattern(&mut self) -> Result<BindingPattern, Diagnostic> {
        if self.pattern_depth >= 256 {
            return Err(self.error("Binding pattern nesting exceeds 256 levels".into()));
        }
        self.pattern_depth += 1;
        let result = self.parse_binding_pattern_inner();
        self.pattern_depth -= 1;
        result
    }

    fn parse_binding_pattern_inner(&mut self) -> Result<BindingPattern, Diagnostic> {
        let start = self.peek_span().start();
        if self.peek() != &TokenKind::LParen {
            let name = self.expect_ident()?;
            let span = self.last_span;
            return Ok(if name == "_" {
                BindingPattern::Discard { span }
            } else {
                BindingPattern::Name { name, span }
            });
        }
        self.advance();
        let first = self.parse_binding_pattern()?;
        if self.peek() != &TokenKind::Comma {
            self.expect(&TokenKind::RParen)?;
            return Ok(first);
        }
        self.advance();
        let mut elements = vec![first];
        while self.peek() != &TokenKind::RParen {
            elements.push(self.parse_binding_pattern()?);
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        Ok(BindingPattern::Tuple {
            elements,
            span: self.span(start, self.last_span.end()),
        })
    }

    // ── Parameter lists ──────────────────────────────────────────────────────────
    pub(super) fn parse_match_payload_pattern(&mut self) -> Result<PayloadPattern, Diagnostic> {
        if self.pattern_depth >= 256 {
            return Err(self.error("Match pattern nesting exceeds 256 levels".into()));
        }
        self.pattern_depth += 1;
        let start = self.peek_span().start();
        let result = if self.peek() == &TokenKind::LParen {
            self.advance();
            let first = self.parse_match_payload_pattern()?;
            if self.peek() != &TokenKind::Comma {
                self.expect(&TokenKind::RParen)?;
                first
            } else {
                self.advance();
                let mut elements = vec![first];
                while self.peek() != &TokenKind::RParen {
                    elements.push(self.parse_match_payload_pattern()?);
                    self.expect_list_separator()?;
                }
                self.expect(&TokenKind::RParen)?;
                PayloadPattern::Tuple {
                    elements,
                    span: self.span(start, self.last_span.end()),
                }
            }
        } else {
            let name = self.expect_ident()?;
            let name = self.parse_dotted_name(name)?;
            let span = self.span(start, self.last_span.end());
            if name == "_" {
                PayloadPattern::Discard { span }
            } else if self.peek() == &TokenKind::LParen {
                self.advance();
                let payload = self.parse_match_payload_pattern()?;
                if self.peek() == &TokenKind::Comma {
                    self.advance();
                }
                self.expect(&TokenKind::RParen)?;
                PayloadPattern::Variant {
                    name,
                    payload: Some(Box::new(payload)),
                    span: self.span(start, self.last_span.end()),
                }
            } else if name.contains('.') {
                PayloadPattern::Variant {
                    name,
                    payload: None,
                    span,
                }
            } else {
                PayloadPattern::Name { name, span }
            }
        };
        self.pattern_depth -= 1;
        Ok(result)
    }
}
