use super::*;

impl Parser {
    pub(super) fn parse_parallel_for(&mut self, start: Span) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::Parallel, "expected 'parallel' after '@'")?;
        self.expect_simple(TokenKind::LeftParen, "expected '(limit: N)'")?;
        let label = self
            .advance()
            .ok_or(ParseError::ExpectedExpression { span: Some(start) })?;
        if !matches!(&label.kind, TokenKind::Identifier(name) if name == "limit") {
            return Err(ParseError::ExpectedToken {
                expected: "expected 'limit'".to_owned(),
                span: Some(label.span),
            });
        }
        self.expect_simple(TokenKind::Colon, "expected ':' after limit")?;
        let limit = self.parse_expression(0)?;
        self.expect_simple(TokenKind::RightParen, "expected ')' after parallel limit")?;
        self.skip_newlines();
        self.expect_simple(TokenKind::For, "@parallel applies only to a for expression")?;
        self.parse_for(start, Some(Box::new(limit)))
    }

    pub(super) fn parse_for(
        &mut self,
        start: Span,
        limit: Option<Box<Expr>>,
    ) -> Result<Expr, ParseError> {
        let indexed = self.match_token(TokenKind::LeftParen);
        let first = self.parse_for_name()?;
        let (index, item) = if indexed {
            self.expect_simple(TokenKind::Comma, "expected '(index, item)'")?;
            let item = self.parse_for_name()?;
            self.expect_simple(
                TokenKind::RightParen,
                "expected ')' after iteration bindings",
            )?;
            (Some(first), item)
        } else {
            (None, first)
        };
        let token = self
            .advance()
            .ok_or(ParseError::ExpectedExpression { span: Some(start) })?;
        if !matches!(&token.kind, TokenKind::Identifier(name) if name == "in") {
            return Err(ParseError::ExpectedToken {
                expected: "expected 'in' after iteration binding".to_owned(),
                span: Some(token.span),
            });
        }
        let iterable = self.parse_expression_structural()?;
        let body = self.parse_block()?;
        let span = start.merge(body.span);
        Ok(self.make_expr(
            ExprKind::For {
                index,
                item,
                iterable: Box::new(iterable),
                body: Box::new(body),
                limit,
            },
            span,
        ))
    }

    fn parse_for_name(&mut self) -> Result<String, ParseError> {
        let token = self.advance().ok_or(ParseError::ExpectedExpression {
            span: self.previous_span(),
        })?;
        match token.kind {
            TokenKind::Identifier(name) => Ok(name),
            _ => Err(ParseError::ExpectedToken {
                expected: "expected iteration binding name".to_owned(),
                span: Some(token.span),
            }),
        }
    }

    pub(super) fn parse_if(&mut self, start: Span) -> Result<Expr, ParseError> {
        let condition = self.parse_expression_structural()?;
        let then_branch = self.parse_expression(0)?;
        // The else branch is optional: skip newlines and check for else, backing
        // up without consuming the newline if absent
        let saved = self.position;
        self.skip_newlines();
        let else_branch = if self.match_token(TokenKind::Else) {
            self.parse_expression(0)?
        } else {
            self.position = saved;
            self.make_expr(ExprKind::Block(vec![]), then_branch.span)
        };
        let span = start.merge(else_branch.span);
        Ok(self.make_expr(
            ExprKind::If {
                condition: Box::new(condition),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(else_branch),
            },
            span,
        ))
    }

    pub(super) fn parse_match(&mut self, start: Span) -> Result<Expr, ParseError> {
        let value = self.parse_expression_structural()?;
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after match value")?;
        self.skip_newlines();
        let mut arms = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            let pattern = self.parse_match_pattern()?;
            self.expect_simple(TokenKind::FatArrow, "expected '=>' after match pattern")?;
            let arm_value = self.parse_expression(0)?;
            let arm_span = pattern.span().merge(arm_value.span);
            arms.push(MatchArm {
                pattern,
                value: arm_value,
                span: arm_span,
            });
            self.consume_member_separator()?;
            self.skip_newlines();
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after match arms")?;
        Ok(self.make_expr(
            ExprKind::Match {
                value: Box::new(value),
                arms,
            },
            start.merge(end.span),
        ))
    }

    pub(super) fn parse_while(&mut self, start: Span) -> Result<Expr, ParseError> {
        let condition = self.parse_expression_structural()?;
        let body = self.parse_block()?;
        let span = start.merge(body.span);
        Ok(self.make_expr(
            ExprKind::While {
                condition: Box::new(condition),
                body: Box::new(body),
            },
            span,
        ))
    }

    pub(super) fn parse_loop(&mut self, start: Span) -> Result<Expr, ParseError> {
        let body = self.parse_block()?;
        let span = start.merge(body.span);
        Ok(self.make_expr(
            ExprKind::Loop {
                body: Box::new(body),
            },
            span,
        ))
    }

    pub(super) fn parse_break(&mut self, start: Span) -> Result<Expr, ParseError> {
        let value = match self.peek().map(|token| &token.kind) {
            None | Some(TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBrace) => None,
            _ => Some(Box::new(self.parse_expression(0)?)),
        };
        let span = value
            .as_ref()
            .map_or(start, |value| start.merge(value.span));
        Ok(self.make_expr(ExprKind::Break { value }, span))
    }

    pub(super) fn parse_abort(&mut self, start: Span) -> Result<Expr, ParseError> {
        let value = if matches!(
            self.peek().map(|token| &token.kind),
            None | Some(TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBrace)
        ) {
            return Err(ParseError::ExpectedExpression {
                span: self.previous_span(),
            });
        } else {
            Box::new(self.parse_expression(0)?)
        };
        let span = start.merge(value.span);
        Ok(self.make_expr(ExprKind::Abort { value }, span))
    }

    fn binding_pattern_ids(
        &mut self,
        pattern: &Pattern,
        ids: &mut Vec<(String, crate::syntax::NodeId)>,
    ) {
        match pattern {
            Pattern::Binding { name, .. } => ids.push((name.clone(), self.id_gen.next())),
            Pattern::Tuple { elements, .. } => {
                for element in elements {
                    self.binding_pattern_ids(element, ids);
                }
            }
            _ => {}
        }
    }

    pub(super) fn parse_binding(&mut self, start: Span, mutable: bool) -> Result<Expr, ParseError> {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::LeftParen)
        ) {
            let pattern = self.parse_tuple_pattern()?;
            let mut binding_ids = Vec::new();
            self.binding_pattern_ids(&pattern, &mut binding_ids);
            self.expect_simple(TokenKind::Equal, "expected '=' after binding pattern")?;
            let value = self.parse_expression(0)?;
            let span = start.merge(value.span);
            return Ok(self.make_expr(
                ExprKind::LetPattern {
                    pattern: Box::new(pattern),
                    binding_ids,
                    mutable,
                    value: Box::new(value),
                },
                span,
            ));
        }
        let name_token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected a binding name".to_owned(),
            span: Some(start),
        })?;
        let name = match name_token.kind {
            TokenKind::Identifier(name) => name,
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected a binding name".to_owned(),
                    span: Some(name_token.span),
                });
            }
        };
        let annotation = if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Colon)) {
            self.advance();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.expect_simple(TokenKind::Equal, "expected '=' after binding name")?;
        let value = self.parse_expression(0)?;
        let span = start.merge(value.span);
        Ok(self.make_expr(
            ExprKind::Let {
                name,
                mutable,
                annotation,
                value: Box::new(value),
            },
            span,
        ))
    }
}
