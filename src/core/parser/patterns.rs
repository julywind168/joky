//! Patterns syntax.
use super::*;
use crate::parser::ast::RecordFieldPattern;

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
        if self.peek() == &TokenKind::LBracket {
            return self
                .parse_sequence_pattern(Self::parse_binding_pattern)
                .map(|p| BindingPattern::Sequence(Box::new(p)));
        }
        if self.peek() != &TokenKind::LParen {
            if self.peek() == &TokenKind::Amp {
                return Err(self.error(
                    "Reference patterns are not supported; destructure a value or a shared reference".into(),
                ));
            }
            let name = self.expect_ident()?;
            if name == "_" {
                return Ok(BindingPattern::Discard {
                    span: self.last_span,
                });
            }
            let name = self.parse_dotted_name(name)?;
            let type_args = if self.peek() == &TokenKind::ColonColon {
                self.advance();
                self.parse_type_arguments()?
            } else {
                Vec::new()
            };
            if self.peek() == &TokenKind::LBrace {
                self.advance();
                let (fields, rest) = self.parse_record_pattern_fields()?;
                return Ok(BindingPattern::Record {
                    inner: Box::new(crate::parser::ast::RecordPattern {
                        resolved: None,
                        name,
                        type_args,
                        fields,
                        rest,
                    }),
                    span: self.span(start, self.last_span.end()),
                });
            }
            if name.contains('.') || !type_args.is_empty() {
                return Err(self.error("Record patterns require a field list in braces".into()));
            }
            self.check_binding_name(&name)?;
            return Ok(BindingPattern::Name {
                name,
                span: self.span(start, self.last_span.end()),
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

    /// Field entries of a record pattern, after the opening brace. `..` is at
    /// most one entry, in final position, optionally followed by one comma.
    fn parse_record_pattern_fields(
        &mut self,
    ) -> Result<(Vec<RecordFieldPattern>, bool), Diagnostic> {
        let mut fields = Vec::new();
        let mut rest = false;
        while self.peek() != &TokenKind::RBrace {
            if self.peek() == &TokenKind::Comma {
                return Err(self.error("Record pattern fields cannot be empty".into()));
            }
            if self.peek() == &TokenKind::DotDot {
                self.advance();
                rest = true;
                if self.peek() == &TokenKind::Comma {
                    self.advance();
                    if self.peek() != &TokenKind::RBrace {
                        return Err(self.error("`..` must be the last record pattern entry".into()));
                    }
                }
                break;
            }
            let field_start = self.peek_span().start();
            let name = self.expect_ident()?;
            if name == "_" {
                return Err(self.error(
                    "Record pattern fields require a name; discard with `field: _`".into(),
                ));
            }
            let pattern = if self.peek() == &TokenKind::Colon {
                self.advance();
                self.parse_binding_pattern()?
            } else {
                self.check_binding_name(&name)?;
                BindingPattern::Name {
                    name: name.clone(),
                    span: self.span(field_start, self.last_span.end()),
                }
            };
            fields.push(RecordFieldPattern {
                name,
                pattern,
                span: self.span(field_start, self.last_span.end()),
            });
            self.expect_braced_separator("record pattern fields")?;
        }
        self.expect(&TokenKind::RBrace)?;
        Ok((fields, rest))
    }

    // ── Parameter lists ──────────────────────────────────────────────────────────
    pub(super) fn parse_match_payload_pattern(&mut self) -> Result<PayloadPattern, Diagnostic> {
        if self.pattern_depth >= 256 {
            return Err(self.error("Match pattern nesting exceeds 256 levels".into()));
        }
        self.pattern_depth += 1;
        let start = self.peek_span().start();
        let result = if self.peek() == &TokenKind::LBracket {
            PayloadPattern::Sequence(Box::new(
                self.parse_sequence_pattern(Self::parse_match_payload_pattern)?,
            ))
        } else if matches!(
            self.peek(),
            TokenKind::IntLit(_)
                | TokenKind::Minus
                | TokenKind::True
                | TokenKind::False
                | TokenKind::CharLit(_)
        ) {
            let negative = self.peek() == &TokenKind::Minus;
            if negative {
                self.advance();
            }
            let literal = match self.peek().clone() {
                TokenKind::IntLit(magnitude) => Literal::Integer {
                    magnitude,
                    negative,
                },
                TokenKind::True if !negative => Literal::Bool(true),
                TokenKind::False if !negative => Literal::Bool(false),
                TokenKind::CharLit(value) if !negative => Literal::Rune(value as u32),
                _ => return Err(self.error("Expected an integer after '-' in pattern".into())),
            };
            self.advance();
            if matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
                if !matches!(literal, Literal::Integer { .. }) {
                    return Err(self.error("Range patterns require integer literal bounds".into()));
                }
                let inclusive = self.peek() == &TokenKind::DotDotEq;
                self.advance();
                let negative = self.peek() == &TokenKind::Minus;
                if negative {
                    self.advance();
                }
                let TokenKind::IntLit(magnitude) = self.peek().clone() else {
                    return Err(self.error("Range patterns require integer literal bounds".into()));
                };
                self.advance();
                PayloadPattern::Range {
                    start: literal,
                    end: Literal::Integer {
                        magnitude,
                        negative,
                    },
                    inclusive,
                    span: self.span(start, self.last_span.end()),
                }
            } else {
                PayloadPattern::Literal {
                    literal,
                    span: self.span(start, self.last_span.end()),
                }
            }
        } else if self.peek() == &TokenKind::LParen {
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
            if name == "_" {
                let span = self.span(start, self.last_span.end());
                self.pattern_depth -= 1;
                return Ok(PayloadPattern::Discard { span });
            }
            let name = self.parse_dotted_name(name)?;
            let type_args = if self.peek() == &TokenKind::ColonColon {
                self.advance();
                self.parse_type_arguments()?
            } else {
                Vec::new()
            };
            if self.peek() == &TokenKind::LBrace {
                self.advance();
                let (fields, rest) = self.parse_record_pattern_fields()?;
                PayloadPattern::Record {
                    resolved: None,
                    name,
                    type_args,
                    fields,
                    rest,
                    span: self.span(start, self.last_span.end()),
                }
            } else if !type_args.is_empty() {
                return Err(self.error("Record patterns require a field list in braces".into()));
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
                    span: self.span(start, self.last_span.end()),
                }
            } else {
                PayloadPattern::Name {
                    name,
                    span: self.span(start, self.last_span.end()),
                }
            }
        };
        self.pattern_depth -= 1;
        Ok(result)
    }
}

impl Parser {
    fn parse_sequence_pattern<P>(
        &mut self,
        parse: fn(&mut Self) -> Result<P, Diagnostic>,
    ) -> Result<SequencePattern<P>, Diagnostic> {
        self.arrays = true;
        let start = self.peek_span().start();
        self.expect(&TokenKind::LBracket)?;
        let mut prefix = Vec::new();
        let mut suffix = Vec::new();
        let mut rest = None;
        while self.peek() != &TokenKind::RBracket {
            if self.peek() == &TokenKind::DotDot {
                if rest.is_some() {
                    return Err(self.error("Sequence patterns allow only one rest".into()));
                }
                self.advance();
                let name = if let TokenKind::Ident(name) = self.peek().clone() {
                    self.advance();
                    self.check_binding_name(&name)?;
                    (name != "_").then_some(name)
                } else {
                    None
                };
                rest = Some(name);
            } else if rest.is_some() {
                suffix.push(parse(self)?);
            } else {
                prefix.push(parse(self)?);
            }
            if self.peek() != &TokenKind::RBracket {
                self.expect(&TokenKind::Comma)?;
            }
        }
        self.expect(&TokenKind::RBracket)?;
        Ok(SequencePattern {
            prefix,
            suffix,
            rest,
            span: self.span(start, self.last_span.end()),
        })
    }
}
