use super::*;
use crate::syntax::CastMode;

impl Parser {
    /// Parses the `as`, `as?`, `as%` and `as|` suffix after an operand. Kept out of
    /// the expression loop body so the recursive expression frame stays small;
    /// the operand is rewritten in place to avoid a second large temporary.
    fn parse_cast_suffix_into(&mut self, left: &mut Expr) -> Result<(), ParseError> {
        self.advance();
        let mode = match self.peek().map(|token| &token.kind) {
            Some(TokenKind::Question) => {
                self.advance();
                CastMode::Checked
            }
            Some(TokenKind::Percent) => {
                self.advance();
                CastMode::Wrapping
            }
            Some(TokenKind::Pipe) => {
                self.advance();
                CastMode::Saturating
            }
            _ => CastMode::Lossless,
        };
        let target = self.parse_type_annotation()?;
        let span = left.span.merge(target.span);
        let value = std::mem::replace(left, self.make_expr(ExprKind::Boolean(false), left.span));
        *left = self.make_expr(
            ExprKind::Cast {
                value: Box::new(value),
                mode,
                target,
            },
            span,
        );
        Ok(())
    }

    /// Parses the range endpoints after `..` / `..=`. Kept out of the
    /// expression loop body so the recursive expression frame stays small.
    fn parse_range_suffix(&mut self, left: Expr, inclusive: bool) -> Result<Expr, ParseError> {
        // Endpoints include add/sub/mul/div but not the looser bitwise
        // operators, so `a .. b & c` parses as `(a .. b) & c`
        let end = self.parse_expression(super::PREC_ADD)?;
        let step = if matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Identifier(name)) if name == "by")
        {
            self.advance();
            Some(Box::new(self.parse_expression(super::PREC_ADD)?))
        } else {
            None
        };
        let span = left.span.merge(step.as_deref().unwrap_or(&end).span);
        Ok(self.make_expr(
            ExprKind::Range {
                start: Box::new(left),
                end: Box::new(end),
                step,
                inclusive,
            },
            span,
        ))
    }

    pub(super) fn parse_expression(&mut self, min_precedence: u8) -> Result<Expr, ParseError> {
        if self.expr_depth >= crate::syntax::MAX_EXPRESSION_NESTING {
            return Err(ParseError::ExpressionTooDeep {
                limit: crate::syntax::MAX_EXPRESSION_NESTING,
                span: self
                    .peek()
                    .map(|token| token.span)
                    .or_else(|| self.previous_span()),
            });
        }
        let stop_at_arm_pipe = std::mem::replace(&mut self.stop_at_arm_pipe, false);
        self.expr_depth += 1;
        let result = self.parse_expression_inner(min_precedence, stop_at_arm_pipe);
        self.expr_depth -= 1;
        self.stop_at_arm_pipe = stop_at_arm_pipe;
        result
    }

    fn parse_expression_inner(
        &mut self,
        min_precedence: u8,
        stop_at_arm_pipe: bool,
    ) -> Result<Expr, ParseError> {
        self.skip_newlines();
        let mut left = self.parse_prefix()?;

        loop {
            // Permit fluent calls and pipelines to continue on the next line.
            // Other newlines still terminate the current expression.
            self.skip_newlines_before_dot();
            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Dot)) {
                self.advance();
                let index_token = self.advance().ok_or(ParseError::ExpectedToken {
                    expected: "expected a tuple field index".to_owned(),
                    span: self.previous_span(),
                })?;
                let access = match index_token.kind {
                    TokenKind::Integer(value) => {
                        FieldAccess::Index(usize::try_from(value).map_err(|_| {
                            ParseError::ExpectedToken {
                                expected: "tuple field index is too large".to_owned(),
                                span: Some(index_token.span),
                            }
                        })?)
                    }
                    TokenKind::Identifier(name) => FieldAccess::Name(name),
                    _ => {
                        return Err(ParseError::ExpectedToken {
                            expected: "expected a tuple field name or index".to_owned(),
                            span: Some(index_token.span),
                        });
                    }
                };
                let span = left.span.merge(index_token.span);
                left = self.make_expr(
                    ExprKind::Field {
                        value: Box::new(left),
                        access,
                    },
                    span,
                );
                continue;
            }

            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::Bang | TokenKind::Question)
            ) {
                let token = self.advance().expect("postfix token was peeked");
                let span = left.span.merge(token.span);
                left = self.make_expr(
                    ExprKind::Unwrap {
                        value: Box::new(left),
                        propagate: matches!(token.kind, TokenKind::Question),
                    },
                    span,
                );
                continue;
            }

            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::Identifier(name)) if name == "as"
            ) {
                self.parse_cast_suffix_into(&mut left)?;
                continue;
            }

            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::LeftParen)
            ) {
                left = self.parse_call(left)?;
                continue;
            }

            if min_precedence == 0
                && self.suppress_trailing_closure == 0
                && self.starts_trailing_closure()
            {
                let closure = match self.peek().map(|token| &token.kind) {
                    Some(TokenKind::OrOr) => {
                        let pipe = self
                            .advance()
                            .expect("trailing closure delimiter was peeked");
                        self.parse_pipe_closure(false, pipe.span, true)?
                    }
                    Some(TokenKind::Pipe) => {
                        let pipe = self
                            .advance()
                            .expect("trailing closure delimiter was peeked");
                        self.parse_pipe_closure(false, pipe.span, false)?
                    }
                    // `{ body }` with the `||` omitted: a zero-parameter
                    // trailing closure whose return type is inferred.
                    Some(TokenKind::LeftBrace) => {
                        let brace = self.advance().expect("left brace was peeked");
                        let body = self.parse_block_after_left_brace(brace.span)?;
                        let span = body.span;
                        self.make_expr(
                            ExprKind::Closure {
                                move_capture: false,
                                parameters: Vec::new(),
                                return_type: TypeAnnotation::named("_", brace.span),
                                body: Box::new(body),
                            },
                            span,
                        )
                    }
                    _ => unreachable!("starts_trailing_closure matched"),
                };
                left = self.attach_trailing_closure(left, closure)?;
                continue;
            }

            if self.starts_legacy_type_arguments() {
                return Err(ParseError::ExpectedToken {
                    expected: "pass type values as ordinary call arguments".to_owned(),
                    span: self.peek().map(|token| token.span),
                });
            }

            if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Equal)) {
                if min_precedence > 0 {
                    break;
                }
                self.advance();
                let value = self.parse_expression(0)?;
                let span = left.span.merge(value.span);
                left = self.make_expr(
                    ExprKind::Assign {
                        target: Box::new(left),
                        value: Box::new(value),
                    },
                    span,
                );
                continue;
            }

            if let Some(operator) = self
                .peek()
                .and_then(|token| super::compound_operator(&token.kind))
            {
                if min_precedence > 0 {
                    break;
                }
                self.advance();
                let value = self.parse_expression(0)?;
                let span = left.span.merge(value.span);
                left = self.make_expr(
                    ExprKind::CompoundAssign {
                        target: Box::new(left),
                        operator,
                        value: Box::new(value),
                    },
                    span,
                );
                continue;
            }

            let Some(token) = self.peek().cloned() else {
                break;
            };
            if matches!(token.kind, TokenKind::DotDot | TokenKind::DotDotEqual) {
                if min_precedence > 2 {
                    break;
                }
                if matches!(left.kind, ExprKind::Range { .. }) {
                    return Err(ParseError::ExpectedToken {
                        expected: "range operators cannot be chained".into(),
                        span: Some(token.span),
                    });
                }
                self.advance();
                left =
                    self.parse_range_suffix(left, matches!(token.kind, TokenKind::DotDotEqual))?;
                continue;
            }
            // `||` followed by `->` or `{` is an empty-parameter pipe closure
            // used as an argument or operand, not a logical-or expression.
            if matches!(token.kind, TokenKind::OrOr) {
                let next = self.tokens.get(self.position + 1).map(|t| &t.kind);
                if matches!(next, Some(TokenKind::Arrow | TokenKind::LeftBrace)) {
                    break;
                }
            }
            if stop_at_arm_pipe && matches!(token.kind, TokenKind::Pipe) {
                break;
            }
            let Some((op, precedence)) = binary_operator(&token.kind) else {
                if !matches!(token.kind, TokenKind::PipeGreater) {
                    break;
                }
                if min_precedence > 0 {
                    break;
                }
                self.advance();
                self.skip_newlines();
                if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Echo)) {
                    let echo = self.advance().expect("peeked echo");
                    left = self.make_echo(left, echo.span);
                    continue;
                }
                let right = self.parse_expression(1)?;
                left = self.make_pipe_call(left, right)?;
                continue;
            };
            if precedence < min_precedence {
                break;
            }

            self.advance();
            let right = self.parse_expression(precedence + 1)?;
            let span = left.span.merge(right.span);
            left = self.make_expr(
                ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(left)
    }

    fn make_pipe_call(&mut self, value: Expr, target: Expr) -> Result<Expr, ParseError> {
        let span = value.span.merge(target.span);
        match &target.kind {
            ExprKind::Call { callee, arguments } => {
                let mut arguments = arguments.clone();
                arguments.insert(0, CallArgument { label: None, value });
                Ok(self.make_expr(
                    ExprKind::Call {
                        callee: callee.clone(),
                        arguments,
                    },
                    span,
                ))
            }
            ExprKind::Name(_) | ExprKind::Field { .. } => Ok(self.make_expr(
                ExprKind::Call {
                    callee: Box::new(target),
                    arguments: vec![CallArgument { label: None, value }],
                },
                span,
            )),
            _ => Err(ParseError::ExpectedExpression {
                span: Some(target.span),
            }),
        }
    }

    fn make_echo(&mut self, value: Expr, keyword: Span) -> Expr {
        let prefix = &self.source[..keyword.start() - self.source_offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        let location = self.make_expr(
            ExprKind::String(format!("{}:{line}:{column}", self.source_path)),
            keyword,
        );
        let callee = self.make_expr(ExprKind::Name("echo".into()), keyword);
        let span = keyword.merge(value.span);
        self.make_expr(
            ExprKind::Call {
                callee: Box::new(callee),
                arguments: vec![
                    CallArgument { label: None, value },
                    CallArgument {
                        label: None,
                        value: location,
                    },
                ],
            },
            span,
        )
    }

    /// Append a trailing closure as the final positional argument. `f(a) { .. }`
    /// extends the existing call, while `f { .. }` calls `f` with the closure
    /// alone.
    fn attach_trailing_closure(&mut self, callee: Expr, closure: Expr) -> Result<Expr, ParseError> {
        let span = callee.span.merge(closure.span);
        let argument = CallArgument {
            label: None,
            value: closure,
        };
        let Expr {
            id,
            kind,
            span: callee_span,
        } = callee;
        match kind {
            ExprKind::Call {
                callee,
                mut arguments,
            } => {
                arguments.push(argument);
                Ok(self.make_expr(ExprKind::Call { callee, arguments }, span))
            }
            kind @ (ExprKind::Name(_) | ExprKind::Field { .. }) => Ok(self.make_expr(
                ExprKind::Call {
                    callee: Box::new(Expr::new(id, kind, callee_span)),
                    arguments: vec![argument],
                },
                span,
            )),
            _ => Err(ParseError::ExpectedExpression {
                span: Some(callee_span),
            }),
        }
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        let token = self.advance().ok_or(ParseError::ExpectedExpression {
            span: self.previous_span(),
        })?;

        match token.kind {
            TokenKind::Integer(value) => Ok(self.make_expr(ExprKind::Integer(value), token.span)),
            TokenKind::Float(value) => Ok(self.make_expr(ExprKind::Float(value), token.span)),
            TokenKind::Duration(value) => Ok(self.make_expr(ExprKind::Duration(value), token.span)),
            // Raw and byte strings are plain literals; `{` does not trigger
            // interpolation
            TokenKind::String(literal) => {
                if literal.bytes {
                    Ok(self.make_expr(ExprKind::Bytes(literal.value), token.span))
                } else if !literal.raw && literal.value.contains('{') {
                    self.parse_interpolated_string(literal, token.span)
                } else {
                    Ok(self.make_expr(ExprKind::String(literal.value), token.span))
                }
            }
            TokenKind::True => Ok(self.make_expr(ExprKind::Boolean(true), token.span)),
            TokenKind::False => Ok(self.make_expr(ExprKind::Boolean(false), token.span)),
            TokenKind::Identifier(name) => {
                if matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Hash)) {
                    self.advance();
                    return self.parse_collection_literal(name, token.span);
                }
                Ok(self.make_expr(ExprKind::Name(name), token.span))
            }
            TokenKind::Let => self.parse_binding(token.span, false),
            TokenKind::Var => self.parse_binding(token.span, true),
            TokenKind::Echo => {
                let value = if matches!(
                    self.peek().map(|token| &token.kind),
                    Some(TokenKind::LeftParen)
                ) {
                    self.parse_prefix()?
                } else {
                    self.parse_expression(1)?
                };
                Ok(self.make_echo(value, token.span))
            }
            TokenKind::Struct => self.parse_anonymous_struct_type(token.span),
            TokenKind::Enum => self.parse_anonymous_enum_type(token.span),
            TokenKind::Fn => self.parse_closure(false, token.span),
            TokenKind::Pipe => self.parse_pipe_closure(false, token.span, false),
            TokenKind::OrOr => self.parse_pipe_closure(false, token.span, true),
            TokenKind::Move => match self.peek().map(|token| &token.kind) {
                Some(TokenKind::Pipe) => {
                    let pipe = self.advance().expect("peeked '|'");
                    self.parse_pipe_closure(true, token.span.merge(pipe.span), false)
                }
                Some(TokenKind::OrOr) => {
                    let pipe = self.advance().expect("peeked '||'");
                    self.parse_pipe_closure(true, token.span.merge(pipe.span), true)
                }
                _ => {
                    let fn_token =
                        self.expect_simple(TokenKind::Fn, "expected 'fn' after 'move'")?;
                    self.parse_closure(true, token.span.merge(fn_token.span))
                }
            },
            TokenKind::If => self.parse_if(token.span),
            TokenKind::Match => self.parse_match(token.span),
            TokenKind::For => self.parse_for(token.span, None),
            TokenKind::At => self.parse_parallel_for(token.span),
            TokenKind::While => self.parse_while(token.span),
            TokenKind::Loop => self.parse_loop(token.span),
            TokenKind::Break => self.parse_break(token.span),
            TokenKind::Abort => self.parse_abort(token.span),
            TokenKind::Continue => Ok(self.make_expr(ExprKind::Continue, token.span)),
            TokenKind::When => self.parse_when(token.span),
            TokenKind::Do => self.parse_do(token.span),
            TokenKind::Parallel => self.parse_concurrent_arms(token.span, true),
            TokenKind::Race => self.parse_concurrent_arms(token.span, false),
            TokenKind::Region => {
                let body = self.parse_block()?;
                let span = token.span.merge(body.span);
                Ok(self.make_expr(ExprKind::Region(Box::new(body)), span))
            }
            TokenKind::Branch => {
                let body = self.parse_block()?;
                let span = token.span.merge(body.span);
                Ok(self.make_expr(ExprKind::Branch(Box::new(body)), span))
            }
            TokenKind::Minus => {
                let expression = self.parse_expression(super::PREC_MUL)?;
                let span = token.span.merge(expression.span);
                Ok(self.make_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Negate,
                        expression: Box::new(expression),
                    },
                    span,
                ))
            }
            TokenKind::Tilde => {
                let expression = self.parse_expression(super::PREC_MUL)?;
                let span = token.span.merge(expression.span);
                Ok(self.make_expr(
                    ExprKind::Unary {
                        op: UnaryOp::BitNot,
                        expression: Box::new(expression),
                    },
                    span,
                ))
            }
            TokenKind::Bang => {
                let expression = self.parse_expression(super::PREC_MUL)?;
                let span = token.span.merge(expression.span);
                Ok(self.make_expr(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        expression: Box::new(expression),
                    },
                    span,
                ))
            }
            TokenKind::LeftParen => {
                if matches!(
                    self.peek().map(|token| &token.kind),
                    Some(TokenKind::RightParen)
                ) {
                    let right = self.advance().expect("peeked right parenthesis");
                    return Ok(
                        self.make_expr(ExprKind::Tuple(Vec::new()), token.span.merge(right.span))
                    );
                }
                let first = self.parse_expression(0)?;
                if !self.match_token(TokenKind::Comma) {
                    let right = self.expect_simple(TokenKind::RightParen, "missing ')'")?;
                    let mut expression = first;
                    expression.span = token.span.merge(right.span);
                    return Ok(expression);
                }
                let mut elements = vec![first];
                while !matches!(
                    self.peek().map(|token| &token.kind),
                    Some(TokenKind::RightParen)
                ) {
                    elements.push(self.parse_expression(0)?);
                    if !self.match_token(TokenKind::Comma) {
                        break;
                    }
                }
                let right = self.expect_simple(TokenKind::RightParen, "missing ')' in tuple")?;
                Ok(self.make_expr(ExprKind::Tuple(elements), token.span.merge(right.span)))
            }
            TokenKind::LeftBrace => self.parse_block_after_left_brace(token.span),
            _ => Err(ParseError::ExpectedExpression {
                span: Some(token.span),
            }),
        }
    }

    fn parse_interpolated_string(
        &mut self,
        literal: StringLiteral,
        span: crate::Span,
    ) -> Result<Expr, ParseError> {
        let StringLiteral {
            value,
            source_offsets,
            ..
        } = literal;
        let mut parts = Vec::new();
        let mut literal = String::new();
        let chars: Vec<char> = value.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            match chars[index] {
                '{' if index + 1 < chars.len() && chars[index + 1] == '{' => {
                    literal.push('{');
                    index += 2;
                }
                '{' => {
                    let expression_start = index + 1;
                    let mut depth = 1;
                    let mut cursor = expression_start;
                    while cursor < chars.len() && depth != 0 {
                        match chars[cursor] {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                        cursor += 1;
                    }
                    if depth != 0 {
                        return Err(ParseError::ExpectedExpression { span: Some(span) });
                    }
                    let expression_source: String =
                        chars[expression_start..cursor - 1].iter().collect();
                    if expression_source.trim().is_empty() {
                        return Err(ParseError::ExpectedExpression { span: Some(span) });
                    }
                    let Ok(tokens) = crate::syntax::lexer::lex(&expression_source) else {
                        return Err(ParseError::ExpectedExpression { span: Some(span) });
                    };
                    let expression_start_byte = value
                        .char_indices()
                        .nth(expression_start)
                        .map_or(value.len(), |(offset, _)| offset);
                    let tokens = Self::map_interpolation_tokens(
                        tokens,
                        &source_offsets,
                        span,
                        expression_start_byte,
                    );
                    // Embedded expressions share the surrounding generator so
                    // their node ids remain unique in the complete program.
                    let parent_id_gen = std::mem::replace(
                        &mut self.id_gen,
                        crate::syntax::ast::NodeIdGenerator::new(),
                    );
                    let mut nested = Parser::new(tokens, &self.source, &self.source_path);
                    nested.source_offset = self.source_offset;
                    nested.id_gen = parent_id_gen;
                    let parsed = nested.parse_expression(0);
                    nested.skip_newlines();
                    let has_trailing_tokens = nested.peek().is_some();
                    self.id_gen = nested.id_gen;
                    let expression = parsed?;
                    if has_trailing_tokens {
                        return Err(ParseError::ExpectedExpression { span: Some(span) });
                    }
                    parts.push((std::mem::take(&mut literal), Some(Box::new(expression))));
                    index = cursor;
                }
                '}' if index + 1 < chars.len() && chars[index + 1] == '}' => {
                    literal.push('}');
                    index += 2;
                }
                '}' => return Err(ParseError::ExpectedExpression { span: Some(span) }),
                character => {
                    literal.push(character);
                    index += 1;
                }
            }
        }
        parts.push((literal, None));
        Ok(self.make_expr(ExprKind::InterpolatedString(parts), span))
    }

    /// Maps token spans in interpolated expressions from decoded-string
    /// coordinates back to source coordinates, using the offset table the lexer
    /// records while scanning
    fn map_interpolation_tokens(
        tokens: Vec<Token>,
        offsets: &[usize],
        string_span: Span,
        expression_start_byte: usize,
    ) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut token| {
                let start = expression_start_byte.saturating_add(token.span.start());
                let end = expression_start_byte.saturating_add(token.span.end());
                let mapped_start = offsets.get(start).copied().unwrap_or(string_span.start());
                let mapped_end = offsets
                    .get(end)
                    .copied()
                    .unwrap_or(mapped_start)
                    .max(mapped_start);
                token.span = Span::new(mapped_start, mapped_end);
                token
            })
            .collect()
    }

    fn parse_anonymous_struct_type(&mut self, start: Span) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after 'struct'")?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            let field_start =
                self.expect_simple(TokenKind::Let, "expected 'let' before struct field")?;
            let name_token = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected struct field name".to_owned(),
                span: Some(field_start.span),
            })?;
            let TokenKind::Identifier(name) = name_token.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected struct field name".to_owned(),
                    span: Some(name_token.span),
                });
            };
            self.expect_simple(TokenKind::Colon, "expected ':' after struct field name")?;
            let ty = self.parse_type_annotation()?;
            let default = self.parse_optional_field_default()?;
            fields.push(StructField {
                name,
                ty,
                default,
                span: field_start.span,
            });
            self.consume_member_separator()?;
            self.skip_newlines();
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after struct fields")?;
        Ok(self.make_expr(ExprKind::AnonymousStruct { fields }, start.merge(end.span)))
    }

    fn parse_anonymous_enum_type(&mut self, start: Span) -> Result<Expr, ParseError> {
        self.expect_simple(TokenKind::LeftBrace, "expected '{' after 'enum'")?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while !matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::RightBrace)
        ) {
            let variant = self.advance().ok_or(ParseError::ExpectedToken {
                expected: "expected enum variant".to_owned(),
                span: self.previous_span(),
            })?;
            let TokenKind::Identifier(name) = variant.kind else {
                return Err(ParseError::ExpectedToken {
                    expected: "expected enum variant".to_owned(),
                    span: Some(variant.span),
                });
            };
            let fields = if self.match_token(TokenKind::LeftParen) {
                let fields = self.parse_parameters()?;
                self.expect_simple(TokenKind::RightParen, "expected ')' after enum variant")?;
                fields
            } else {
                Vec::new()
            };
            variants.push(EnumVariant {
                name,
                fields,
                span: variant.span,
            });
            self.consume_optional_member_separator();
            self.skip_newlines();
        }
        let end = self.expect_simple(TokenKind::RightBrace, "expected '}' after enum variants")?;
        Ok(self.make_expr(ExprKind::AnonymousEnum { variants }, start.merge(end.span)))
    }
}
