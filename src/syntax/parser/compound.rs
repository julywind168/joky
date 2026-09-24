use super::*;

impl Parser {
    pub(super) fn parse_closure(
        &mut self,
        move_capture: bool,
        start: Span,
    ) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftParen, "expected '(' after 'fn'")?;
        let mut parameters = Vec::new();
        if !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightParen)
        ) {
            loop {
                let token = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected closure parameter name".to_owned(),
                    span: self.previous_span(),
                })?;
                let TokenKind::Identifier(name) = token.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected closure parameter name".to_owned(),
                        span: Some(token.span),
                    });
                };
                let ty = if self.match_token(TokenKind::Colon) {
                    self.parse_type_annotation()?
                } else {
                    TypeAnnotation::named("_", token.span)
                };
                parameters.push(Parameter {
                    name,
                    borrowed: false,
                    ty,
                    span: token.span,
                });
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect_simple(
            TokenKind::RightParen,
            "expected ')' after closure parameters",
        )?;
        let return_type = if self.match_token(TokenKind::Arrow) {
            self.parse_return_type()?
        } else {
            TypeAnnotation::named("_", self.previous_span().unwrap_or(start))
        };
        let body = self.parse_block()?;
        let span = start.merge(body.span);
        Ok(self.make_expr(
            ExprKind::Closure {
                move_capture,
                parameters,
                return_type,
                body: Box::new(body),
            },
            span,
        ))
    }

    /// Parse a `|params| body` closure literal. `||` arrives as a single
    /// `OrOr` token, so `empty_parameters` records that the caller already
    /// consumed both delimiters. The body is a block or a single expression;
    /// parameter and return types are inferred from the call site.
    pub(super) fn parse_pipe_closure(
        &mut self,
        move_capture: bool,
        start: Span,
        empty_parameters: bool,
    ) -> Result<Expr, ParseError> {
        let mut parameters = Vec::new();
        if !empty_parameters {
            loop {
                let token = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected closure parameter name".to_owned(),
                    span: self.previous_span(),
                })?;
                let TokenKind::Identifier(name) = token.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected closure parameter name".to_owned(),
                        span: Some(token.span),
                    });
                };
                let ty = if self.match_token(TokenKind::Colon) {
                    self.parse_type_annotation()?
                } else {
                    TypeAnnotation::named("_", token.span)
                };
                parameters.push(Parameter {
                    name,
                    borrowed: false,
                    ty,
                    span: token.span,
                });
                if !self.match_token(TokenKind::Comma) {
                    break;
                }
            }
            self.expect_simple(TokenKind::Pipe, "expected '|' after closure parameters")?;
        }
        let return_type = if self.match_token(TokenKind::Arrow) {
            self.parse_return_type()?
        } else {
            TypeAnnotation::named("_", self.previous_span().unwrap_or(start))
        };
        // The closure body is a fresh context, like a block: trailing
        // closures inside it work even in a suppressed enclosing position.
        let outer_suppression = std::mem::replace(&mut self.suppress_trailing_closure, 0);
        let body = if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftBrace)
        ) {
            self.parse_block()?
        } else {
            self.parse_expression(0)?
        };
        self.suppress_trailing_closure = outer_suppression;
        let span = start.merge(body.span);
        Ok(self.make_expr(
            ExprKind::Closure {
                move_capture,
                parameters,
                return_type,
                body: Box::new(body),
            },
            span,
        ))
    }

    /// A trailing closure attaches to the call in front of it. It requires a
    /// `{` body so `ready() || fallback()` stays a logical-or expression, and
    /// it never crosses a newline so `parallel`/`race` arms keep their `|`.
    /// A bare `{` starts the zero-parameter form with `||` omitted.
    pub(super) fn starts_trailing_closure(&self) -> bool {
        match self.peek().map(|token| &token.kind) {
            Some(TokenKind::OrOr) => {
                Self::starts_closure_body(self.tokens.get(self.position + 1).map(|t| &t.kind))
            }
            Some(TokenKind::Pipe) => self.closing_pipe_offset().is_some_and(|offset| {
                Self::starts_closure_body(self.tokens.get(offset + 1).map(|t| &t.kind))
            }),
            Some(TokenKind::LeftBrace) => true,
            _ => false,
        }
    }

    /// A closure body starts at `{`, or at `-> Type {` when the literal spells
    /// its return type. Anything else leaves `||`/`|` as an operator.
    fn starts_closure_body(kind: Option<&TokenKind>) -> bool {
        matches!(kind, Some(TokenKind::LeftBrace | TokenKind::Arrow))
    }

    fn closing_pipe_offset(&self) -> Option<usize> {
        let mut depth = 0usize;
        for (offset, token) in self.tokens.iter().enumerate().skip(self.position + 1) {
            match token.kind {
                TokenKind::LeftParen | TokenKind::LeftBracket => depth += 1,
                TokenKind::RightParen | TokenKind::RightBracket => depth = depth.checked_sub(1)?,
                TokenKind::Pipe if depth == 0 => return Some(offset),
                TokenKind::Newline | TokenKind::LeftBrace | TokenKind::RightBrace => return None,
                _ => {}
            }
        }
        None
    }

    pub(super) fn parse_concurrent_arms(
        &mut self,
        start: Span,
        parallel: bool,
    ) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftBrace, "expected '{' before concurrent arms")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::RightBrace)
            ) {
                let end = self.advance().expect("right brace checked above");
                if arms.is_empty() {
                    return Err(ParseError::ExpectedExpression {
                        span: Some(end.span),
                    });
                }
                let span = start.merge(end.span);
                let kind = if parallel {
                    ExprKind::Parallel(arms)
                } else {
                    ExprKind::Race(arms)
                };
                return Ok(self.make_expr(kind, span));
            }
            self.expect_simple(TokenKind::Pipe, "expected '|' before concurrent arm")?;
            self.stop_at_arm_pipe = true;
            let arm = self.parse_expression(0);
            self.stop_at_arm_pipe = false;
            arms.push(arm?);
            match self.peek().map(|token| &token.kind) {
                Some(TokenKind::Newline) => self.skip_newlines(),
                Some(TokenKind::RightBrace) => {}
                _ => {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected a newline or '}' after concurrent arm".to_owned(),
                        span: self.peek().map(|token| token.span),
                    });
                }
            }
        }
    }

    pub(super) fn parse_collection_literal(
        &mut self,
        name: String,
        start: Span,
    ) -> Result<Expr, ParseError> {
        let kind = match name.as_str() {
            "List" => 0,
            "MutList" => 1,
            "Set" => 2,
            "MutSet" => 3,
            "Map" => 4,
            "MutMap" => 5,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "List#, MutList#, Set#, MutSet#, Map#, or MutMap# collection literal"
                        .to_owned(),
                    span: Some(start),
                })
            }
        };
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after collection '#'")?;
        self.skip_newlines();
        let mut elements = Vec::new();
        let mut entries = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            if matches!(kind, 4 | 5) {
                let key = self.parse_expression(0)?;
                self.expect_simple(TokenKind::FatArrow, "expected '=>' in Map literal")?;
                let value = self.parse_expression(0)?;
                entries.push(MapLiteralEntry { key, value });
            } else {
                elements.push(self.parse_expression(0)?);
            }
            if !self.match_token(TokenKind::Comma) {
                self.skip_newlines();
                if !matches!(
                    self.peek().map(|token| &token.kind),
                    Some(TokenKind::RightBrace)
                ) {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected ',' or '}' in collection literal".to_owned(),
                        span: self.previous_span(),
                    });
                }
            } else {
                self.skip_newlines();
            }
        }
        let end = self.expect_simple(
            TokenKind::RightBrace,
            "expected '}' after collection literal",
        )?;
        let literal = match kind {
            0 => CollectionLiteral::List(elements),
            1 => CollectionLiteral::MutList(elements),
            2 => CollectionLiteral::Set(elements),
            3 => CollectionLiteral::MutSet(elements),
            4 => CollectionLiteral::Map(entries),
            _ => CollectionLiteral::MutMap(entries),
        };
        Ok(self.make_expr(ExprKind::CollectionLiteral(literal), start.merge(end.span)))
    }
}
