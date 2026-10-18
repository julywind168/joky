//! Control flow syntax.
use super::*;

impl Parser {
    pub(super) fn parse_condition(&mut self) -> Result<Expr, Diagnostic> {
        let previous = std::mem::replace(&mut self.allow_record, false);
        let result = self.parse_assignment();
        self.allow_record = previous;
        result
    }

    pub(super) fn parse_match(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::Match)?;
        let value = Box::new(self.parse_condition()?);
        self.expect(&TokenKind::LBrace)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
        let previous_newlines = std::mem::replace(&mut self.newline_sensitive, true);
        let mut arms = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            let arm_start = self.peek_span().start();
            let pattern = self.parse_match_pattern()?;
            self.expect(&TokenKind::FatArrow)?;
            let body = self.parse_sequence(true)?;
            arms.push(MatchArm {
                pattern,
                body,
                span: self.span(arm_start, self.last_span.end()),
            });
            self.expect_braced_separator("match arms")?;
        }
        self.expect(&TokenKind::RBrace)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        self.newline_sensitive = previous_newlines;
        Ok(Expr::Match {
            conditional: false,
            value,
            arms,
            span: self.span(start, self.last_span.end()),
        })
    }

    /// Share enum pattern syntax with match, including recursive payloads.
    fn parse_match_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        if matches!(
            self.peek(),
            TokenKind::LBracket
                | TokenKind::IntLit(_)
                | TokenKind::Minus
                | TokenKind::True
                | TokenKind::False
                | TokenKind::CharLit(_)
        ) {
            return self
                .parse_match_payload_pattern()
                .map(|p| Pattern::Value(Box::new(p)));
        }
        let pattern_start = self.peek_span().start();
        let name = self.expect_ident()?;
        let name = self.parse_dotted_name(name)?;
        let pattern = if name == "_" {
            Pattern::Wildcard {
                span: self.last_span,
            }
        } else {
            let binding = if self.peek() == &TokenKind::LParen {
                self.advance();
                let binding = self.parse_match_payload_pattern()?;
                if self.peek() == &TokenKind::Comma {
                    self.advance();
                }
                self.expect(&TokenKind::RParen)?;
                Some(Box::new(binding))
            } else {
                None
            };
            Pattern::Variant {
                name,
                binding,
                span: self.span(pattern_start, self.last_span.end()),
            }
        };
        Ok(pattern)
    }

    fn parse_let_condition(&mut self) -> Result<Option<(Pattern, Expr)>, Diagnostic> {
        if self.peek() != &TokenKind::Let {
            return Ok(None);
        }
        self.advance();
        let pattern = self.parse_match_pattern()?;
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_condition()?;
        Ok(Some((pattern, value)))
    }

    /// Conditional patterns lower to two match arms, retaining normal match
    /// binding, ownership and borrow planning. The fallback is always implicit.
    fn conditional_match(
        pattern: Pattern,
        value: Expr,
        success: Expr,
        fallback: Expr,
        span: Span,
    ) -> Expr {
        Expr::Match {
            conditional: true,
            value: Box::new(value),
            arms: vec![
                MatchArm {
                    span: success.span(),
                    pattern,
                    body: success,
                },
                MatchArm {
                    span: fallback.span(),
                    pattern: Pattern::Wildcard { span },
                    body: fallback,
                },
            ],
            span,
        }
    }

    pub(super) fn parse_while(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::While)?;
        let matched = self.parse_let_condition()?;
        let condition = if matched.is_some() {
            Expr::Literal {
                literal: Literal::Bool(true),
                span: self.last_span,
            }
        } else {
            self.parse_condition()?
        };
        let mut body = self.parse_block()?;
        let span = self.span(start, self.last_span.end());
        if let Some((pattern, value)) = matched {
            let body_span = body.span();
            // Loop bodies discard their values even when the fallback diverges.
            let success = Expr::Sequence {
                exprs: vec![body],
                trailing: true,
                span: body_span,
            };
            body = Self::conditional_match(pattern, value, success, Expr::Break { span }, span);
        }
        Ok(Expr::While {
            condition: Box::new(condition),
            body: Box::new(body),
            span,
        })
    }

    pub(super) fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::If)?;
        let matched = self.parse_let_condition()?;
        let condition = if matched.is_some() {
            None
        } else {
            Some(Box::new(self.parse_condition()?))
        };
        let then_branch = Box::new(self.parse_block()?);
        let else_branch = if self.peek() == &TokenKind::Else {
            self.advance();
            Some(Box::new(if self.peek() == &TokenKind::If {
                self.parse_if()?
            } else {
                self.parse_block()?
            }))
        } else {
            None
        };
        let span = self.span(start, self.last_span.end());
        if let Some((pattern, value)) = matched {
            let fallback = else_branch.map(|branch| *branch).unwrap_or(Expr::Block {
                exprs: vec![],
                span,
            });
            return Ok(Self::conditional_match(
                pattern,
                value,
                *then_branch,
                fallback,
                span,
            ));
        }
        Ok(Expr::If {
            condition: condition.unwrap(),
            then_branch,
            else_branch,
            span: self.span(start, self.last_span.end()),
        })
    }

    /// Parse a contextual graph block without retaining the atom parser frame.
    pub(super) fn parse_graph(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let Expr::Block { exprs, span } = self.parse_block()? else {
            unreachable!()
        };
        Ok(Expr::Graph {
            exprs,
            span: self.span(start, span.end()),
        })
    }

    /// Parse a block of expressions.
    pub(super) fn parse_block(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::LBrace)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
        let previous_newlines = std::mem::replace(&mut self.newline_sensitive, true);
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            stmts.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RBrace)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        self.newline_sensitive = previous_newlines;
        Ok(Expr::Block {
            exprs: stmts,
            span: self.span(start, self.last_span.end()),
        })
    }

    // ── Top-level parsing ──────────────────────────────────────────────────────────
}
