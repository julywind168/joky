use super::*;

impl Parser {
    pub(super) fn expect_simple(
        &mut self,
        expected: TokenKind,
        message: &str,
    ) -> Result<Token, ParseError> {
        self.skip_newlines();
        let token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: message.to_owned(),
            span: self.previous_span(),
        })?;
        if std::mem::discriminant(&token.kind) == std::mem::discriminant(&expected) {
            Ok(token)
        } else {
            Err(ParseError::ExpectedToken {
                expected: message.to_owned(),
                span: Some(token.span),
            })
        }
    }

    pub(super) fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    pub(super) fn advance(&mut self) -> Option<Token> {
        let token = self.peek().cloned()?;
        self.position += 1;
        Some(token)
    }

    pub(super) fn previous_span(&self) -> Option<Span> {
        self.position
            .checked_sub(1)
            .and_then(|position| self.tokens.get(position))
            .map(|token| token.span)
    }

    pub(super) fn skip_newlines(&mut self) {
        while matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Newline)
        ) {
            self.advance();
        }
    }

    pub(super) fn skip_newlines_before_dot(&mut self) {
        let mut position = self.position;
        let mut found_newline = false;
        while matches!(
            self.tokens.get(position).map(|token| &token.kind),
            Some(TokenKind::Newline)
        ) {
            found_newline = true;
            position += 1;
        }
        if found_newline
            && matches!(
                self.tokens.get(position).map(|token| &token.kind),
                Some(TokenKind::Dot | TokenKind::PipeGreater)
            )
        {
            self.position = position;
        }
    }

    pub(super) fn match_token(&mut self, expected: TokenKind) -> bool {
        self.skip_newlines();
        if let Some(token) = self.peek() {
            if std::mem::discriminant(&token.kind) == std::mem::discriminant(&expected) {
                self.advance();
                return true;
            }
        }
        false
    }
}
