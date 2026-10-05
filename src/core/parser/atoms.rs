//! Atoms syntax.
use super::*;

impl Parser {
    /// Parse an atomic expression without binary operators.
    pub(super) fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        match self.peek().clone() {
            TokenKind::FloatLit(s) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Float(s),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Integer {
                        magnitude: n,
                        negative: false,
                    },
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Bool(true),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Bool(false),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::Interpolated(parts) => {
                self.advance();
                self.parse_interpolation(parts, self.span(start, self.last_span.end()))
            }
            TokenKind::StrLit(s) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::String(s),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::CharLit(value) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Rune(value as u32),
                    span: self.span(start, self.last_span.end()),
                })
            }
            // Both binding kinds require an initializer and allow an annotation.
            TokenKind::Let | TokenKind::Var => {
                let mutable = self.peek() == &TokenKind::Var;
                self.advance();
                let pattern = self.parse_binding_pattern()?;
                let annotation = if self.peek() == &TokenKind::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Eq)?;
                let value = self.parse_assignment()?;
                let value = Box::new(value);
                let span = self.span(start, self.last_span.end());
                let BindingPattern::Name { name, .. } = pattern else {
                    return Ok(Expr::Destructure {
                        pattern,
                        mutable,
                        annotation,
                        value,
                        span,
                    });
                };
                Ok(if mutable {
                    Expr::Var {
                        name,
                        annotation,
                        value,
                        span,
                    }
                } else {
                    Expr::Let {
                        name,
                        annotation,
                        value,
                        span,
                    }
                })
            }
            TokenKind::Unsafe => {
                self.advance();
                let Expr::Block { exprs, .. } = self.parse_block()? else {
                    unreachable!()
                };
                Ok(Expr::Unsafe {
                    exprs,
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::If => self.parse_if(),
            TokenKind::Match => self.parse_match(),
            TokenKind::While => {
                self.advance();
                let condition = Box::new(self.parse_condition()?);
                let body = Box::new(self.parse_block()?);
                Ok(Expr::While {
                    condition,
                    body,
                    span: self.span(start, self.last_span.end()),
                })
            }
            // parallel/race { | branch ... }
            TokenKind::Parallel | TokenKind::Race => {
                let kind = if self.peek() == &TokenKind::Race {
                    ScopeKind::Race
                } else {
                    ScopeKind::Join
                };
                self.advance();
                self.expect(&TokenKind::LBrace)?;
                let mut branches = Vec::new();
                while self.peek() == &TokenKind::Pipe {
                    self.advance(); // Consume the branch separator.
                    let previous_pipe = std::mem::replace(&mut self.allow_pipe, false);
                    let branch = self.parse_assignment()?;
                    self.allow_pipe = previous_pipe;
                    branches.push(branch);
                    while self.peek() == &TokenKind::Semi {
                        self.advance();
                    }
                }
                self.expect(&TokenKind::RBrace)?;
                Ok(Expr::Scope {
                    kind,
                    branches,
                    span: self.span(start, self.last_span.end()),
                })
            }
            // scope { body }: a sequential task scope; pipe-separated branches
            // moved to `parallel`.
            TokenKind::Scope => {
                if self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| t.kind == TokenKind::LBrace)
                    && self
                        .tokens
                        .get(self.pos + 2)
                        .is_some_and(|t| t.kind == TokenKind::Pipe)
                {
                    return Err(self.error("Use parallel for pipe-separated branches".into()));
                }
                self.advance();
                let body = self.parse_block()?;
                Ok(Expr::TaskScope {
                    body: Box::new(body),
                    span: self.span(start, self.last_span.end()),
                })
            }
            // branch { body }: register a child task in the enclosing scope.
            TokenKind::Branch => {
                self.advance();
                let body = self.parse_block()?;
                Ok(Expr::Branch {
                    body: Box::new(body),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::TyInt8
            | TokenKind::TyInt16
            | TokenKind::TyInt32
            | TokenKind::TyInt64
            | TokenKind::TyFloat32
            | TokenKind::TyFloat64
            | TokenKind::TyUInt8
            | TokenKind::TyUInt16
            | TokenKind::TyUInt32
            | TokenKind::TyRune
            | TokenKind::TyUInt64
            | TokenKind::TyBool
            | TokenKind::TyUnit
            | TokenKind::TyString
            | TokenKind::TyPtr => {
                let name = self.parse_type()?.to_string();
                if self.peek() != &TokenKind::Dot {
                    return Err(
                        self.error("A type expression requires an associated member".into())
                    );
                }
                Ok(Expr::Ident {
                    name,
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::Ident(name) => {
                self.advance();
                let self_application = (name == "Self")
                    .then_some(self.impl_target.as_ref())
                    .flatten()
                    .and_then(|ty| match ty {
                        Type::Applied(application) => Some(application.clone()),
                        _ => None,
                    });
                if let Some(application) = &self_application {
                    if self.peek() == &TokenKind::ColonColon {
                        return Err(self.error("Self already has its impl type arguments".into()));
                    }
                    if self.peek() == &TokenKind::Dot {
                        return Ok(Expr::GenericName(Box::new(GenericName {
                            name: application.name.clone(),
                            type_args: application.arguments.clone(),
                            span: self.span(start, self.last_span.end()),
                        })));
                    }
                }
                let name = if name == "Self" {
                    self.impl_target
                        .as_ref()
                        .map_or(name.clone(), |ty| match ty {
                            Type::Applied(application) => application.name.clone(),
                            _ => ty.to_string(),
                        })
                } else {
                    name
                };
                let mut expr = Expr::Ident {
                    name,
                    span: self.span(start, self.last_span.end()),
                };
                while self.peek() == &TokenKind::Dot {
                    self.advance();
                    let name = self.expect_ident()?;
                    expr = Expr::Field {
                        base: Box::new(expr),
                        name,
                        type_args: Vec::new(),
                        span: self.span(start, self.last_span.end()),
                    };
                }
                let type_args = if self.peek() == &TokenKind::ColonColon {
                    self.advance();
                    let types = self.parse_type_arguments()?;
                    if self.peek() != &TokenKind::LParen && !self.starts_record_body() {
                        if let Expr::Field {
                            type_args, span, ..
                        } = &mut expr
                        {
                            *type_args = types;
                            *span = self.span(start, self.last_span.end());
                            return Ok(expr);
                        }
                        let name = Self::name_path(&expr).expect("identifier path");
                        return Ok(Expr::GenericName(Box::new(GenericName {
                            name,
                            type_args: types,
                            span: self.span(start, self.last_span.end()),
                        })));
                    }
                    types
                } else {
                    self_application
                        .map_or_else(Vec::new, |application| application.arguments.clone())
                };
                if self.peek() == &TokenKind::LParen {
                    let name = Self::name_path(&expr).expect("identifier path");
                    let args = self.parse_call_arguments()?;
                    Ok(Expr::Call {
                        callee: name,
                        type_args,
                        args,
                        span: self.span(start, self.last_span.end()),
                    })
                } else if self.starts_record_body() {
                    let name = Self::name_path(&expr).expect("identifier path");
                    self.advance();
                    let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
                    let mut fields = Vec::new();
                    while self.peek() != &TokenKind::RBrace {
                        let field_start = self.peek_span().start();
                        let name = self.expect_ident()?;
                        self.expect(&TokenKind::Colon)?;
                        let value = self.parse_assignment()?;
                        fields.push(FieldInit {
                            name,
                            value,
                            span: self.span(field_start, self.last_span.end()),
                        });
                        if self.peek() == &TokenKind::Comma {
                            self.advance();
                        }
                    }
                    self.expect(&TokenKind::RBrace)?;
                    self.allow_pipe = previous_pipe;
                    Ok(Expr::Record {
                        name,
                        type_args,
                        fields,
                        span: self.span(start, self.last_span.end()),
                    })
                } else if !type_args.is_empty() {
                    // Keep Self's impl arguments even when parentheses separate
                    // the namespace from its following member access.
                    Ok(Expr::GenericName(Box::new(GenericName {
                        name: Self::name_path(&expr).expect("identifier path"),
                        type_args,
                        span: self.span(start, self.last_span.end()),
                    })))
                } else {
                    Ok(expr)
                }
            }
            // Parenthesized expression.
            TokenKind::LParen => {
                self.advance();
                let previous = std::mem::replace(&mut self.allow_record, true);
                let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
                let first = self.parse_expr()?;
                let expr = if self.peek() == &TokenKind::Comma {
                    self.advance();
                    let mut elements = vec![first];
                    while self.peek() != &TokenKind::RParen {
                        elements.push(self.parse_expr()?);
                        self.expect_list_separator()?;
                    }
                    Expr::Tuple {
                        elements,
                        span: Span::empty(start),
                    }
                } else {
                    first
                };
                self.expect(&TokenKind::RParen)?;
                self.allow_record = previous;
                self.allow_pipe = previous_pipe;
                Ok(expr.with_span(self.span(start, self.last_span.end())))
            }
            // Block
            TokenKind::LBrace => self.parse_block(),
            other => Err(self.error(format!("Expected an expression, got {:?}", other))),
        }
    }

    /// A field-shaped or empty brace belongs to a constructor. Other braces
    /// remain independent block expressions; whitespace never selects a rule.
    fn starts_record_body(&self) -> bool {
        self.allow_record
            && self.peek() == &TokenKind::LBrace
            && (self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| token.kind == TokenKind::RBrace)
                || (self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|token| matches!(token.kind, TokenKind::Ident(_)))
                    && self
                        .tokens
                        .get(self.pos + 2)
                        .is_some_and(|token| token.kind == TokenKind::Colon)))
    }

    pub(super) fn name_path(mut expr: &Expr) -> Option<String> {
        let mut segments = Vec::new();
        while let Expr::Field { base, name, .. } = expr {
            segments.push(name.as_str());
            expr = base;
        }
        let Expr::Ident { name, .. } = expr else {
            return None;
        };
        segments.push(name);
        segments.reverse();
        Some(segments.join("."))
    }

    pub(super) fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        // A graph body does not need the identifier/record atom planner on
        // its recursive path. Preserve ordinary condition bindings named graph.
        let mut expr = if self.allow_record
            && matches!(self.peek(), TokenKind::Ident(name) if name == "graph")
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| token.kind == TokenKind::LBrace)
        {
            self.parse_graph()?
        } else {
            self.parse_atom()?
        };
        while self.peek() == &TokenKind::Dot {
            self.advance();
            let name = self.expect_ident()?;
            let type_args = if self.peek() == &TokenKind::ColonColon {
                self.advance();
                self.parse_type_arguments()?
            } else {
                Vec::new()
            };
            if self.peek() == &TokenKind::LParen {
                let args = self.parse_call_arguments()?;
                let span = self.span(expr.span().start(), self.last_span.end());
                expr = Expr::MethodCall(Box::new(MethodCall {
                    receiver: expr,
                    name,
                    type_args,
                    args,
                    span,
                }));
            } else {
                let span = self.span(expr.span().start(), self.last_span.end());
                expr = Expr::Field {
                    base: Box::new(expr),
                    name,
                    type_args,
                    span,
                };
            }
        }
        Ok(expr)
    }

    fn parse_call_arguments(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        self.expect(&TokenKind::LParen)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
        let mut args = Vec::new();
        while self.peek() != &TokenKind::RParen {
            args.push(self.parse_expr()?);
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        Ok(args)
    }
}
