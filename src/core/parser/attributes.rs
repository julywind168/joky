//! Attribute syntax is structured metadata, never an ordinary function call.
use super::*;

impl Parser {
    pub(super) fn parse_attribute(&mut self, depth: usize) -> Result<Attribute, Diagnostic> {
        if depth >= 32 {
            return Err(self.error("Attribute nesting exceeds 32 levels".into()));
        }
        let start = self.peek_span().start();
        let name = self.expect_ident()?;
        let path = self.parse_dotted_name(name)?;
        let arguments = if self.peek() == &TokenKind::LParen {
            self.advance();
            let mut arguments = Vec::new();
            let mut names = std::collections::HashSet::new();
            while self.peek() != &TokenKind::RParen {
                if arguments.len() >= 64 {
                    return Err(self.error("Attribute argument count exceeds 64".into()));
                }
                let start = self.peek_span().start();
                let name = if matches!(self.peek(), TokenKind::Ident(_))
                    && self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|t| t.kind == TokenKind::Colon)
                {
                    let name = self.expect_ident()?;
                    self.advance();
                    if !names.insert(name.clone()) {
                        return Err(self.error("Duplicate named attribute argument".into()));
                    }
                    Some(name)
                } else {
                    if !names.is_empty() {
                        return Err(self.error(
                            "Positional attribute arguments must precede named arguments".into(),
                        ));
                    }
                    None
                };
                let value = match self.peek().clone() {
                    TokenKind::Ident(_) => AttributeValue::Name(self.parse_attribute(depth + 1)?),
                    TokenKind::StrLit(value) => {
                        self.advance();
                        AttributeValue::Literal(Literal::String(value))
                    }
                    TokenKind::True | TokenKind::False => {
                        let value = self.peek() == &TokenKind::True;
                        self.advance();
                        AttributeValue::Literal(Literal::Bool(value))
                    }
                    TokenKind::IntLit(_) | TokenKind::Minus => {
                        let negative = self.peek() == &TokenKind::Minus;
                        if negative {
                            self.advance();
                        }
                        let TokenKind::IntLit(magnitude) = self.peek().clone() else {
                            return Err(self.error("Expected integer attribute argument".into()));
                        };
                        self.advance();
                        AttributeValue::Literal(Literal::Integer {
                            magnitude,
                            negative,
                        })
                    }
                    _ => {
                        return Err(
                            self.error("Expected a literal or name in attribute arguments".into())
                        )
                    }
                };
                arguments.push(AttributeArgument {
                    name,
                    value,
                    span: self.span(start, self.last_span.end()),
                });
                if self.peek() != &TokenKind::Comma {
                    break;
                }
                self.advance();
            }
            self.expect(&TokenKind::RParen)?;
            Some(arguments)
        } else {
            None
        };
        Ok(Attribute {
            path,
            arguments,
            span: self.span(start, self.last_span.end()),
        })
    }
}
