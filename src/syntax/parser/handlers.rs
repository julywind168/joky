use super::*;

impl Parser {
    pub(super) fn parse_do(&mut self, start: Span) -> Result<Expr, ParseError> {
        let body = self.parse_block()?;
        self.expect_simple(TokenKind::With, "expected 'with' after do body")?;
        let handler_left = self.expect_simple(TokenKind::LeftBrace, "expected '{' after 'with'")?;
        self.skip_newlines();
        let mut handlers = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            handlers.push(self.parse_handler_arm()?);
            self.consume_member_separator()?;
            self.skip_newlines();
        }
        let right = self.expect_simple(TokenKind::RightBrace, "expected '}' after handlers")?;
        if handlers.is_empty() {
            return Err(ParseError::ExpectedToken {
                expected: "expected at least one handler arm".to_owned(),
                span: Some(handler_left.span),
            });
        }
        Ok(self.make_expr(
            ExprKind::Do {
                body: Box::new(body),
                handlers,
            },
            start.merge(right.span),
        ))
    }

    pub(super) fn parse_when(&mut self, start: Span) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftParen, "expected '(' after 'when'")?;
        let mut cowns = vec![self.parse_expression(0)?];
        while self.match_token(TokenKind::Comma) {
            cowns.push(self.parse_expression(0)?);
        }
        self.expect_simple(TokenKind::RightParen, "expected ')' after when Cown list")?;

        // Payload binding uses a trailing closure parameter list `|a, b|`.
        // Omitting it binds simple-name Cowns to same-named payload locals;
        // the checker rejects complex Cown expressions without a list.
        let bindings = if self.match_token(TokenKind::Pipe) {
            let mut patterns = vec![self.parse_when_parameter()?];
            while self.match_token(TokenKind::Comma) {
                patterns.push(self.parse_when_parameter()?);
            }
            self.expect_simple(
                TokenKind::Pipe,
                "expected '|' after when payload parameters",
            )?;
            Some(patterns)
        } else if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::OrOr)) {
            return Err(ParseError::ExpectedToken {
                expected: "'||' binds no payload; omit the parameter list to bind payloads \
                           implicitly, or write |name| with one name per Cown"
                    .to_owned(),
                span: self.peek().map(|token| token.span),
            });
        } else {
            None
        };

        let until = if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Identifier(word)) if word == "until"
        ) {
            self.expect_contextual_word("until")?;
            Some(Box::new(self.parse_expression_structural()?))
        } else {
            None
        };

        let left = self.expect_simple(TokenKind::LeftBrace, "expected '{' after when Cown list")?;
        self.skip_newlines();
        // The removed `#in` binding appeared as the first item of the body;
        // point the migration diagnostic at it.
        if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Hash)) {
            let hash = self.advance().expect("peeked '#'");
            let keyword = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected a payload parameter list like |state|".to_owned(),
                span: Some(hash.span),
            })?;
            return Err(ParseError::ExpectedToken {
                expected: "'#in' is no longer accepted; name the Cown payloads with closure \
                           parameters, e.g. when (c) |state| { ... }"
                    .to_owned(),
                span: Some(hash.span.merge(keyword.span)),
            });
        }
        let body = self.parse_block_after_left_brace(left.span)?;
        Ok(self.make_expr(
            ExprKind::When {
                cowns,
                bindings,
                until,
                body: Box::new(body.clone()),
            },
            start.merge(body.span),
        ))
    }

    fn parse_when_parameter(&mut self) -> Result<Pattern, ParseError> {
        let token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected a Cown payload name".to_owned(),
            span: self.previous_span(),
        })?;
        let TokenKind::Identifier(name) = token.kind else {
            return Err(ParseError::ExpectedToken {
                expected: "expected a Cown payload name".to_owned(),
                span: Some(token.span),
            });
        };
        if name == "_" {
            Ok(Pattern::Wildcard { span: token.span })
        } else {
            Ok(Pattern::Binding {
                name,
                span: token.span,
            })
        }
    }

    pub(super) fn parse_handler_arm(&mut self) -> Result<HandlerArm, ParseError> {
        let effect_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected effect name in handler".to_owned(),
            span: self.previous_span(),
        })?;
        let mut effect = match effect_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected effect name in handler".to_owned(),
                    span: Some(effect_token.span),
                })
            }
        };
        self.expect_simple(TokenKind::Dot, "expected '.' between effect and operation")?;
        let operation_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected operation name in handler".to_owned(),
            span: self.previous_span(),
        })?;
        let mut operation = match operation_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected operation name in handler".to_owned(),
                    span: Some(operation_token.span),
                })
            }
        };
        while self.match_token(TokenKind::Dot) {
            effect.push('.');
            effect.push_str(&operation);
            let token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected operation name".into(),
                span: self.previous_span(),
            })?;
            operation = match token.kind {
                TokenKind::Identifier(name) => name,
                _ => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected operation name".into(),
                        span: Some(token.span),
                    })
                }
            };
        }
        self.expect_simple(TokenKind::LeftParen, "expected '(' after operation name")?;
        let mut parameters = Vec::new();
        if !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightParen)
        ) {
            loop {
                parameters.push(self.parse_nested_pattern()?);
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect_simple(
            TokenKind::RightParen,
            "expected ')' after handler parameters",
        )?;
        self.expect_simple(TokenKind::FatArrow, "expected '=>' after handler pattern")?;
        let value = self.parse_expression(0)?;
        Ok(HandlerArm {
            id: self.id_gen.next(),
            effect,
            operation,
            parameters,
            span: effect_token.span.merge(value.span),
            value,
        })
    }
}
