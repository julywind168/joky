//! Expressions syntax.
use super::*;

impl Parser {
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
            ) || (self.peek() == &TokenKind::Pipe && !self.allow_pipe)
            {
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
        self.nested_expression(Self::parse_assignment_inner)
    }

    fn parse_assignment_inner(&mut self) -> Result<Expr, Diagnostic> {
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
                    )
                    || (self.peek() == &TokenKind::Pipe && !self.allow_pipe)
                {
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
        let lhs = self.parse_range()?;
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

    /// Bounded ranges bind below logical operators and above assignment.
    fn parse_range(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.parse_logical_or()?;
        if self.at_expression_break()
            || !matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq)
        {
            return Ok(start);
        }
        let inclusive = self.peek() == &TokenKind::DotDotEq;
        self.advance();
        let end = self.parse_logical_or()?;
        if !self.at_expression_break()
            && matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq)
        {
            return Err(self.error("Range chains require parentheses".into()));
        }
        self.iterators = true;
        let span = self.span(start.span().start(), end.span().end());
        Ok(Expr::Call {
            callee: if inclusive {
                "$joky_iter.range_inclusive"
            } else {
                "$joky_iter.range"
            }
            .into(),
            type_args: vec![],
            args: vec![start, end],
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
        while !self.at_expression_break() && self.peek() == &token {
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
        let mut lhs = operand(self)?;
        loop {
            if self.at_expression_break() {
                break;
            }
            if self.peek() == &TokenKind::Pipe && !self.allow_pipe {
                break;
            }
            let Some((_, op)) = operators.iter().find(|(token, _)| self.peek() == token) else {
                break;
            };
            self.advance();
            let rhs = operand(self)?;
            let span = self.span(lhs.span().start(), rhs.span().end());
            lhs = Expr::BinOp {
                op: op.clone(),
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
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
        self.advance();
        if op == UnaryOp::Borrow && self.peek() == &TokenKind::Mut {
            self.advance();
            op = UnaryOp::BorrowMut;
        }
        // Keep a directly negated integer as one untyped literal. A minus
        // outside parentheses is an operation and cannot legalize overflow.
        if op == UnaryOp::Neg {
            if let TokenKind::IntLit(magnitude) = self.peek().clone() {
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
        let operand = Box::new(self.nested_expression(Self::parse_unary)?);
        let span = self.span(start, operand.span().end());
        Ok(Expr::Unary { op, operand, span })
    }
}
