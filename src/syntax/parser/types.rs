use super::*;

impl Parser {
    pub(super) fn parse_type_annotation(&mut self) -> Result<TypeAnnotation, ParseError> {
        let token = self.advance().ok_or(ParseError::ExpectedToken {
            expected: "expected a type expression".to_owned(),
            span: self.previous_span(),
        })?;
        let mut annotation = match token.kind {
            TokenKind::Identifier(name) => TypeAnnotation::named(&name, token.span),
            TokenKind::Integer(value) => TypeAnnotation {
                kind: TypeExpr::Const(value),
                span: token.span,
            },
            TokenKind::TypedInteger(_, _) => {
                return Err(ParseError::ExpectedToken {
                    expected: "type-level integer constants must be unsuffixed".to_owned(),
                    span: Some(token.span),
                });
            }
            TokenKind::Fn => {
                self.expect_simple(TokenKind::LeftParen, "expected '(' after 'fn' type")?;
                let parameters = self.parse_type_argument_list(TokenKind::RightParen, false)?;
                self.expect_simple(TokenKind::Arrow, "expected '->' in function type")?;
                let result = self.parse_type_annotation()?;
                TypeAnnotation {
                    span: token.span.merge(result.span),
                    kind: TypeExpr::Function {
                        parameters,
                        result: Box::new(result),
                    },
                }
            }
            TokenKind::LeftParen => {
                let arguments = self.parse_type_argument_list(TokenKind::RightParen, false)?;
                if arguments.iter().any(|arg| arg.label.is_some()) {
                    return Err(ParseError::ExpectedToken {
                        expected: "tuple type elements cannot have labels".to_owned(),
                        span: Some(token.span),
                    });
                }
                TypeAnnotation {
                    span: token.span.merge(self.previous_span().unwrap_or(token.span)),
                    kind: TypeExpr::Tuple(arguments.into_iter().map(|arg| arg.value).collect()),
                }
            }
            _ => {
                return Err(ParseError::ExpectedToken {
                    expected: "expected a type expression".to_owned(),
                    span: Some(token.span),
                })
            }
        };
        loop {
            // A newline terminates a field/annotation; match_token otherwise
            // skips it even when the requested postfix token is absent.
            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::Newline)
            ) {
                break;
            }
            if self.match_token(TokenKind::Dot) {
                let member = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected associated type name".to_owned(),
                    span: self.previous_span(),
                })?;
                let TokenKind::Identifier(name) = member.kind else {
                    return Err(ParseError::ExpectedToken {
                        expected: "expected associated type name".to_owned(),
                        span: Some(member.span),
                    });
                };
                annotation = TypeAnnotation {
                    span: annotation.span.merge(member.span),
                    kind: TypeExpr::Member {
                        base: Box::new(annotation),
                        name,
                    },
                };
            } else {
                let closing = if self.match_token(TokenKind::LeftParen) {
                    TokenKind::RightParen
                } else {
                    break;
                };
                let arguments =
                    self.parse_type_argument_list(closing, annotation.is_name("Dyn"))?;
                annotation = TypeAnnotation {
                    span: annotation
                        .span
                        .merge(self.previous_span().unwrap_or(annotation.span)),
                    kind: TypeExpr::Apply {
                        callee: Box::new(annotation),
                        arguments,
                    },
                };
            }
        }
        Ok(annotation)
    }

    fn parse_type_argument_list(
        &mut self,
        closing: TokenKind,
        dynamic: bool,
    ) -> Result<Vec<TypeArgument>, ParseError> {
        let mut arguments = Vec::new();
        self.skip_newlines();
        if self.match_token(closing.clone()) {
            return Ok(arguments);
        }
        loop {
            let label = match (
                self.peek().map(|token| &token.kind),
                self.tokens.get(self.position + 1).map(|token| &token.kind),
            ) {
                (Some(TokenKind::Identifier(label)), Some(TokenKind::Colon)) => {
                    let label = label.clone();
                    self.advance();
                    self.advance();
                    Some(label)
                }
                _ => None,
            };
            let mut value = self.parse_type_annotation()?;
            if dynamic && arguments.is_empty() && label.is_none() {
                let start = value.span;
                let mut traits = vec![value];
                while self.match_token(TokenKind::Plus) {
                    self.skip_newlines();
                    traits.push(self.parse_type_annotation()?);
                }
                value = if traits.len() == 1 {
                    traits.pop().unwrap()
                } else {
                    TypeAnnotation {
                        span: start.merge(traits.last().unwrap().span),
                        kind: TypeExpr::TraitComposition(traits),
                    }
                };
            }
            arguments.push(TypeArgument { label, value });
            self.skip_newlines();
            if self.match_token(closing.clone()) {
                break;
            }
            self.expect_simple(TokenKind::Comma, "expected ',' between type arguments")?;
            self.skip_newlines();
            if self.match_token(closing.clone()) {
                break;
            }
        }
        Ok(arguments)
    }

    pub(super) fn parse_return_type(&mut self) -> Result<TypeAnnotation, ParseError> {
        self.parse_type_annotation()
    }
}
