//! Expressions syntax.
use super::*;

impl Parser<'_> {
    /// Semicolons sequence expressions at the lowest precedence. Declarations
    /// and assignment RHSs use parse_assignment so they cannot swallow the
    /// next statement; parentheses explicitly opt into a sequence there.
    pub(super) fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_sequence(false)
    }

    pub(super) fn parse_sequence(
        &mut self,
        newline_ends_sequence: bool,
    ) -> Result<Expr, Diagnostic> {
        let first = self.parse_assignment()?;
        self.parse_sequence_tail(first, newline_ends_sequence)
    }

    pub(super) fn parse_subscript(&mut self) -> Result<Expr, Diagnostic> {
        let first = self.nested_expression(|parser| parser.parse_assignment_inner(true))?;
        self.parse_sequence_tail(first, false)
    }

    fn parse_sequence_tail(
        &mut self,
        first: Expr,
        newline_ends_sequence: bool,
    ) -> Result<Expr, Diagnostic> {
        if self.peek() != &TokenKind::Semi {
            return Ok(first);
        }
        let start = first.span().start();
        let mut exprs = vec![first];
        let mut trailing = false;
        while self.peek() == &TokenKind::Semi {
            self.advance();
            trailing = true;
            // Repeated separators are empty statements. Delimiters belong to
            // the caller; they also make a trailing semicolon discard a value.
            if self.peek() == &TokenKind::Semi {
                continue;
            }
            if newline_ends_sequence && self.at_expression_break() {
                break;
            }
            if matches!(
                self.peek(),
                TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Comma
                    | TokenKind::Eof
            ) {
                break;
            }
            exprs.push(self.parse_assignment()?);
            trailing = false;
        }
        Ok(Expr::Sequence {
            exprs,
            trailing,
            span: self.span(start, self.last_span.end()),
        })
    }

    /// Assignment is right-associative, binds above sequencing, and returns Unit.
    pub(super) fn parse_assignment(&mut self) -> Result<Expr, Diagnostic> {
        self.nested_expression(|parser| parser.parse_assignment_inner(false))
    }

    fn parse_assignment_inner(&mut self, subscript: bool) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        match self.peek() {
            TokenKind::Return => {
                self.advance();
                let value = if self.at_expression_break()
                    || matches!(
                        self.peek(),
                        TokenKind::Semi
                            | TokenKind::RBrace
                            | TokenKind::RParen
                            | TokenKind::RBracket
                            | TokenKind::Comma
                            | TokenKind::Eof
                    ) {
                    None
                } else {
                    Some(Box::new(self.parse_assignment()?))
                };
                return Ok(Expr::Return {
                    value,
                    span: self.span(start, self.last_span.end()),
                });
            }
            TokenKind::Break => {
                self.advance();
                return Ok(Expr::Break {
                    span: self.last_span,
                });
            }
            TokenKind::Continue => {
                self.advance();
                return Ok(Expr::Continue {
                    span: self.last_span,
                });
            }
            _ => {}
        }
        let lhs = self.parse_range(subscript)?;
        if self.at_expression_break()
            || !matches!(self.peek(), TokenKind::Eq | TokenKind::CompoundAssign(_))
        {
            return Ok(lhs);
        }
        let op = match self.peek() {
            TokenKind::CompoundAssign(op) => Some(op.clone()),
            _ => None,
        };
        let start = lhs.span().start();
        let mut place = &lhs;
        while let Expr::Field { base, .. } = place {
            place = base;
        }
        if Self::name_path(&lhs).is_none()
            && !matches!(
                place,
                Expr::Index { .. }
                    | Expr::Unary {
                        op: UnaryOp::Deref,
                        ..
                    }
            )
        {
            return Err(self.error(
                "Assignment target must be a variable, field, index or dereferenced place".into(),
            ));
        }
        self.advance();
        let value = Box::new(self.parse_assignment()?);
        let span = self.span(start, self.last_span.end());
        Ok(match lhs {
            Expr::Ident { name, .. } => Expr::Assign {
                name,
                value,
                op,
                span,
            },
            target => Expr::FieldAssign {
                target: Box::new(target),
                op,
                value,
                span,
            },
        })
    }

    /// Ranges bind below logical operators and above assignment.
    fn parse_range(&mut self, subscript: bool) -> Result<Expr, Diagnostic> {
        let offset = self.peek_span().start();
        let start = if subscript && self.peek() == &TokenKind::DotDot {
            None
        } else {
            let start = self.parse_logical_or()?;
            if self.at_expression_break()
                || !matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq)
            {
                return Ok(start);
            }
            Some(start)
        };
        let inclusive = self.peek() == &TokenKind::DotDotEq;
        let open_end = subscript
            && !inclusive
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|t| t.kind == TokenKind::RBracket);
        let open = start.is_none() || open_end;
        if open {
            self.format_role(format::Role::OpenSlice);
        }
        self.advance();
        let end = if open_end {
            None
        } else {
            Some(self.parse_logical_or()?)
        };
        if !self.at_expression_break()
            && matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq)
        {
            return Err(self.error("Range chains require parentheses".into()));
        }
        if open && self.peek() != &TokenKind::RBracket {
            return Err(
                self.error("A slice with an omitted bound must be the entire subscript".into())
            );
        }
        self.iterators |= !open;
        let span = self.span(
            start.as_ref().map_or(offset, |e| e.span().start()),
            end.as_ref()
                .map_or(self.last_span.end(), |e| e.span().end()),
        );
        Ok(Expr::Call {
            callee: match (&start, &end) {
                (None, None) => "$joky_slice.full",
                (None, Some(_)) => "$joky_slice.to",
                (Some(_), None) => "$joky_slice.from",
                (Some(_), Some(_)) if inclusive => "$joky_iter.range_inclusive",
                (Some(_), Some(_)) => "$joky_iter.range",
            }
            .into(),
            type_args: vec![],
            args: start.into_iter().chain(end).collect(),
            span,
        })
    }

    fn parse_logical_or(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_logical_level(Self::parse_logical_and, TokenKind::OrOr, LogicalOp::Or)
    }

    fn parse_logical_and(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_logical_level(Self::parse_comparison, TokenKind::AndAnd, LogicalOp::And)
    }

    fn parse_logical_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expr, Diagnostic>,
        token: TokenKind,
        op: LogicalOp,
    ) -> Result<Expr, Diagnostic> {
        let mut lhs = operand(self)?;
        let mut depth = 1;
        while !self.at_expression_break() && self.peek() == &token {
            depth += 1;
            self.check_expression_depth(depth, self.peek_span())?;
            self.advance();
            let rhs = operand(self)?;
            let span = self.span(lhs.span().start(), rhs.span().end());
            lhs = Expr::Logical {
                op: op.clone(),
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn comparison_op(&self) -> Option<BinOp> {
        if self.at_expression_break() {
            return None;
        }
        match self.peek() {
            TokenKind::EqEq => Some(BinOp::Eq),
            TokenKind::Ne => Some(BinOp::Ne),
            TokenKind::Lt => Some(BinOp::Lt),
            TokenKind::Le => Some(BinOp::Le),
            TokenKind::Gt => Some(BinOp::Gt),
            TokenKind::Ge => Some(BinOp::Ge),
            _ => None,
        }
    }

    /// Comparisons bind below bitwise operators and arithmetic and cannot chain without parentheses.
    fn parse_comparison(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.parse_bit_or()?;
        if let Some(op) = self.comparison_op() {
            self.advance();
            let rhs = self.parse_bit_or()?;
            if self.comparison_op().is_some() {
                return Err(self.error("Comparison chains require parentheses".into()));
            }
            let span = self.span(lhs.span().start(), rhs.span().end());
            return Ok(Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            });
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_bit_xor, &[(TokenKind::Pipe, BinOp::BitOr)])
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_bit_and, &[(TokenKind::Caret, BinOp::BitXor)])
    }

    fn parse_bit_and(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_shift, &[(TokenKind::Amp, BinOp::BitAnd)])
    }

    fn parse_shift(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_additive,
            &[(TokenKind::Shl, BinOp::Shl), (TokenKind::Shr, BinOp::Shr)],
        )
    }

    fn parse_binary_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expr, Diagnostic>,
        operators: &[(TokenKind, BinOp)],
    ) -> Result<Expr, Diagnostic> {
        let first = operand(self)?;
        let mut rest = Vec::new();
        loop {
            if self.at_expression_break() {
                break;
            }
            let Some((_, op)) = operators.iter().find(|(token, _)| self.peek() == token) else {
                break;
            };
            if *op == BinOp::Sub && self.peek_line() > self.last_line {
                if let Newlines::Joined { minus } = &mut self.newlines {
                    minus.get_or_insert(self.pos);
                }
            }
            self.advance();
            let rhs = operand(self)?;
            rest.push((op.clone(), rhs));
        }
        let Some((_, last)) = rest.last() else {
            return Ok(first);
        };
        let span = self.span(first.span().start(), last.span().end());
        if rest.len() == 1 {
            let (op, rhs) = rest.pop().unwrap();
            return Ok(Expr::BinOp {
                lhs: Box::new(first),
                op,
                rhs: Box::new(rhs),
                span,
            });
        }
        Ok(Expr::BinaryChain {
            first: Box::new(first),
            rest,
            span,
        })
    }

    fn parse_additive(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_product,
            &[
                (TokenKind::Plus, BinOp::Add),
                (TokenKind::Minus, BinOp::Sub),
            ],
        )
    }

    fn parse_product(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_unary,
            &[
                (TokenKind::Star, BinOp::Mul),
                (TokenKind::Slash, BinOp::Div),
                (TokenKind::Percent, BinOp::Rem),
            ],
        )
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let mut op = match self.peek() {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            TokenKind::Tilde => UnaryOp::BitNot,
            TokenKind::Star => UnaryOp::Deref,
            TokenKind::Amp => UnaryOp::Borrow,
            _ => return self.parse_postfix(),
        };
        let start = self.peek_span().start();
        self.format_role(format::Role::Prefix);
        self.advance();
        if op == UnaryOp::Borrow && self.peek() == &TokenKind::Mut {
            self.advance();
            op = UnaryOp::BorrowMut;
        }
        // Keep a directly negated integer as one untyped literal. A minus
        // outside parentheses is an operation and cannot legalize overflow.
        if op == UnaryOp::Neg {
            if let TokenKind::IntLit(magnitude) = self.peek().clone() {
                // With a suffix, parse the positive literal and its entire
                // postfix chain as the operand, just as for a negated variable.
                let suffix = &self.tokens[self.pos + 1];
                if !Self::starts_postfix(
                    &suffix.kind,
                    matches!(self.newlines, Newlines::Separate) && suffix.line > self.peek_line(),
                ) {
                    self.advance();
                    return Ok(Expr::Literal {
                        literal: Literal::Integer {
                            magnitude,
                            negative: true,
                        },
                        span: self.span(start, self.last_span.end()),
                    });
                }
            }
        }
        let operand = Box::new(self.nested_expression(Self::parse_unary)?);
        let span = self.span(start, operand.span().end());
        Ok(Expr::Unary { op, operand, span })
    }
}
