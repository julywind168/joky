//! Atoms syntax.
use super::*;

impl Parser<'_> {
    fn parse_concurrent(&mut self, kind: ConcurrentKind) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.advance();
        self.expect(&TokenKind::LBrace)?;
        let previous_newlines = std::mem::replace(&mut self.newlines, Newlines::Separate);
        let mut branches = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            if self.peek() == &TokenKind::Pipe {
                return Err(self.error(
                    "Use commas or newlines between join/race branches; remove the leading '|'"
                        .into(),
                ));
            }
            let branch = self.parse_assignment()?;
            branches.push(branch);
            self.expect_braced_separator("join/race branches")?;
        }
        self.expect(&TokenKind::RBrace)?;
        self.newlines = previous_newlines;
        Ok(Expr::Concurrent {
            kind,
            branches,
            span: self.span(start, self.last_span.end()),
        })
    }

    /// Parse an atomic expression without binary operators.
    pub(super) fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        match self.peek().clone() {
            TokenKind::Pipe | TokenKind::OrOr | TokenKind::Move => self.parse_anonymous_function(),
            TokenKind::FloatLit(s) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Float(s),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::DurationLit(n) => {
                self.advance();
                self.durations = true;
                let span = self.span(start, self.last_span.end());
                Ok(Expr::Call {
                    callee: "$joky_time.Duration.from_nanos".into(),
                    type_args: vec![],
                    args: vec![Expr::Literal {
                        literal: Literal::UInt64(n),
                        span,
                    }],
                    span,
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
            TokenKind::SqlTemplate(parts) => {
                self.advance();
                self.parse_sql_template(parts, self.span(start, self.last_span.end()))
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
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            // `join` is contextual so ordinary functions such as string.join remain valid.
            TokenKind::Ident(ref name)
                if name == "join"
                    && self.allow_record
                    && self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|t| t.kind == TokenKind::LBrace) =>
            {
                self.parse_concurrent(ConcurrentKind::Join)
            }
            TokenKind::Ident(name)
                if name == "isolate"
                    && self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|t| t.kind == TokenKind::LBrace) =>
            {
                self.advance();
                let body = self.parse_block()?;
                Ok(Expr::Isolate {
                    body: Box::new(body),
                    span: self.span(start, self.last_span.end()),
                })
            }
            TokenKind::Race => self.parse_concurrent(ConcurrentKind::Race),
            TokenKind::Parallel => Err(self.error("Use join instead of parallel".into())),
            // scope { body }: a sequential task scope; pipe-separated branches
            // moved to `join`.
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
                    return Err(
                        self.error("Use join with comma- or newline-separated branches".into())
                    );
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
                let name = self.parse_type_inner(false)?.to_string();
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
            TokenKind::Ident(ref name) if name == "when" && self.starts_when() => self.parse_when(),
            TokenKind::Ident(name) => {
                self.advance();
                if let Some(value) = self.const_parameter_value(&name, self.last_span) {
                    return Ok(value);
                }
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
                let mut depth = 1;
                while self.peek() == &TokenKind::Dot {
                    depth += 1;
                    self.check_expression_depth(depth, self.peek_span())?;
                    self.advance();
                    let name = self.parse_member_name()?;
                    expr = Expr::Field {
                        base: Box::new(expr),
                        name,
                        type_args: Vec::new(),
                        span: self.span(start, self.last_span.end()),
                    };
                }
                let type_args =
                    if !self.at_expression_break() && self.peek() == &TokenKind::ColonColon {
                        self.advance();
                        let types = self.parse_type_arguments()?;
                        if (self.at_expression_break() || self.peek() != &TokenKind::LParen)
                            && !self.starts_record_body()
                        {
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
                if !self.at_expression_break() && self.peek() == &TokenKind::LParen {
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
                    let previous_newlines =
                        std::mem::replace(&mut self.newlines, Newlines::Separate);
                    let mut fields = Vec::new();
                    let mut base = None;
                    while self.peek() != &TokenKind::RBrace {
                        if self.peek() == &TokenKind::DotDot {
                            self.advance();
                            base = Some(Box::new(self.parse_assignment()?));
                            self.expect_braced_separator("record fields")?;
                            if self.peek() != &TokenKind::RBrace {
                                return Err(self.error("Record update base must be last".into()));
                            }
                            break;
                        }
                        if self.peek() == &TokenKind::Comma {
                            return Err(self.error("Record fields cannot be empty".into()));
                        }
                        let field_start = self.peek_span().start();
                        let name = self.expect_ident()?;
                        let value = if self.peek() == &TokenKind::Colon {
                            self.advance();
                            self.parse_assignment()?
                        } else {
                            self.const_parameter_value(&name, self.last_span)
                                .unwrap_or_else(|| Expr::Ident {
                                    name: name.clone(),
                                    span: self.last_span,
                                })
                        };
                        fields.push(FieldInit {
                            name,
                            value,
                            span: self.span(field_start, self.last_span.end()),
                        });
                        self.expect_braced_separator("record fields")?;
                    }
                    self.expect(&TokenKind::RBrace)?;
                    self.newlines = previous_newlines;
                    Ok(Expr::Record {
                        name,
                        type_args,
                        fields,
                        base,
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
            TokenKind::LBracket => self.parse_array_literal(),
            // Unit literal, parenthesized expression, or tuple.
            TokenKind::LParen => {
                self.advance();
                if self.peek() == &TokenKind::RParen {
                    self.advance();
                    return Ok(Expr::Block {
                        exprs: Vec::new(),
                        span: self.span(start, self.last_span.end()),
                    });
                }
                let previous = std::mem::replace(&mut self.allow_record, true);
                let previous_newlines =
                    std::mem::replace(&mut self.newlines, Newlines::Joined { minus: None });
                let first = self.parse_expr()?;
                let expr = if self.peek() == &TokenKind::Comma {
                    self.advance();
                    self.check_list_item()?;
                    let mut elements = vec![first];
                    while self.peek() != &TokenKind::RParen {
                        elements.push(self.parse_expr()?);
                        self.check_list_item()?;
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
                self.newlines = previous_newlines;
                Ok(expr.with_span(self.span(start, self.last_span.end())))
            }
            // Block
            TokenKind::LBrace => self.parse_block(),
            _ => Err(self.error(format!(
                "Expected an expression, got {}",
                self.found_token()
            ))),
        }
    }

    fn const_parameter_value(&self, name: &str, span: Span) -> Option<Expr> {
        self.type_params
            .iter()
            .find(|p| p.name == name && p.kind == ParameterKind::Const)
            .map(|p| Expr::Literal {
                literal: Literal::ConstParameter(p.clone()),
                span,
            })
    }

    /// A field-shaped or empty brace belongs to a constructor. Other braces
    /// remain independent block expressions. A complete name ends at a newline
    /// outside parentheses, so constructor braces must start on the same line.
    fn starts_record_body(&self) -> bool {
        if !self.allow_record || self.at_expression_break() || self.peek() != &TokenKind::LBrace {
            return false;
        }
        let Some(first) = self.tokens.get(self.pos + 1) else {
            return false;
        };
        matches!(first.kind, TokenKind::RBrace | TokenKind::DotDot)
            || (matches!(first.kind, TokenKind::Ident(_))
                && self.tokens.get(self.pos + 2).is_some_and(|next| {
                    matches!(
                        next.kind,
                        TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                    ) || next.line > first.line
                }))
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

    pub(super) fn starts_postfix(token: &TokenKind, newline: bool) -> bool {
        token == &TokenKind::Dot
            || !newline
                && matches!(
                    token,
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::Bang | TokenKind::Question
                )
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
        let mut depth = 1;
        // A leading dot explicitly continues a field or method chain across lines.
        while Self::starts_postfix(self.peek(), self.at_expression_break()) {
            depth += 1;
            self.check_expression_depth(depth, self.peek_span())?;
            if self.peek() == &TokenKind::LParen {
                let args = self.parse_call_arguments()?;
                let span = self.span(expr.span().start(), self.last_span.end());
                expr = Expr::Invoke {
                    callee: Box::new(expr),
                    args,
                    span,
                };
                continue;
            }
            if matches!(self.peek(), TokenKind::Bang | TokenKind::Question) {
                let op = if self.peek() == &TokenKind::Bang {
                    UnaryOp::Unwrap
                } else {
                    UnaryOp::Try
                };
                self.advance();
                let span = self.span(expr.span().start(), self.last_span.end());
                expr = Expr::Unary {
                    op,
                    operand: Box::new(expr),
                    span,
                };
                continue;
            }
            if self.peek() == &TokenKind::LBracket {
                self.advance();
                let record = std::mem::replace(&mut self.allow_record, true);
                let newlines =
                    std::mem::replace(&mut self.newlines, Newlines::Joined { minus: None });
                let index = self.parse_expr()?;
                self.expect(&TokenKind::RBracket)?;
                self.allow_record = record;
                self.newlines = newlines;
                let span = self.span(expr.span().start(), self.last_span.end());
                expr = Expr::Index {
                    base: Box::new(expr),
                    index: Box::new(index),
                    span,
                };
                continue;
            }
            self.advance();
            let name = self.parse_member_name()?;
            let type_args = if !self.at_expression_break() && self.peek() == &TokenKind::ColonColon
            {
                self.advance();
                self.parse_type_arguments()?
            } else {
                Vec::new()
            };
            if !self.at_expression_break() && self.peek() == &TokenKind::LParen {
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

    fn parse_member_name(&mut self) -> Result<String, Diagnostic> {
        if let TokenKind::TupleIndex(index) = self.peek() {
            let name = index.to_string();
            self.advance();
            Ok(name)
        } else {
            self.expect_ident()
        }
    }

    fn parse_anonymous_function(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        let moving = self.peek() == &TokenKind::Move;
        if moving {
            self.advance();
        }
        let empty = self.peek() == &TokenKind::OrOr;
        if empty {
            self.advance();
        } else {
            self.expect(&TokenKind::Pipe)?;
        }
        let mut params = Vec::new();
        while !empty && self.peek() != &TokenKind::Pipe {
            let span_start = self.peek_span().start();
            let mutable = self.peek() == &TokenKind::Var;
            if mutable {
                self.advance();
            }
            let name = self.expect_ident()?;
            self.check_binding_name(&name)?;
            if name == "_" || name == "self" {
                return Err(
                    self.error("Anonymous function parameters require ordinary names".into())
                );
            }
            let ty = if self.peek() == &TokenKind::Colon {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(ClosureParameter {
                mutable,
                name,
                ty,
                span: self.span(span_start, self.last_span.end()),
            });
            if self.peek() == &TokenKind::Comma {
                self.advance();
            } else if self.peek() != &TokenKind::Pipe {
                return Err(
                    self.error("Expected comma or closing pipe in closure parameters".into())
                );
            }
        }
        if !empty {
            self.expect(&TokenKind::Pipe)?;
        }
        let (suspends, noescape, names) = self.parse_fn_attrs()?;
        if noescape || !names.is_empty() {
            return Err(self.error("Only @suspends is supported on anonymous functions".into()));
        }
        let ret = if self.peek() == &TokenKind::Arrow {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(Expr::AnonymousFunction(Box::new(AnonymousFunction {
            moving,
            suspends,
            params,
            ret,
            body,
            span: self.span(start, self.last_span.end()),
        })))
    }

    fn parse_call_arguments(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        self.expect(&TokenKind::LParen)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_newlines =
            std::mem::replace(&mut self.newlines, Newlines::Joined { minus: None });
        let mut args = Vec::new();
        while self.peek() != &TokenKind::RParen {
            args.push(self.parse_expr()?);
            self.check_list_item()?;
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        self.allow_record = previous;
        self.newlines = previous_newlines;
        Ok(args)
    }
}

impl Parser<'_> {
    fn parse_array_literal(&mut self) -> Result<Expr, Diagnostic> {
        self.arrays = true;
        let start = self.peek_span().start();
        self.advance();
        let previous = std::mem::replace(&mut self.allow_record, true);
        let newlines = std::mem::replace(&mut self.newlines, Newlines::Joined { minus: None });
        let mut elements = Vec::new();
        while self.peek() != &TokenKind::RBracket {
            let value = if self.peek() == &TokenKind::DotDot {
                let start = self.peek_span().start();
                self.advance();
                let value = self.parse_assignment()?;
                Expr::Spread {
                    span: self.span(start, value.span().end()),
                    value: Box::new(value),
                }
            } else {
                self.parse_assignment()?
            };
            self.check_list_item()?;
            elements.push(value);
            if self.peek() != &TokenKind::RBracket {
                self.expect(&TokenKind::Comma)?;
            }
        }
        self.expect(&TokenKind::RBracket)?;
        self.allow_record = previous;
        self.newlines = newlines;
        Ok(Expr::Array {
            elements,
            span: self.span(start, self.last_span.end()),
        })
    }
}
