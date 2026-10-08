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
        while self.peek() == &TokenKind::Pipe {
            let arm_start = self.peek_span().start();
            self.advance();
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
            self.expect(&TokenKind::FatArrow)?;
            let previous_pipe = std::mem::replace(&mut self.allow_pipe, false);
            let body = self.parse_expr()?;
            self.allow_pipe = previous_pipe;
            arms.push(MatchArm {
                pattern,
                body,
                span: self.span(arm_start, self.last_span.end()),
            });
        }
        self.expect(&TokenKind::RBrace)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        self.newline_sensitive = previous_newlines;
        Ok(Expr::Match {
            value,
            arms,
            span: self.span(start, self.last_span.end()),
        })
    }

    pub(super) fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::If)?;
        let condition = Box::new(self.parse_condition()?);
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
        Ok(Expr::If {
            condition,
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
