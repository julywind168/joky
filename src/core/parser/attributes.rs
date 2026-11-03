//! Attribute syntax is structured metadata, never an ordinary function call.
use super::*;

impl Parser<'_> {
    pub(super) fn parse_every_attribute(
        &self,
        mut attributes: Vec<Attribute>,
    ) -> Result<(Option<std::num::NonZeroU64>, Vec<Attribute>), Diagnostic> {
        let Some(index) = attributes.iter().position(|a| a.path == "every") else {
            return Ok((None, attributes));
        };
        let attribute = attributes.remove(index);
        if !attributes.is_empty() {
            return Err(Diagnostic::at(
                self.peek_line(),
                attribute.span,
                "#every must be the only function declaration attribute",
            ));
        }
        match attribute.arguments.as_deref() {
            Some(
                [AttributeArgument {
                    name: None,
                    value: AttributeValue::Duration(nanos),
                    ..
                }],
            ) if *nanos > 0 => Ok((std::num::NonZeroU64::new(*nanos), attributes)),
            _ => Err(Diagnostic::at(
                self.peek_line(),
                attribute.span,
                "#every requires one positive Duration literal, such as #every(1s)",
            )),
        }
    }

    pub(super) fn parse_intrinsic_attribute(
        &self,
        attributes: Vec<Attribute>,
    ) -> Result<Option<Box<IntrinsicAttribute>>, Diagnostic> {
        if attributes.is_empty() {
            return Ok(None);
        }
        if attributes.len() != 1 {
            return Err(self.error("Duplicate #intrinsic attribute".into()));
        }
        let attribute = &attributes[0];
        let name = match attribute.arguments.as_deref() {
            Some(
                [AttributeArgument {
                    name: None,
                    value: AttributeValue::Literal(Literal::String(name)),
                    ..
                }],
            ) => name,
            _ => {
                return Err(Diagnostic::at(
                    self.peek_line(),
                    attribute.span,
                    "#intrinsic requires one positional string identifier",
                ))
            }
        };
        Ok(Some(Box::new(IntrinsicAttribute {
            name: name.clone(),
            has_body: false,
            span: attribute.span,
        })))
    }

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
                    TokenKind::DurationLit(nanos) => {
                        self.advance();
                        AttributeValue::Duration(nanos)
                    }
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
