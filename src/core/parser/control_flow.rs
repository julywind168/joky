//! Control flow syntax.
use super::*;

impl Parser<'_> {
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
        let previous_newlines = std::mem::replace(&mut self.newlines, Newlines::Separate);
        let mut arms = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            let arm_start = self.peek_span().start();
            let pattern = self.parse_match_pattern()?;
            if self.peek() == &TokenKind::Pipe {
                return Err(self.error(
                    "Or-patterns (`a | b`) are not supported; use separate match arms".into(),
                ));
            }
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
        self.newlines = previous_newlines;
        Ok(Expr::Match {
            conditional: false,
            iteration: false,
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
                | TokenKind::LParen
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
            iteration: false,
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

    /// Lower to ordinary scoped bindings, match and while. Unspellable aliases
    /// select the standard protocol, and destructuring retains irrefutability.
    pub(super) fn parse_for(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::For)?;
        let mut pattern = self.parse_binding_pattern()?;
        let index = if self.peek() == &TokenKind::Comma {
            let name = match &pattern {
                BindingPattern::Name { name, .. } => name.clone(),
                BindingPattern::Discard { .. } => "_".to_owned(),
                _ => return Err(self.error("A for index must be a name or '_'".into())),
            };
            self.advance();
            pattern = self.parse_binding_pattern()?;
            if name != "_" && binds_name(&pattern, &name) {
                return Err(self.error(format!("Duplicate binding `{name}` in for pattern")));
            }
            Some(name)
        } else {
            None
        };
        self.expect(&TokenKind::In)?;
        let value = self.parse_condition()?;
        let value_span = value.span();
        let body = self.parse_block()?;
        let span = self.span(start, self.last_span.end());
        self.iterators = true;
        let input = format!("$for_input_{start}");
        let iterator = format!("$for_iterator_{start}");
        let item = format!("$for_item_{start}");
        let counter = format!("$for_index_{start}");
        let overflow = format!("$for_overflow_{start}");
        let body = if let Some(name) = &index {
            Expr::IndexedLoop(Box::new(crate::parser::ast::IndexedLoop {
                name: name.clone(),
                counter: counter.clone(),
                overflow: overflow.clone(),
                body,
                span,
            }))
        } else {
            body
        };
        let binding_span = pattern.span();
        let item_value = Box::new(Expr::Ident {
            name: item.clone(),
            span: binding_span,
        });
        let binding = if let BindingPattern::Name { name, .. } = pattern {
            Expr::Let {
                name,
                annotation: None,
                value: item_value,
                span: binding_span,
            }
        } else {
            Expr::Destructure {
                pattern,
                mutable: false,
                annotation: None,
                value: item_value,
                span: binding_span,
            }
        };
        let next = Expr::Call {
            callee: "$joky_iter.next".into(),
            type_args: vec![],
            args: vec![Expr::Unary {
                op: UnaryOp::BorrowMut,
                operand: Box::new(Expr::Ident {
                    name: iterator.clone(),
                    span: value_span,
                }),
                span: value_span,
            }],
            span: value_span,
        };
        let mut body = Self::conditional_match(
            Pattern::Variant {
                name: "$joky_option.Option.Some".into(),
                binding: Some(Box::new(PayloadPattern::Name {
                    name: item,
                    span: binding_span,
                })),
                span: binding_span,
            },
            next,
            Expr::Sequence {
                exprs: vec![binding, body],
                trailing: true,
                span,
            },
            Expr::Break { span },
            span,
        );
        if let Expr::Match { iteration, .. } = &mut body {
            *iteration = true;
        }
        // Capture before conversion so input control flow and exclusive-reference
        // moves obey ordinary binding rules. Conversion sees one typed local.
        let mut exprs = vec![
            Expr::Let {
                name: input.clone(),
                annotation: None,
                value: Box::new(value),
                span: value_span,
            },
            Expr::Var {
                name: iterator,
                annotation: None,
                value: Box::new(Expr::Call {
                    callee: "$for_into_iter".into(),
                    type_args: vec![],
                    args: vec![Expr::Ident {
                        name: input,
                        span: value_span,
                    }],
                    span: value_span,
                }),
                span: value_span,
            },
            Expr::While {
                condition: Box::new(Expr::Literal {
                    literal: Literal::Bool(true),
                    span,
                }),
                body: Box::new(body),
                span,
            },
        ];
        if index.is_some() {
            exprs.splice(
                2..2,
                [
                    Expr::Var {
                        name: counter,
                        annotation: Some(Type::UInt64),
                        value: Box::new(Expr::Literal {
                            literal: Literal::UInt64(0),
                            span,
                        }),
                        span,
                    },
                    Expr::Var {
                        name: overflow,
                        annotation: Some(Type::Bool),
                        value: Box::new(Expr::Literal {
                            literal: Literal::Bool(false),
                            span,
                        }),
                        span,
                    },
                ],
            );
        }
        Ok(Expr::Block { exprs, span })
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
        let mut arms = Vec::new();
        let fallback = loop {
            let arm_start = self.peek_span().start();
            self.expect(&TokenKind::If)?;
            if let Some((pattern, value)) = self.parse_let_condition()? {
                let body = self.parse_block()?;
                let other = if self.peek() == &TokenKind::Else {
                    self.advance();
                    if self.peek() == &TokenKind::If {
                        Some(self.nested_expression(Self::parse_if)?)
                    } else {
                        Some(self.parse_block()?)
                    }
                } else {
                    None
                };
                let span = self.span(arm_start, self.last_span.end());
                break Some(Box::new(Self::conditional_match(
                    pattern,
                    value,
                    body,
                    other.unwrap_or(Expr::Block {
                        exprs: vec![],
                        span,
                    }),
                    span,
                )));
            }
            arms.push((self.parse_condition()?, self.parse_block()?));
            if self.peek() != &TokenKind::Else {
                break None;
            }
            self.advance();
            if self.peek() != &TokenKind::If {
                break Some(Box::new(self.parse_block()?));
            }
        };
        let span = self.span(start, self.last_span.end());
        if arms.is_empty() {
            return Ok((*fallback.unwrap()).with_span(span));
        }
        if arms.len() == 1 {
            let (condition, body) = arms.pop().unwrap();
            return Ok(Expr::If {
                condition: Box::new(condition),
                then_branch: Box::new(body),
                else_branch: fallback,
                span,
            });
        }
        Ok(Expr::IfChain {
            arms,
            fallback,
            span,
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
        let previous_newlines = std::mem::replace(&mut self.newlines, Newlines::Separate);
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            stmts.push(self.parse_expr()?);
            if !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof)
                && !self.at_expression_break()
            {
                return Err(self.error("Expected ';' or a newline between expressions".into()));
            }
        }
        self.expect(&TokenKind::RBrace)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        self.newlines = previous_newlines;
        Ok(Expr::Block {
            exprs: stmts,
            span: self.span(start, self.last_span.end()),
        })
    }

    // ── Top-level parsing ──────────────────────────────────────────────────────────
}

fn binds_name(pattern: &BindingPattern, expected: &str) -> bool {
    match pattern {
        BindingPattern::Name { name, .. } => name == expected,
        BindingPattern::Tuple { elements, .. } => elements.iter().any(|p| binds_name(p, expected)),
        BindingPattern::Record { inner, .. } => inner
            .fields
            .iter()
            .any(|f| binds_name(&f.pattern, expected)),
        BindingPattern::Sequence(p) => {
            p.prefix
                .iter()
                .chain(&p.suffix)
                .any(|p| binds_name(p, expected))
                || p.rest
                    .as_ref()
                    .is_some_and(|r| r.as_deref() == Some(expected))
        }
        BindingPattern::Discard { .. } => false,
    }
}
