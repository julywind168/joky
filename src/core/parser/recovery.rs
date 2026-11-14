//! Function recovery for editor snapshots.
use super::*;

impl Parser<'_> {
    pub(super) fn parse_body_for_editor(&mut self) -> Result<Expr, Diagnostic> {
        let from = self.pos;
        match self.parse_block() {
            Err(error) if error.code == crate::ErrorCode::SyntaxError => {
                self.recover_function(from);
                let end = self.last_span.end();
                let span = self.span(end, end);
                // Keep a valid signature available for callers without treating the
                // damaged body as a Unit return or requiring a prelude function.
                Ok(Expr::While {
                    condition: Box::new(Expr::Literal {
                        literal: Literal::Bool(true),
                        span,
                    }),
                    body: Box::new(Expr::Block {
                        exprs: Vec::new(),
                        span,
                    }),
                    span,
                })
            }
            result => result,
        }
    }

    pub(super) fn recover_function(&mut self, from: usize) {
        let mut depth = 0usize;
        let mut next = from;
        while self.tokens[next].kind != TokenKind::Eof && !self.starts_declaration(next) {
            let kind = &self.tokens[next].kind;
            next += 1;
            match kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        self.pos = next;
        self.last_span = self.tokens[next - 1].span;
        self.last_line = self.tokens[next - 1].end_line;
        self.allow_record = true;
        self.newlines = Newlines::Separate;
        self.pattern_depth = 0;
    }

    fn starts_declaration(&self, index: usize) -> bool {
        let token = &self.tokens[index];
        // A named function cannot start an expression, even on the same line.
        match &self.tokens[index..] {
            [fn_token, name, ..]
                if fn_token.kind == TokenKind::Fn && matches!(name.kind, TokenKind::Ident(_)) =>
            {
                return true;
            }
            [public, fn_token, ..]
                if public.kind == TokenKind::Pub && fn_token.kind == TokenKind::Fn =>
            {
                return true;
            }
            _ => {}
        }
        if token.line == self.tokens[index - 1].end_line {
            return false;
        }
        let declaration = match token.kind {
            TokenKind::Unsafe => self.tokens[index + 1].kind == TokenKind::Extern,
            TokenKind::Pub
            | TokenKind::Hash
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Impl
            | TokenKind::Trait
            | TokenKind::Type
            | TokenKind::Const
            | TokenKind::Extern => true,
            _ => false,
        };
        declaration
            && self.text[..token.span.start()]
                .rsplit(['\r', '\n'])
                .next()
                .is_some_and(|prefix| prefix.trim().is_empty())
    }
}
