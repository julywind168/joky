//! Contextual state access syntax; ordinary calls named `when` remain valid.
use super::*;

impl Parser {
    pub(super) fn starts_when(&self) -> bool {
        if matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.kind),
            Some(TokenKind::StrLit(_) | TokenKind::Interpolated(_))
        ) {
            return true;
        }
        if self
            .tokens
            .get(self.pos + 1)
            .is_none_or(|t| t.kind != TokenKind::LParen)
        {
            return false;
        }
        let mut depth = 0;
        for (i, token) in self.tokens.iter().enumerate().skip(self.pos + 1) {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .tokens
                            .get(i + 1)
                            .is_some_and(|t| t.kind == TokenKind::LBrace);
                    }
                }
                _ => {}
            }
        }
        false
    }

    pub(super) fn parse_when(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        let label = if let TokenKind::StrLit(label) = self.peek().clone() {
            if label.is_empty() {
                return Err(self.error("when label must not be empty".into()));
            }
            self.advance();
            Some(label)
        } else {
            None
        };
        self.expect(&TokenKind::LParen)?;
        let mut accesses = Vec::new();
        let mut paths = Vec::new();
        while self.peek() != &TokenKind::RParen {
            let mutable = self.peek() == &TokenKind::Mut;
            if mutable {
                self.advance();
            }
            let name = self.expect_ident()?;
            if accesses.iter().any(|(n, _)| n == &name) {
                return Err(self.error("Duplicate binding in when access list".into()));
            }
            let optional = self.peek() == &TokenKind::Question;
            if optional {
                self.advance();
            }
            let path = if self.peek() == &TokenKind::Colon {
                self.advance();
                self.arrays = true;
                let ty = self.parse_type()?;
                self.expect(&TokenKind::Eq)?;
                let Expr::Index { base, index, .. } = self.parse_expr()? else {
                    return Err(self.error("when path requires directory[key]".into()));
                };
                Some(WhenPath {
                    ty,
                    directory: *base,
                    key: *index,
                    optional,
                })
            } else {
                if optional {
                    return Err(self.error("Optional when binding requires a typed path".into()));
                }
                None
            };
            accesses.push((name, mutable));
            paths.push(path);
            if self.peek() != &TokenKind::Comma {
                break;
            }
            self.advance();
        }
        self.expect(&TokenKind::RParen)?;
        if !(1..=64).contains(&accesses.len()) {
            return Err(self.error("when requires 1 to 64 domain bindings".into()));
        }
        let body = self.parse_block()?;
        let missing = if self.peek() == &TokenKind::Else {
            self.advance();
            Some(self.parse_block()?)
        } else {
            None
        };
        let alias = if matches!(self.peek(), TokenKind::Ident(name) if name == "alias") {
            self.advance();
            Some(self.parse_block()?)
        } else {
            None
        };
        let optional = paths
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.optional));
        if optional.is_some_and(|i| i + 1 != paths.len())
            || (optional.is_some() != missing.is_some())
        {
            return Err(
                self.error("Optional when path must be last and have an else branch".into())
            );
        }
        if (missing.is_some() || alias.is_some()) && accesses.len() < 2 {
            return Err(self.error("when branches require a preceding domain binding".into()));
        }
        if alias.is_some() && paths.iter().all(Option::is_none) {
            return Err(self.error("when alias branch requires an access path".into()));
        }
        Ok(Expr::When(Box::new(When {
            method: None,
            label,
            accesses,
            paths,
            missing,
            alias,
            body,
            span: self.span(start, self.last_span.end()),
        })))
    }
}
