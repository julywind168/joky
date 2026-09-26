use super::*;

impl Parser {
    pub(super) fn parse_match_pattern(&mut self) -> Result<Pattern, ParseError> {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftParen)
        ) {
            return self.parse_tuple_pattern();
        }
        let token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected enum name in match pattern".to_owned(),
            span: self.previous_span(),
        })?;
        self.parse_pattern_from_token(token, false)
    }

    pub(super) fn parse_nested_pattern(&mut self) -> Result<Pattern, ParseError> {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftParen)
        ) {
            return self.parse_tuple_pattern();
        }
        let token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected nested pattern".to_owned(),
            span: self.previous_span(),
        })?;
        self.parse_pattern_from_token(token, true)
    }

    pub(super) fn parse_tuple_pattern(&mut self) -> Result<Pattern, ParseError> {
        let start = self
            .advance()
            .ok_or(ParseError::ExpectedToken {
                expected: "expected '(' in tuple pattern".to_owned(),
                span: self.previous_span(),
            })?
            .span;
        if self.match_token(TokenKind::RightParen) {
            let end = self.previous_span().unwrap_or(start);
            return Ok(Pattern::Tuple {
                elements: Vec::new(),
                span: start.merge(end),
            });
        }
        let first = self.parse_nested_pattern()?;
        if !self.match_token(TokenKind::Comma) {
            let end = self.expect_simple(TokenKind::RightParen, "expected ')' after pattern")?;
            let mut pattern = first;
            match &mut pattern {
                Pattern::Wildcard { span }
                | Pattern::Binding { span, .. }
                | Pattern::EnumVariant { span, .. }
                | Pattern::Tuple { span, .. } => *span = start.merge(end.span),
            }
            return Ok(pattern);
        }
        let mut elements = vec![first];
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightParen)
        ) {
            elements.push(self.parse_nested_pattern()?);
            if !self.match_token(TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect_simple(TokenKind::RightParen, "expected ')' after tuple pattern")?;
        Ok(Pattern::Tuple {
            elements,
            span: start.merge(end.span),
        })
    }

    pub(super) fn parse_pattern_from_token(
        &mut self,
        token: Token,
        allow_binding: bool,
    ) -> Result<Pattern, ParseError> {
        let TokenKind::Identifier(name) = token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected pattern".to_owned(),
                span: Some(token.span),
            });
        };
        if name == "_" {
            return Ok(Pattern::Wildcard { span: token.span });
        }
        if !self.match_token(TokenKind::Dot) {
            // Unqualified Option/Result variant patterns parse at any nesting
            // depth, so payloads like `Ok(Some(pair))` stay patterns rather
            // than degrading into a binding of the name `Some`.
            if matches!(name.as_str(), "Some" | "None" | "Ok" | "Err") {
                let mut fields = Vec::new();
                let mut rest = false;
                if self.match_token(TokenKind::LeftParen) {
                    if self.match_token(TokenKind::DotDot) {
                        rest = true;
                    } else {
                        let field = self.parse_nested_pattern()?;
                        let span = token.span.merge(field.span());
                        fields.push(PatternField {
                            label: None,
                            pattern: field,
                            span,
                        });
                    }
                    let kind = if matches!(name.as_str(), "Ok" | "Err") {
                        "Result"
                    } else {
                        "Option"
                    };
                    self.expect_simple(
                        TokenKind::RightParen,
                        &format!("expected ')' after {kind} pattern"),
                    )?;
                }
                let enum_name = if matches!(name.as_str(), "Ok" | "Err") {
                    "Result"
                } else {
                    "Option"
                };
                let variant = name.clone();
                let end = self.previous_span().unwrap_or(token.span);
                return Ok(Pattern::EnumVariant {
                    enum_name: enum_name.to_owned(),
                    variant,
                    fields,
                    rest,
                    span: token.span.merge(end),
                });
            }
            if allow_binding {
                return Ok(Pattern::Binding {
                    name,
                    span: token.span,
                });
            }
            return Err(ParseError::ExpectedToken {
                expected: "expected '.' in match pattern".to_owned(),
                span: Some(token.span),
            });
        }
        let variant_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected enum variant in match pattern".to_owned(),
            span: Some(token.span),
        })?;
        let TokenKind::Identifier(variant) = variant_token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected enum variant in match pattern".to_owned(),
                span: Some(variant_token.span),
            });
        };
        let mut fields = Vec::new();
        let mut rest = false;
        if self.match_token(TokenKind::LeftParen) {
            if !matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::RightParen)
            ) {
                loop {
                    if self.match_token(TokenKind::DotDot) {
                        rest = true;
                        if self.match_token(TokenKind::Comma) {
                            return Err(ParseError::ExpectedToken {
                                expected: "'..' must be the last enum pattern field".to_owned(),
                                span: self.previous_span(),
                            });
                        }
                        break;
                    }
                    let field_token = self.advance().ok_or(ParseError::ExpectedToken {
                        expected: "expected field pattern".to_owned(),
                        span: Some(variant_token.span),
                    })?;
                    let label = match &field_token.kind {
                        TokenKind::Identifier(label)
                            if label != "_" && self.match_token(TokenKind::Colon) =>
                        {
                            Some(label.clone())
                        }
                        _ => None,
                    };
                    let pattern = if label.is_some() {
                        self.parse_nested_pattern()?
                    } else {
                        self.parse_pattern_from_token(field_token.clone(), true)?
                    };
                    fields.push(PatternField {
                        label,
                        span: field_token.span.merge(pattern.span()),
                        pattern,
                    });
                    if !self.match_token(TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect_simple(TokenKind::RightParen, "expected ')' after match bindings")?;
        }
        let end = self.previous_span().unwrap_or(variant_token.span);
        Ok(Pattern::EnumVariant {
            enum_name: name,
            variant,
            fields,
            rest,
            span: token.span.merge(end),
        })
    }
}
