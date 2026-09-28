//! Joky parser.
//!
//! Recursive descent from tokens to a Module AST.
pub mod ast;
mod lexer;

use self::ast::*;
use self::lexer::{Lexer, Token, TokenKind};
use crate::Span;

/// A lexical or syntactic error at a one-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    fn at(line: usize, span: impl Into<Span>, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            span: span.into(),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for Diagnostic {}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    last_span: Span,
    allow_record: bool,
    // An ungrouped pipe ends a match arm or scope branch.
    allow_pipe: bool,
    type_params: Vec<std::sync::Arc<TypeParameter>>,
    type_depth: usize,
    pattern_depth: usize,
    impl_target: Option<Type>,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            last_span: Span::empty(0),
            allow_record: true,
            allow_pipe: true,
            type_params: Vec::new(),
            type_depth: 0,
            pattern_depth: 0,
            impl_target: None,
        }
    }

    /// Create a parser from source text.
    fn from_source(source: &str) -> Result<Self, Diagnostic> {
        Ok(Parser::new(Lexer::tokenize(source)?))
    }

    // ── Parser helpers ──────────────────────────────────────────────────────────

    fn error(&self, message: String) -> Diagnostic {
        Diagnostic::at(self.peek_line(), self.peek_span(), message)
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_line(&self) -> usize {
        self.tokens[self.pos].line
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn advance(&mut self) -> &TokenKind {
        let tok = &self.tokens[self.pos];
        self.last_span = tok.span;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        &tok.kind
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), Diagnostic> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("Expected {:?}, got {:?}", expected, self.peek())))
        }
    }

    fn expect_ident(&mut self) -> Result<String, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Ident(s) => {
                if s == "Self" && self.impl_target.is_some() {
                    return Err(self.error("Self is reserved inside impl".into()));
                }
                self.advance();
                Ok(s)
            }
            other => Err(self.error(format!("Expected an identifier, got {:?}", other))),
        }
    }

    /// Continue an already-consumed identifier through `.nested` path segments.
    fn parse_dotted_name(&mut self, mut name: String) -> Result<String, Diagnostic> {
        while self.peek() == &TokenKind::Dot {
            self.advance();
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        Ok(name)
    }

    // ── Type parsing ──────────────────────────────────────────────────────────

    fn parse_type(&mut self) -> Result<Type, Diagnostic> {
        if self.type_depth >= 256 {
            return Err(self.error("Type syntax nesting exceeds 256 levels".into()));
        }
        self.type_depth += 1;
        let result = self.parse_type_inner();
        self.type_depth -= 1;
        result
    }

    fn parse_type_inner(&mut self) -> Result<Type, Diagnostic> {
        match self.peek().clone() {
            TokenKind::LParen => {
                self.advance();
                let first = self.parse_type()?;
                if self.peek() != &TokenKind::Comma {
                    self.expect(&TokenKind::RParen)?;
                    return Ok(first);
                }
                self.advance();
                let mut elements = vec![first];
                while self.peek() != &TokenKind::RParen {
                    elements.push(self.parse_type()?);
                    self.expect_list_separator()?;
                }
                self.expect(&TokenKind::RParen)?;
                Ok(Type::tuple(elements))
            }
            TokenKind::TyUInt32 => {
                self.advance();
                Ok(Type::UInt32)
            }
            TokenKind::TyUInt64 => {
                self.advance();
                Ok(Type::UInt64)
            }
            TokenKind::TyInt64 => {
                self.advance();
                Ok(Type::Int64)
            }
            TokenKind::TyInt32 => {
                self.advance();
                Ok(Type::Int32)
            }
            TokenKind::TyString => {
                self.advance();
                Ok(Type::String)
            }
            TokenKind::TyBool => {
                self.advance();
                Ok(Type::Bool)
            }
            TokenKind::TyUnit => {
                self.advance();
                Ok(Type::Unit)
            }
            TokenKind::Ident(name) => {
                self.advance();
                let name = self.parse_dotted_name(name)?;
                if name == "Self" {
                    if let Some(target) = &self.impl_target {
                        return Ok(target.clone());
                    }
                }
                if self.peek() == &TokenKind::Lt {
                    return Ok(Type::Applied(std::sync::Arc::new(TypeApplication {
                        name,
                        arguments: self.parse_type_arguments()?,
                    })));
                }
                Ok(self
                    .type_params
                    .iter()
                    .find(|p| p.name == name)
                    .map_or_else(|| Type::Named(name), |p| Type::Parameter(p.clone())))
            }
            other => Err(self.error(format!("Expected a type, got {:?}", other))),
        }
    }

    fn parse_binding_pattern(&mut self) -> Result<BindingPattern, Diagnostic> {
        if self.pattern_depth >= 256 {
            return Err(self.error("Binding pattern nesting exceeds 256 levels".into()));
        }
        self.pattern_depth += 1;
        let result = self.parse_binding_pattern_inner();
        self.pattern_depth -= 1;
        result
    }

    fn parse_binding_pattern_inner(&mut self) -> Result<BindingPattern, Diagnostic> {
        let start = self.peek_span().start();
        if self.peek() != &TokenKind::LParen {
            let name = self.expect_ident()?;
            let span = self.last_span;
            return Ok(if name == "_" {
                BindingPattern::Discard { span }
            } else {
                BindingPattern::Name { name, span }
            });
        }
        self.advance();
        let first = self.parse_binding_pattern()?;
        if self.peek() != &TokenKind::Comma {
            self.expect(&TokenKind::RParen)?;
            return Ok(first);
        }
        self.advance();
        let mut elements = vec![first];
        while self.peek() != &TokenKind::RParen {
            elements.push(self.parse_binding_pattern()?);
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        Ok(BindingPattern::Tuple {
            elements,
            span: Span::new(start, self.last_span.end()),
        })
    }

    // ── Parameter lists ──────────────────────────────────────────────────────────

    /// Require commas between list elements while allowing a trailing comma.
    fn expect_list_separator(&mut self) -> Result<(), Diagnostic> {
        match self.peek() {
            TokenKind::Comma => {
                self.advance();
                Ok(())
            }
            TokenKind::RParen => Ok(()),
            other => Err(self.error(format!("Expected ',' or ')', got {other:?}"))),
        }
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, Diagnostic> {
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &TokenKind::RParen {
            let start = self.peek_span().start();
            let name = self.expect_ident()?;
            let ty = if name == "self" && self.impl_target.is_some() {
                if !params.is_empty() || self.peek() == &TokenKind::Colon {
                    return Err(self
                        .error("A by-value self must be the first, unannotated parameter".into()));
                }
                self.impl_target.clone().unwrap()
            } else {
                self.expect(&TokenKind::Colon)?;
                self.parse_type()?
            };
            params.push(Param {
                name,
                ty,
                span: Span::new(start, self.last_span.end()),
            });
            self.expect_list_separator()?;
        }
        self.expect(&TokenKind::RParen)?;
        Ok(params)
    }

    // ── Expression parsing ────────────────────────────────────────────────────────

    /// Parse an atomic expression without binary operators.
    fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        match self.peek().clone() {
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Integer {
                        magnitude: n,
                        negative: false,
                    },
                    span: Span::new(start, self.last_span.end()),
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Bool(true),
                    span: Span::new(start, self.last_span.end()),
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Bool(false),
                    span: Span::new(start, self.last_span.end()),
                })
            }
            TokenKind::StrLit(s) => {
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::String(s),
                    span: Span::new(start, self.last_span.end()),
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
                let span = Span::new(start, self.last_span.end());
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
            TokenKind::If => self.parse_if(),
            TokenKind::Match => self.parse_match(),
            TokenKind::While => {
                self.advance();
                let condition = Box::new(self.parse_condition()?);
                let body = Box::new(self.parse_block()?);
                Ok(Expr::While {
                    condition,
                    body,
                    span: Span::new(start, self.last_span.end()),
                })
            }
            // scope { | branch ... }
            TokenKind::Scope => {
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
                    branches,
                    span: Span::new(start, self.last_span.end()),
                })
            }
            TokenKind::TyInt32
            | TokenKind::TyInt64
            | TokenKind::TyUInt32
            | TokenKind::TyUInt64
            | TokenKind::TyBool
            | TokenKind::TyUnit
            | TokenKind::TyString => {
                let name = self.parse_type()?.to_string();
                if self.peek() != &TokenKind::Dot {
                    return Err(
                        self.error("A type expression requires an associated member".into())
                    );
                }
                Ok(Expr::Ident {
                    name,
                    span: Span::new(start, self.last_span.end()),
                })
            }
            TokenKind::Ident(name) => {
                self.advance();
                let name = if name == "Self" {
                    self.impl_target
                        .as_ref()
                        .map_or(name.clone(), ToString::to_string)
                } else {
                    name
                };
                let mut expr = Expr::Ident {
                    name,
                    span: Span::new(start, self.last_span.end()),
                };
                while self.peek() == &TokenKind::Dot {
                    self.advance();
                    let name = self.expect_ident()?;
                    expr = Expr::Field {
                        base: Box::new(expr),
                        name,
                        type_args: Vec::new(),
                        span: Span::new(start, self.last_span.end()),
                    };
                }
                let type_args = if self.peek() == &TokenKind::ColonColon {
                    self.advance();
                    let types = self.parse_type_arguments()?;
                    if self.peek() != &TokenKind::LParen {
                        if let Expr::Field {
                            type_args, span, ..
                        } = &mut expr
                        {
                            *type_args = types;
                            *span = Span::new(start, self.last_span.end());
                            return Ok(expr);
                        }
                        let name = Self::name_path(&expr).expect("identifier path");
                        return Ok(Expr::GenericName(Box::new(GenericName {
                            name,
                            type_args: types,
                            span: Span::new(start, self.last_span.end()),
                        })));
                    }
                    types
                } else {
                    Vec::new()
                };
                if self.peek() == &TokenKind::LParen {
                    let name = Self::name_path(&expr).expect("identifier path");
                    self.advance();
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
                    Ok(Expr::Call {
                        callee: name,
                        type_args,
                        args,
                        span: Span::new(start, self.last_span.end()),
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
                            span: Span::new(field_start, self.last_span.end()),
                        });
                        if self.peek() == &TokenKind::Comma {
                            self.advance();
                        }
                    }
                    self.expect(&TokenKind::RBrace)?;
                    self.allow_pipe = previous_pipe;
                    Ok(Expr::Record {
                        name,
                        fields,
                        span: Span::new(start, self.last_span.end()),
                    })
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
                Ok(expr.with_span(Span::new(start, self.last_span.end())))
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

    fn name_path(mut expr: &Expr) -> Option<String> {
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

    fn parse_condition(&mut self) -> Result<Expr, Diagnostic> {
        let previous = std::mem::replace(&mut self.allow_record, false);
        let result = self.parse_assignment();
        self.allow_record = previous;
        result
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_atom()?;
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
                self.advance();
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
                let span = Span::new(expr.span().start(), self.last_span.end());
                expr = Expr::MethodCall(Box::new(MethodCall {
                    receiver: expr,
                    name,
                    type_args,
                    args,
                    span,
                }));
            } else {
                let span = Span::new(expr.span().start(), self.last_span.end());
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

    fn parse_match(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::Match)?;
        let value = Box::new(self.parse_condition()?);
        self.expect(&TokenKind::LBrace)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
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
                    let binding = self.expect_ident()?;
                    if self.peek() == &TokenKind::Comma {
                        self.advance();
                    }
                    self.expect(&TokenKind::RParen)?;
                    Some(binding)
                } else {
                    None
                };
                Pattern::Variant {
                    name,
                    binding,
                    span: Span::new(pattern_start, self.last_span.end()),
                }
            };
            self.expect(&TokenKind::FatArrow)?;
            let previous_pipe = std::mem::replace(&mut self.allow_pipe, false);
            let body = self.parse_expr()?;
            self.allow_pipe = previous_pipe;
            arms.push(MatchArm {
                pattern,
                body,
                span: Span::new(arm_start, self.last_span.end()),
            });
        }
        self.expect(&TokenKind::RBrace)?;
        self.allow_record = previous;
        self.allow_pipe = previous_pipe;
        Ok(Expr::Match {
            value,
            arms,
            span: Span::new(start, self.last_span.end()),
        })
    }

    fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
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
            span: Span::new(start, self.last_span.end()),
        })
    }

    /// Semicolons sequence expressions at the lowest precedence. Declarations
    /// and assignment RHSs use parse_assignment so they cannot swallow the
    /// next statement; parentheses explicitly opt into a sequence there.
    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        let first = self.parse_assignment()?;
        if self.peek() != &TokenKind::Semi {
            return Ok(first);
        }
        let start = first.span().start();
        let mut exprs = vec![first];
        let mut trailing = false;
        while self.peek() == &TokenKind::Semi {
            self.advance();
            trailing = true;
            // Repeated separators are empty statements. Delimiters belong to
            // the caller; they also make a trailing semicolon discard a value.
            if self.peek() == &TokenKind::Semi {
                continue;
            }
            if matches!(
                self.peek(),
                TokenKind::RParen
                    | TokenKind::RBrace
                    | TokenKind::Comma
                    | TokenKind::Pipe
                    | TokenKind::Eof
            ) {
                break;
            }
            exprs.push(self.parse_assignment()?);
            trailing = false;
        }
        Ok(Expr::Sequence {
            exprs,
            trailing,
            span: Span::new(start, self.last_span.end()),
        })
    }

    /// Assignment is right-associative, binds above sequencing, and returns Unit.
    fn parse_assignment(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        match self.peek() {
            TokenKind::Return => {
                self.advance();
                let value = if matches!(
                    self.peek(),
                    TokenKind::Semi
                        | TokenKind::RBrace
                        | TokenKind::RParen
                        | TokenKind::Comma
                        | TokenKind::Pipe
                        | TokenKind::Eof
                ) {
                    None
                } else {
                    Some(Box::new(self.parse_assignment()?))
                };
                return Ok(Expr::Return {
                    value,
                    span: Span::new(start, self.last_span.end()),
                });
            }
            TokenKind::Break => {
                self.advance();
                return Ok(Expr::Break {
                    span: self.last_span,
                });
            }
            TokenKind::Continue => {
                self.advance();
                return Ok(Expr::Continue {
                    span: self.last_span,
                });
            }
            _ => {}
        }
        let lhs = self.parse_logical_or()?;
        if self.peek() != &TokenKind::Eq {
            return Ok(lhs);
        }
        let start = lhs.span().start();
        if Self::name_path(&lhs).is_none() {
            return Err(self.error("Assignment target must be a variable or a field path".into()));
        }
        self.advance();
        let value = Box::new(self.parse_assignment()?);
        let span = Span::new(start, self.last_span.end());
        Ok(match lhs {
            Expr::Ident { name, .. } => Expr::Assign { name, value, span },
            target => Expr::FieldAssign {
                target: Box::new(target),
                value,
                span,
            },
        })
    }

    fn parse_logical_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_logical_and()?;
        while self.peek() == &TokenKind::OrOr {
            self.advance();
            let rhs = self.parse_logical_and()?;
            let span = Span::new(lhs.span().start(), rhs.span().end());
            lhs = Expr::Logical {
                op: LogicalOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_logical_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_comparison()?;
        while self.peek() == &TokenKind::AndAnd {
            self.advance();
            let rhs = self.parse_comparison()?;
            let span = Span::new(lhs.span().start(), rhs.span().end());
            lhs = Expr::Logical {
                op: LogicalOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn comparison_op(&self) -> Option<BinOp> {
        match self.peek() {
            TokenKind::EqEq => Some(BinOp::Eq),
            TokenKind::Ne => Some(BinOp::Ne),
            TokenKind::Lt => Some(BinOp::Lt),
            TokenKind::Le => Some(BinOp::Le),
            TokenKind::Gt => Some(BinOp::Gt),
            TokenKind::Ge => Some(BinOp::Ge),
            _ => None,
        }
    }

    /// Comparisons bind below bitwise operators and arithmetic and cannot chain without parentheses.
    fn parse_comparison(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.parse_bit_or()?;
        if let Some(op) = self.comparison_op() {
            self.advance();
            let rhs = self.parse_bit_or()?;
            if self.comparison_op().is_some() {
                return Err(self.error("Comparison chains require parentheses".into()));
            }
            let span = Span::new(lhs.span().start(), rhs.span().end());
            return Ok(Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            });
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_bit_xor, &[(TokenKind::Pipe, BinOp::BitOr)])
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_bit_and, &[(TokenKind::Caret, BinOp::BitXor)])
    }

    fn parse_bit_and(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(Self::parse_shift, &[(TokenKind::Amp, BinOp::BitAnd)])
    }

    fn parse_shift(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_additive,
            &[(TokenKind::Shl, BinOp::Shl), (TokenKind::Shr, BinOp::Shr)],
        )
    }

    fn parse_binary_level(
        &mut self,
        operand: fn(&mut Self) -> Result<Expr, Diagnostic>,
        operators: &[(TokenKind, BinOp)],
    ) -> Result<Expr, Diagnostic> {
        let mut lhs = operand(self)?;
        loop {
            if self.peek() == &TokenKind::Pipe && !self.allow_pipe {
                break;
            }
            let Some((_, op)) = operators.iter().find(|(token, _)| self.peek() == token) else {
                break;
            };
            self.advance();
            let rhs = operand(self)?;
            let span = Span::new(lhs.span().start(), rhs.span().end());
            lhs = Expr::BinOp {
                op: op.clone(),
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_product,
            &[
                (TokenKind::Plus, BinOp::Add),
                (TokenKind::Minus, BinOp::Sub),
            ],
        )
    }

    fn parse_product(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_binary_level(
            Self::parse_unary,
            &[
                (TokenKind::Star, BinOp::Mul),
                (TokenKind::Slash, BinOp::Div),
            ],
        )
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let op = match self.peek() {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            TokenKind::Tilde => UnaryOp::BitNot,
            _ => return self.parse_postfix(),
        };
        let start = self.peek_span().start();
        self.advance();
        // Keep a directly negated integer as one untyped literal. A minus
        // outside parentheses is an operation and cannot legalize overflow.
        if op == UnaryOp::Neg {
            if let TokenKind::IntLit(magnitude) = self.peek().clone() {
                self.advance();
                return Ok(Expr::Literal {
                    literal: Literal::Integer {
                        magnitude,
                        negative: true,
                    },
                    span: Span::new(start, self.last_span.end()),
                });
            }
        }
        let operand = Box::new(self.parse_unary()?);
        let span = Span::new(start, operand.span().end());
        Ok(Expr::Unary { op, operand, span })
    }

    /// Parse a block of expressions.
    fn parse_block(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::LBrace)?;
        let previous = std::mem::replace(&mut self.allow_record, true);
        let previous_pipe = std::mem::replace(&mut self.allow_pipe, true);
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
        Ok(Expr::Block {
            exprs: stmts,
            span: Span::new(start, self.last_span.end()),
        })
    }

    // ── Top-level parsing ──────────────────────────────────────────────────────────

    /// Parse the optional `@suspends` attribute; anything else is an error.
    fn parse_suspends_attr(&mut self) -> Result<bool, Diagnostic> {
        if self.peek() != &TokenKind::At {
            return Ok(false);
        }
        self.advance();
        let attr = self.expect_ident()?;
        if attr != "suspends" {
            return Err(self.error(format!("Unknown attribute @{}", attr)));
        }
        Ok(true)
    }

    /// Parse the optional `-> RetType`; a missing arrow means Unit.
    fn parse_ret_ty(&mut self) -> Result<Type, Diagnostic> {
        if self.peek() == &TokenKind::Arrow {
            self.advance();
            Ok(self.parse_type()?)
        } else {
            Ok(Type::Unit)
        }
    }

    fn parse_type_parameters(&mut self) -> Result<Vec<std::sync::Arc<TypeParameter>>, Diagnostic> {
        let mut type_params = Vec::<std::sync::Arc<TypeParameter>>::new();
        if self.peek() == &TokenKind::Lt {
            self.advance();
            loop {
                let span = self.peek_span();
                let parameter = self.expect_ident()?;
                if parameter == "_" || type_params.iter().any(|p| p.name == parameter) {
                    return Err(Diagnostic::at(
                        self.peek_line(),
                        span,
                        "Duplicate or reserved type parameter",
                    ));
                }
                if type_params.len() == 64 {
                    return Err(
                        self.error("A declaration may have at most 64 type parameters".into())
                    );
                }
                type_params.push(std::sync::Arc::new(TypeParameter::new(parameter, span)));
                if self.peek() != &TokenKind::Comma {
                    break;
                }
                self.advance();
                if self.peek() == &TokenKind::Gt {
                    break;
                }
            }
            self.expect(&TokenKind::Gt)?;
        }
        Ok(type_params)
    }

    // Split closing angle brackets only while parsing type syntax.
    fn expect_type_close(&mut self) -> Result<(), Diagnostic> {
        if matches!(self.peek(), TokenKind::Shr | TokenKind::Ge) {
            let remainder = if self.peek() == &TokenKind::Shr {
                TokenKind::Gt
            } else {
                TokenKind::Eq
            };
            let token = &mut self.tokens[self.pos];
            let start = token.span.start();
            self.last_span = Span::new(start, start + 1);
            token.kind = remainder;
            token.span = Span::new(start + 1, token.span.end());
            Ok(())
        } else {
            self.expect(&TokenKind::Gt)
        }
    }

    fn parse_type_arguments(&mut self) -> Result<Vec<Type>, Diagnostic> {
        self.expect(&TokenKind::Lt)?;
        let mut args = vec![self.parse_type()?];
        while self.peek() == &TokenKind::Comma {
            self.advance();
            if matches!(self.peek(), TokenKind::Gt | TokenKind::Shr | TokenKind::Ge) {
                break;
            }
            args.push(self.parse_type()?);
        }
        self.expect_type_close()?;
        Ok(args)
    }

    fn parse_fn_def(&mut self, start: usize) -> Result<FnDef, Diagnostic> {
        // The `fn` token has been consumed.
        let name = self.expect_ident()?;
        let type_params = self.parse_type_parameters()?;
        if self.impl_target.is_some() && !type_params.is_empty() {
            return Err(self.error("Generic inherent methods are not supported yet".into()));
        }
        self.type_params.clone_from(&type_params);
        let params = self.parse_params()?;
        let suspends = self.parse_suspends_attr()?;
        let ret_ty = self.parse_ret_ty()?;
        let body = self.parse_block()?;
        self.type_params.clear();
        Ok(FnDef {
            name,
            type_params,
            params,
            ret_ty,
            suspends,
            body,
            span: Span::new(start, self.last_span.end()),
        })
    }

    fn parse_extern_fn(&mut self, start: usize) -> Result<ExternFnDecl, Diagnostic> {
        // The `extern fn` tokens have been consumed.
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let suspends = self.parse_suspends_attr()?;
        let ret_ty = self.parse_ret_ty()?;
        Ok(ExternFnDecl {
            name,
            params,
            ret_ty,
            suspends,
            span: Span::new(start, self.last_span.end()),
        })
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Fn => {
                let start = self.peek_span().start();
                self.advance();
                Ok(Item::FnDef(self.parse_fn_def(start)?))
            }
            TokenKind::Extern => {
                let start = self.peek_span().start();
                self.advance();
                self.expect(&TokenKind::Fn)?;
                Ok(Item::ExternFn(self.parse_extern_fn(start)?))
            }
            other => Err(self.error(format!("Expected fn or extern fn, got {:?}", other))),
        }
    }

    fn parse_impl(&mut self) -> Result<ImplDef, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::Impl)?;
        let target = self.parse_type()?;
        let target_name = target.to_string();
        if matches!(target, Type::Applied(_) | Type::Tuple(_)) {
            return Err(self.error("impl requires a concrete named type".into()));
        }
        self.impl_target = Some(target.clone());
        self.expect(&TokenKind::LBrace)?;
        let mut methods = Vec::new();
        let mut constants = Vec::new();
        while self.peek() != &TokenKind::RBrace {
            let start = self.peek_span().start();
            match self.peek() {
                TokenKind::Fn => {
                    self.advance();
                    methods.push(self.parse_fn_def(start)?);
                }
                TokenKind::Const => {
                    self.advance();
                    let name = self.expect_ident()?;
                    self.expect(&TokenKind::Colon)?;
                    let ty = self.parse_type()?;
                    self.expect(&TokenKind::Eq)?;
                    let value = self.parse_assignment()?;
                    constants.push(AssociatedConst {
                        name,
                        ty,
                        value,
                        span: Span::new(start, self.last_span.end()),
                    });
                }
                TokenKind::Semi => {
                    self.advance();
                }
                _ => return Err(self.error("Expected fn or const in impl".into())),
            }
        }
        self.expect(&TokenKind::RBrace)?;
        self.impl_target = None;
        Ok(ImplDef {
            target,
            target_name,
            methods,
            constants,
            span: Span::new(start, self.last_span.end()),
        })
    }

    /// Parse all top-level items.
    pub fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut items = Vec::new();
        let mut imports = Vec::new();
        let mut type_aliases = Vec::new();
        let mut records = Vec::new();
        let mut enums = Vec::new();
        let mut implementations = Vec::new();
        while self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            if self.peek() == &TokenKind::Import {
                if !items.is_empty()
                    || !type_aliases.is_empty()
                    || !records.is_empty()
                    || !enums.is_empty()
                    || !implementations.is_empty()
                {
                    return Err(self.error("Imports must precede all declarations".into()));
                }
                let start = self.peek_span().start();
                self.advance();
                let mut path = self.expect_ident()?;
                while self.peek() == &TokenKind::Slash {
                    self.advance();
                    path.push('/');
                    path.push_str(&self.expect_ident()?);
                }
                let mut alias = if matches!(self.peek(), TokenKind::Ident(name) if name == "as") {
                    self.advance();
                    Some(self.expect_ident()?)
                } else {
                    None
                };
                let mut members = Vec::new();
                if self.peek() == &TokenKind::Dot {
                    self.advance();
                    self.expect(&TokenKind::LBrace)?;
                    while self.peek() != &TokenKind::RBrace {
                        let member_start = self.peek_span().start();
                        let kind = if self.peek() == &TokenKind::Type {
                            self.advance();
                            ImportMemberKind::Type
                        } else {
                            ImportMemberKind::Value
                        };
                        let name = self.expect_ident()?;
                        let member_alias = if matches!(self.peek(), TokenKind::Ident(alias) if alias == "as")
                        {
                            self.advance();
                            Some(self.expect_ident()?)
                        } else {
                            None
                        };
                        members.push(ImportMember {
                            name,
                            kind,
                            alias: member_alias,
                            span: Span::new(member_start, self.last_span.end()),
                        });
                        if self.peek() == &TokenKind::Comma {
                            self.advance();
                        } else if self.peek() != &TokenKind::RBrace {
                            return Err(
                                self.error("Expected ',' or '}' in import member list".into())
                            );
                        }
                    }
                    self.expect(&TokenKind::RBrace)?;
                }
                if alias.is_none() && matches!(self.peek(), TokenKind::Ident(name) if name == "as")
                {
                    self.advance();
                    alias = Some(self.expect_ident()?);
                }
                imports.push(Import {
                    path,
                    alias,
                    members,
                    span: Span::new(start, self.last_span.end()),
                });
            } else if self.peek() == &TokenKind::Impl {
                implementations.push(self.parse_impl()?);
            } else if self.peek() == &TokenKind::Enum {
                let start = self.peek_span().start();
                self.advance();
                let name_span = self.peek_span();
                let name = self.expect_ident()?;
                let type_params = self.parse_type_parameters()?;
                self.type_params.clone_from(&type_params);
                self.expect(&TokenKind::LBrace)?;
                let mut variants = Vec::new();
                while self.peek() != &TokenKind::RBrace {
                    let start = self.peek_span().start();
                    let name = self.expect_ident()?;
                    let payload = if self.peek() == &TokenKind::LParen {
                        self.advance();
                        let ty = self.parse_type()?;
                        if self.peek() == &TokenKind::Comma {
                            self.advance();
                        }
                        self.expect(&TokenKind::RParen)?;
                        Some(ty)
                    } else {
                        None
                    };
                    variants.push(VariantDef {
                        name,
                        payload,
                        span: Span::new(start, self.last_span.end()),
                    });
                    if self.peek() == &TokenKind::Comma {
                        self.advance();
                    }
                }
                self.expect(&TokenKind::RBrace)?;
                self.type_params.clear();
                let mut declaration = EnumDef::new(
                    name,
                    variants,
                    name_span,
                    Span::new(start, self.last_span.end()),
                );
                declaration.type_params = type_params;
                enums.push(declaration);
            } else if self.peek() == &TokenKind::Struct {
                let start = self.peek_span().start();
                self.advance();
                let name_span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(&TokenKind::LBrace)?;
                let mut fields = Vec::new();
                while self.peek() != &TokenKind::RBrace {
                    let start = self.peek_span().start();
                    let name = self.expect_ident()?;
                    self.expect(&TokenKind::Colon)?;
                    let ty = self.parse_type()?;
                    fields.push(Param {
                        name,
                        ty,
                        span: Span::new(start, self.last_span.end()),
                    });
                    if self.peek() == &TokenKind::Comma {
                        self.advance();
                    }
                }
                self.expect(&TokenKind::RBrace)?;
                records.push(RecordDef::new(
                    name,
                    fields,
                    name_span,
                    Span::new(start, self.last_span.end()),
                ));
            } else if self.peek() == &TokenKind::Type {
                let start = self.peek_span().start();
                self.advance();
                let name_span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(&TokenKind::Eq)?;
                let target_start = self.peek_span().start();
                let target = self.parse_type()?;
                type_aliases.push(TypeAlias {
                    name,
                    target,
                    name_span,
                    target_span: Span::new(target_start, self.last_span.end()),
                    span: Span::new(start, self.last_span.end()),
                });
            } else {
                items.push(self.parse_item()?);
            }
        }
        Ok(Module {
            imports,
            scope: None,
            type_aliases,
            records,
            enums,
            implementations,
            items,
        })
    }
}

// ── Convenience API ──────────────────────────────────────────────────────────────

/// Parse a source module or return a diagnostic.
pub fn parse(source: &str) -> Result<Module, Diagnostic> {
    Parser::from_source(source)?.parse_module()
}

/// Parse trusted library tokens with canonical, unspellable declaration roots.
pub(crate) fn parse_library(
    source: &str,
    span: Span,
    names: &[(&str, &str)],
) -> Result<Module, Diagnostic> {
    let mut tokens = Lexer::tokenize(source)?;
    for token in &mut tokens {
        token.span = span;
        if let TokenKind::Ident(name) = &mut token.kind {
            if let Some((_, target)) = names.iter().find(|(source, _)| *source == name) {
                *name = (*target).into();
            }
        }
    }
    Parser::new(tokens).parse_module()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_pipes_require_delimiters_for_bitwise_or() {
        for source in [
            "enum E { A B } fn f(e: E) -> Int32 { match e { | A => (1 | 2) | B => { 4 | 8 } } }",
            "enum E { A B } fn f(e: E) { match e { | A => g(1 | 2) | B => g(4 | 8) } }",
            "enum E { A B } struct R { x: Int32 } fn f(e: E) { match e { | A => R { x: 1 | 2 } | B => R { x: 4 | 8 } } }",
            "enum E { A B } fn f(e: E) { match e { | A => (1 | 2, 3 | 4) | B => (5, 6) } }",
            "enum E { A B } fn f(e: E) { match e { | A => let x = 1; (x | 2) | B => 3 } }",
            "enum E { A B } fn f(e: E) { match e { | A => if (1 | 2) == 3 { 1 | 2 } else { 0 } | B => 4 } }",
            "fn f() @suspends { scope { | g(1 | 2) | g(4 | 8); } }",
            "enum E { A B } fn f(e: E) { match e { | A => match e { | A => 1 | B => 2 } | B => 3 } }",
        ] { parse(source).unwrap(); }
        for source in [
            "enum E { A B } fn f(e: E) { match e { | A => 1 | 2 | B => 3 } }",
            "enum E { A B } fn f(e: E) { match e { | A => 1 | B } }",
            "fn f() { 1 | }",
            "fn f() { ~ }",
            "fn f() { 1 << }",
            "fn f() { 1 >> }",
            "fn f() { 1 ^ }",
            "fn f() { 1 & }",
            "fn f() { 1 < < 2 }",
            "fn f() { 1 <<< 2 }",
            "fn f() { 1 >>= 2 }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let source = "// 🙂\nfn f() -> Int64 { -0x8000_0000_0000_0000 }";
        let module = parse(source).unwrap();
        let Item::FnDef(f) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &f.body else {
            panic!()
        };
        assert!(matches!(
            exprs[0],
            Expr::Literal {
                literal: Literal::Integer {
                    magnitude: 9223372036854775808,
                    negative: true
                },
                ..
            }
        ));
        assert_eq!(
            exprs[0].span().slice(source),
            Some("-0x8000_0000_0000_0000")
        );
    }

    #[test]
    fn tuples_distinguish_grouping_lists_and_irrefutable_patterns() {
        let source = "// 🙂\nfn f(p: (Int32, Bool,)) -> (Int32,) { let (x, _): (Int32, Bool) = p; (x; x + 1,) }";
        let ast = parse(source).unwrap();
        let Item::FnDef(f) = &ast.items[0] else {
            panic!()
        };
        assert_eq!(f.params[0].ty.to_string(), "(Int32, Bool)");
        assert_eq!(f.ret_ty.to_string(), "(Int32,)");
        let Expr::Block { exprs, .. } = &f.body else {
            panic!()
        };
        let Expr::Sequence { exprs, .. } = &exprs[0] else {
            panic!()
        };
        let Expr::Destructure { pattern, value, .. } = &exprs[0] else {
            panic!()
        };
        assert_eq!(pattern.span().slice(source), Some("(x, _)"));
        assert_eq!(value.span().slice(source), Some("p"));
        let Expr::Tuple { elements, span } = &exprs[1] else {
            panic!()
        };
        assert_eq!(span.slice(source), Some("(x; x + 1,)"));
        assert!(
            matches!(&elements[0], Expr::Sequence { exprs, trailing: false, .. } if exprs.len() == 2)
        );
        for source in [
            "fn f() { let (x) = 1 let (_, (y,)) = (x, (2,)) }",
            "fn f() { var (x, (_, y)) = (1, (2, 3)) }",
            "fn f(p: ((Int32))) -> (Int32) { (p) }",
            "fn f() { let _ = 1 var _ = 2 }",
            "fn f(p: (Unit,)) -> (Unit,) { ({},) }",
            "fn f() { let pair = (R { x: 1 }, E.Value(2)) }",
            "fn f(p: (Int32, Bool)) { let (x, y) = p; (x, y) }",
        ] {
            assert!(parse(source).is_ok(), "{source}");
        }
        for source in [
            "fn f() { () }",
            "fn f(x: ()) {}",
            "type T = ()",
            "fn f() { let () = {} }",
            "fn f() { (1 2) }",
            "fn f() { (1,,2) }",
            "fn f() { (,1) }",
            "fn f(x: (Int32 Bool)) {}",
            "fn f(x: (Int32,,)) {}",
            "fn f() { let (x y) = (1,2) }",
            "fn f() { let (x,,y) = (1,2) }",
            "fn f() { let (1, y) = (1,2) }",
            "fn f() { let E.Value(x) = e }",
            "fn f() { let (E.Value(x),) = e }",
            "fn f() { let (x: Int32,) = (1,) }",
            "fn f() { let (x, ..) = (1,2) }",
            "fn f() { let (x,) }",
            "fn f() { (x, y) = (1,2) }",
            "fn f() { let p = (1,2) p.0 }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        // New recursive grammar forms reject excessive syntax nesting explicitly.
        let ty = format!("fn f(x: {}Int32{}) {{}}", "(".repeat(300), ",)".repeat(300));
        assert!(parse(&ty).unwrap_err().message.contains("256"));
        let pattern = format!(
            "fn f() {{ let {}x{} = 1 }}",
            "(".repeat(300),
            ",)".repeat(300)
        );
        assert!(parse(&pattern).unwrap_err().message.contains("256"));
    }

    #[test]
    fn enums_and_match_preserve_spans_and_sequence_boundaries() {
        let source = "// 🙂\nenum Choice { Empty, Value(Int32,), } fn f(e: Choice) -> Int32 { match e { | Choice.Empty => 0 | Value(n,) => n; n + 1 } }";
        let module = parse(source).unwrap();
        let enumeration = &module.enums[0];
        assert_eq!(enumeration.name_span.slice(source), Some("Choice"));
        assert_eq!(
            enumeration.variants[1].span.slice(source),
            Some("Value(Int32,)")
        );
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        let Expr::Match { value, arms, span } = &exprs[0] else {
            panic!()
        };
        assert_eq!(value.span().slice(source), Some("e"));
        assert_eq!(arms[0].pattern.span().slice(source), Some("Choice.Empty"));
        assert_eq!(arms[1].pattern.span().slice(source), Some("Value(n,)"));
        assert_eq!(arms[1].span.slice(source), Some("| Value(n,) => n; n + 1"));
        assert!(
            matches!(&arms[1].body, Expr::Sequence { trailing: false, exprs, .. } if exprs.len() == 2)
        );
        assert!(span.slice(source).unwrap().starts_with("match e {"));
        for source in [
            "fn f(e: E) { match e { | _ => return } }",
            "fn f(e: E) { match e { | _ => 1; } }",
            "fn f(e: E) { match (P { x: e }).x { | _ => {} } }",
            "fn f(e: E) { match e { | A => match e { | _ => 1 } | _ => 2 } }",
            "fn f(e: E) { match e { | _ => true || false } }",
            "fn f() { E.Value(1; 2) }",
        ] {
            assert!(parse(source).is_ok(), "{source}");
        }
        for source in [
            "enum E { A } import joky/vm",
            "enum E { ,A }",
            "enum E { A,, B }",
            "enum E { A() }",
            "enum E { A(Int32, Bool) }",
            "fn f() { enum E { A } }",
            "fn enum() {}",
            "fn f(match: Int32) {}",
            "fn f(e: E) { match e { A => 1 } }",
            "fn f(e: E) { match e { | A() => 1 } }",
            "fn f(e: E) { match e { | A(x, y) => x } }",
            "fn f(e: E) { match e { | A(B(x)) => x } }",
            "fn f(e: E) { match e { | A(x) if x > 0 => x } }",
            "fn f(e: E) { match e { | _ => } }",
            "fn f(e: E) { match e { | 1 => 0 } }",
            "fn f(e: E) { match e { | A = 1 } }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn records_parse_fields_places_and_condition_boundaries() {
        let source = "// 🙂\nstruct 点 { x: Int32, y: Int32 } fn f() { var p = 点 { y: 2 x: 1, } p.x = 3 if p.x == 3 {} while (点 { x: 1 y: 2 }).x < 0 {} }";
        let module = parse(source).unwrap();
        let record = &module.records[0];
        assert_eq!(record.name_span.slice(source), Some("点"));
        assert_eq!(record.fields[0].span.slice(source), Some("x: Int32"));
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert!(
            matches!(&exprs[1], Expr::FieldAssign { target, .. } if matches!(target.as_ref(), Expr::Field { name, .. } if name == "x"))
        );
        assert!(matches!(&exprs[2], Expr::If { .. }));
        assert!(matches!(&exprs[3], Expr::While { .. }));
        // A following ordinary block stays separate. An empty or field-shaped
        // brace after a name is always a constructor outside conditions.
        assert!(parse("fn f() { let x = 1 { let y = x y } }").is_ok());
        assert!(parse("fn f() { let x = 1 x {} }").is_ok());
        for source in [
            "struct P {} import joky/vm",
            "struct P { ,x: Int32 }",
            "struct P { x: Int32,, y: Int32 }",
            "struct P { x: }",
            "struct P",
            "struct P {} fn f() { P { , } }",
            "struct P {} fn f() { P { x: 1,, } }",
            "struct P {} fn f() { P { x: 1; y: 2 } }",
            "fn f() { make().x = 1 }",
            "fn f() { let p = 1 p.x(1 2) }",
            "fn f() { struct P {} }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn type_aliases_keep_source_spans_and_allow_forward_declarations() {
        let source = "// 🙂\nimport joky/vm; type 数 = Later\nfn f(x: 数,) -> Later { let y: 数 = x y } type Later = Int32; type Handle = vm.Handle";
        let module = parse(source).unwrap();
        assert_eq!(module.imports.len(), 1);
        assert_eq!(module.items.len(), 1);
        assert_eq!(module.type_aliases.len(), 3);
        for (alias, name, target, spelling) in [
            (
                &module.type_aliases[0],
                "数",
                Type::Named("Later".into()),
                "type 数 = Later",
            ),
            (
                &module.type_aliases[1],
                "Later",
                Type::Int32,
                "type Later = Int32",
            ),
            (
                &module.type_aliases[2],
                "Handle",
                Type::Named("vm.Handle".into()),
                "type Handle = vm.Handle",
            ),
        ] {
            assert_eq!(alias.name, name);
            assert_eq!(alias.target, target);
            assert_eq!(alias.name_span.slice(source), Some(name));
            assert_eq!(
                alias.target_span.slice(source),
                Some(target.to_string().as_str())
            );
            assert_eq!(alias.span.slice(source), Some(spelling));
        }
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        assert_eq!(function.params[0].ty, Type::Named("数".into()));
        assert_eq!(function.ret_ty, Type::Named("Later".into()));
    }

    #[test]
    fn malformed_aliases_and_non_top_level_aliases_are_rejected() {
        for (source, marked, message) in [
            ("type = Int32", "=", "Expected an identifier"),
            ("type Count Int32", "Int32", "Expected Eq"),
            ("type Count =", "", "Expected a type"),
            ("type Count = 1", "1", "Expected a type"),
            ("type Int32 = Bool", "Int32", "Expected an identifier"),
            ("type Count.Value = Int32", ".", "Expected Eq"),
            ("type Count<T> = Int32", "<", "Expected Eq"),
            (
                "fn f() { type Count = Int32 }",
                "type",
                "Expected an expression",
            ),
            (
                "type Count = Int32 import joky/vm",
                "import",
                "Imports must precede",
            ),
            ("fn type() {}", "type", "Expected an identifier"),
        ] {
            let error = parse(source).unwrap_err();
            assert_eq!(error.span.slice(source), Some(marked), "{source}");
            assert!(error.message.contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn lists_require_commas_and_report_the_first_unseparated_token() {
        for (body, marked, message) in [
            ("fn f(x: Int32 y: Int32) {}", "y", "Expected ',' or ')'"),
            ("extern fn f(x: Int32 y: Bool)", "y", "Expected ',' or ')'"),
            ("fn f() { g(1 2) }", "2", "Expected ',' or ')'"),
            ("fn f() { g(1; 2 3) }", "3", "Expected ',' or ')'"),
            ("fn f() { g(1; 2; 3 4) }", "4", "Expected ',' or ')'"),
            ("fn f() { g(1 数) }", "数", "Expected ',' or ')'"),
            ("fn f(,x: Int32) {}", ",", "Expected an identifier"),
            ("fn f(x: Int32,,) {}", ",", "Expected an identifier"),
            ("fn f() { g(,1) }", ",", "Expected an expression"),
            ("fn f() { g(1,,2) }", ",", "Expected an expression"),
            ("fn f() { g(1", "", "Expected ',' or ')'"),
            ("fn f(x: Int32", "", "Expected ',' or ')'"),
            ("fn f() { g(1,", "", "Expected an expression"),
        ] {
            let source = format!("// é🙂\n{body}");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.line, 2, "{source}");
            assert_eq!(error.span.slice(&source), Some(marked), "{source}");
            assert!(error.message.contains(message), "{source}: {error}");
        }
        for body in [
            "fn f(x: Int32\ny: Int32) {}",
            "fn f() { g(1\n2) }",
            "fn f(x: Int32 // comma required\ny: Int32) {}",
            "fn f() { g(1 // comma required\n2) }",
        ] {
            let error = parse(body).unwrap_err();
            assert_eq!(error.line, 2);
            assert!(error.message.contains("Expected ',' or ')'"));
        }
    }

    #[test]
    fn trailing_commas_preserve_sequences_and_optional_semicolons() {
        let source = r#"
            extern fn pick(x: Int32, y: Int32,)
            fn f(x: Int32, // A comment does not replace a comma.
                 y: Int32,) {
                let a = 1 let b = 2
                pick(a; b, pick(x, y,),)
                pick((a; b), (x; y;),)
            }
        "#;
        let module = parse(source).unwrap();
        let Item::ExternFn(external) = &module.items[0] else {
            panic!()
        };
        assert_eq!(external.params.len(), 2);
        let Item::FnDef(function) = &module.items[1] else {
            panic!()
        };
        assert_eq!(function.params.len(), 2);
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert_eq!(exprs.len(), 4);
        for (call, trailing) in [(&exprs[2], false), (&exprs[3], true)] {
            let Expr::Call { args, .. } = call else {
                panic!()
            };
            assert_eq!(args.len(), 2);
            let Expr::Sequence {
                exprs,
                trailing: false,
                ..
            } = &args[0]
            else {
                panic!()
            };
            assert_eq!(exprs.len(), 2);
            if trailing {
                assert!(matches!(&args[1], Expr::Sequence { trailing: true, .. }));
            } else {
                let Expr::Call { args, .. } = &args[1] else {
                    panic!()
                };
                assert_eq!(args.len(), 2);
            }
        }
        parse("fn empty() {} fn single(x: Int32,) { empty() single(x,) }").unwrap();
    }

    #[test]
    fn imports_precede_definitions_and_extern_declarations() {
        for declaration in ["fn f() {}", "extern fn f()", "fn f() {}; extern fn g();"] {
            let source = format!("// é🙂\n{declaration}\n; import joky/vm");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.line, 3);
            assert_eq!(error.span.slice(&source), Some("import"));
            assert_eq!(error.message, "Imports must precede all declarations");
        }
        let module = parse(
            "; // leading trivia\nimport joky/vm;;\nimport user/math\nextern fn f(); fn g() {}",
        )
        .unwrap();
        assert_eq!(module.imports.len(), 2);
        assert_eq!(module.items.len(), 2);
        assert_eq!(parse("import joky/vm;").unwrap().imports.len(), 1);
    }

    #[test]
    fn imports_support_rust_style_type_and_value_selection() {
        let source = "import joky/option.{type Option, Some as Present, None} as option fn f() {}";
        let module = parse(source).unwrap();
        let import = &module.imports[0];
        assert_eq!(import.path, "joky/option");
        assert_eq!(import.alias.as_deref(), Some("option"));
        assert_eq!(import.members.len(), 3);
        assert_eq!(import.members[0].name, "Option");
        assert_eq!(import.members[0].kind, ImportMemberKind::Type);
        assert_eq!(import.members[1].alias.as_deref(), Some("Present"));
        assert_eq!(import.members[2].kind, ImportMemberKind::Value);
        assert_eq!(
            import.span.slice(source),
            Some("import joky/option.{type Option, Some as Present, None} as option")
        );
    }

    #[test]
    fn return_preserves_newlines_grouping_and_sequence_boundaries() {
        for (body, returned) in [
            ("return\n数 + 1; 9", "数 + 1"),
            ("return (数; 2); 9", "(数; 2)"),
            ("return (数;); 9", "(数;)"),
        ] {
            let source = format!("fn f(数: Int32) {{ {body} }}");
            let module = parse(&source).unwrap();
            let Item::FnDef(function) = &module.items[0] else {
                panic!()
            };
            let Expr::Block { exprs, .. } = &function.body else {
                panic!()
            };
            let Expr::Sequence {
                exprs, trailing, ..
            } = &exprs[0]
            else {
                panic!()
            };
            assert!(!trailing);
            assert_eq!(exprs.len(), 2);
            let Expr::Return {
                value: Some(value),
                span,
            } = &exprs[0]
            else {
                panic!()
            };
            assert_eq!(value.span().slice(&source), Some(returned));
            assert_eq!(span.slice(&source), Some(body.split("; 9").next().unwrap()));
            assert_eq!(exprs[1].span().slice(&source), Some("9"));
        }
        for source in [
            "return",
            "return;",
            "(return)",
            "f(return, 1)",
            "scope { | return | f() }",
        ] {
            let mut parser = Parser::from_source(source).unwrap();
            parser.parse_expr().unwrap();
            assert_eq!(parser.peek(), &TokenKind::Eof);
        }
        for source in [
            "return", "return;", "return)", "return,", "return}", "return|",
        ] {
            let mut parser = Parser::from_source(source).unwrap();
            assert!(matches!(
                parser.parse_assignment().unwrap(),
                Expr::Return { value: None, .. }
            ));
        }
    }

    #[test]
    fn jumps_require_grouping_inside_operator_operands() {
        for body in [
            "1 + return 2",
            "true && break",
            "!continue",
            "return +",
            "break = 1",
        ] {
            let source = format!("fn f() {{ {body} }}");
            let error = parse(&source).unwrap_err();
            assert!(error.span.slice(&source).is_some(), "{error}");
        }
        for body in [
            "1 + (return 2)",
            "true && (break)",
            "!(continue)",
            "return return 2",
        ] {
            parse(&format!("fn f() {{ {body} }}")).unwrap();
        }
    }

    #[test]
    fn logical_precedence_and_else_if_spans_are_preserved() {
        let source = "fn f(数: Int32) { if 数 > 0 || false && !true { 1 } else if 数 == 0 { 2 } else { 3 } }";
        let module = parse(source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("expected function")
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!("expected block")
        };
        let Expr::If {
            condition,
            then_branch,
            else_branch,
            span,
        } = &exprs[0]
        else {
            panic!("expected if")
        };
        assert_eq!(
            span.slice(source),
            Some("if 数 > 0 || false && !true { 1 } else if 数 == 0 { 2 } else { 3 }")
        );
        assert_eq!(then_branch.span().slice(source), Some("{ 1 }"));
        let Expr::Logical {
            op: LogicalOp::Or,
            lhs,
            rhs,
            ..
        } = condition.as_ref()
        else {
            panic!("expected disjunction")
        };
        assert!(matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Gt, .. }));
        assert!(matches!(
            rhs.as_ref(),
            Expr::Logical {
                op: LogicalOp::And,
                ..
            }
        ));
        let Expr::If {
            else_branch: Some(other),
            ..
        } = else_branch.as_ref().unwrap().as_ref()
        else {
            panic!("expected else if")
        };
        assert_eq!(other.span().slice(source), Some("{ 3 }"));
    }

    #[test]
    fn incomplete_conditionals_and_logical_operators_report_located_errors() {
        for (body, message) in [
            ("if", "Expected an expression"),
            ("if true 1", "Expected LBrace"),
            ("if true {} else", "Expected LBrace"),
            ("if true {} else 1", "Expected LBrace"),
            ("if true {}; else {}", "Expected an expression"),
            ("else {}", "Expected an expression"),
            ("if true; {}", "Expected LBrace"),
            ("true &&", "Expected an expression"),
            ("|| true", "Expected an expression"),
            ("true ||| false", "Expected an expression"),
            ("scope { || f() }", "Expected RBrace"),
            ("true &", "Expected an expression"),
        ] {
            let source = format!("fn f() {{ {body} }}");
            let error = parse(&source).unwrap_err();
            assert!(error.message.contains(message), "{source}: {error}");
            assert!(error.span.slice(&source).is_some());
        }
        parse("fn f() { if true {}\nelse {} scope { | f() | f() } }").unwrap();
    }

    #[test]
    fn integer_literals_preserve_untyped_magnitudes_and_negative_spans() {
        for expression in [
            "2147483648",
            "-2147483648",
            "9223372036854775807",
            "-9223372036854775808",
            "-(2147483648)",
        ] {
            parse(&format!("fn f() -> Int64 {{ {expression} }}")).unwrap();
        }
        for spelling in ["18446744073709551616", "999999999999999999999999999999"] {
            let source = format!("// 🙂\nfn f() {{ {spelling} }}");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.line, 2);
            assert!(error.message.contains("outside UInt64 magnitude range"));
            assert_eq!(error.span.slice(&source), Some(spelling));
        }
        let source = "fn f() { -9223372036854775808 }";
        let module = parse(source).unwrap();
        let Item::FnDef(f) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &f.body else {
            panic!()
        };
        assert!(matches!(
            exprs[0],
            Expr::Literal {
                literal: Literal::Integer {
                    magnitude: 9223372036854775808,
                    negative: true
                },
                ..
            }
        ));
        assert_eq!(exprs[0].span().slice(source), Some("-9223372036854775808"));
    }

    #[test]
    fn unary_precedence_and_utf8_spans_are_preserved() {
        let source = "fn f(数: Int32) -> Int32 { 1 + -数 * --2 }";
        let module = parse(source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("expected function")
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!("expected block")
        };
        let Expr::BinOp {
            op: BinOp::Add,
            rhs,
            ..
        } = &exprs[0]
        else {
            panic!("expected addition")
        };
        let Expr::BinOp {
            op: BinOp::Mul,
            lhs,
            rhs,
            ..
        } = rhs.as_ref()
        else {
            panic!("expected product")
        };
        assert_eq!(lhs.span().slice(source), Some("-数"));
        let Expr::Unary {
            op: UnaryOp::Neg,
            operand,
            ..
        } = rhs.as_ref()
        else {
            panic!("expected negation")
        };
        assert_eq!(rhs.span().slice(source), Some("--2"));
        assert_eq!(operand.span().slice(source), Some("-2"));
    }

    #[test]
    fn comparisons_require_explicit_grouping_and_unary_operators_require_operands() {
        for expression in [
            "1 < 2 < 3",
            "1 < 2 == true",
            "true == false != true",
            "true != 1 < 2",
        ] {
            let source = format!("fn f() {{ {expression} }}");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.message, "Comparison chains require parentheses");
            assert!(matches!(error.span.slice(&source), Some("<" | "==" | "!=")));
        }
        for expression in ["(1 < 2) == true", "true == (1 < 2)", "!(true != false)"] {
            parse(&format!("fn f() -> Bool {{ {expression} }}")).unwrap();
        }
        for expression in ["-", "!", "1 + -", "(1; !)", "1 * !"] {
            let source = format!("fn f() {{ {expression} }}");
            let error = parse(&source).unwrap_err();
            assert!(error.message.contains("Expected an expression"), "{error}");
        }
    }

    #[test]
    fn semicolons_preserve_statement_boundaries_and_sequence_spans() {
        let source = "fn f() { var x = 0; x = 1; (x = x + 1; x < 4) }";
        let module = parse(source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("expected function")
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!("expected block")
        };
        let Expr::Sequence {
            exprs,
            trailing,
            span,
        } = &exprs[0]
        else {
            panic!("expected sequence")
        };
        assert!(!trailing);
        assert_eq!(
            span.slice(source),
            Some("var x = 0; x = 1; (x = x + 1; x < 4)")
        );
        assert_eq!(exprs.len(), 3);
        let Expr::Var { value, .. } = &exprs[0] else {
            panic!("expected var")
        };
        assert_eq!(value.span().slice(source), Some("0"));
        let Expr::Assign { value, .. } = &exprs[1] else {
            panic!("expected assignment")
        };
        assert_eq!(value.span().slice(source), Some("1"));
        let Expr::Sequence {
            exprs,
            trailing,
            span,
        } = &exprs[2]
        else {
            panic!("expected parenthesized sequence")
        };
        assert!(!trailing);
        assert_eq!(span.slice(source), Some("(x = x + 1; x < 4)"));
        assert_eq!(exprs[0].span().slice(source), Some("x = x + 1"));
        assert_eq!(exprs[1].span().slice(source), Some("x < 4"));
    }

    #[test]
    fn semicolons_stop_at_call_delimiters_and_allow_top_level_separators() {
        let source =
            ";import joky/vm; extern fn pick(x: Unit, y: Int32); fn f() { pick((1;), (2; 3)); };;";
        let module = parse(source).unwrap();
        assert_eq!(module.imports.len(), 1);
        assert_eq!(module.items.len(), 2);
        assert_eq!(module.imports[0].span.slice(source), Some("import joky/vm"));
        let Item::FnDef(function) = &module.items[1] else {
            panic!("expected function")
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!("expected block")
        };
        let Expr::Sequence {
            exprs,
            trailing: true,
            ..
        } = &exprs[0]
        else {
            panic!("expected trailing semicolon")
        };
        let Expr::Call { args, .. } = &exprs[0] else {
            panic!("expected call")
        };
        assert_eq!(args.len(), 2);
        assert!(matches!(&args[0], Expr::Sequence { trailing: true, .. }));
        assert!(matches!(
            &args[1],
            Expr::Sequence {
                trailing: false,
                ..
            }
        ));
    }

    #[test]
    fn incomplete_semicolon_expressions_report_located_errors() {
        for (source, message) in [
            ("fn f() { var x = ; }", "Expected an expression"),
            ("fn f() { var x = 0; x = ; }", "Expected an expression"),
            ("fn f() { (1; + 2) }", "Expected an expression"),
            ("fn f() { (1;", "Expected RParen"),
            ("fn f() { 1;", "Expected RBrace"),
            ("fn f() { while true; {} }", "Expected LBrace"),
            ("fn f(x: Int32; y: Int32) {}", "Expected ',' or ')'"),
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.message.contains(message), "{source}: {error}");
            assert!(error.span.slice(source).is_some());
        }
    }

    #[test]
    fn mutable_bindings_loops_and_assignments_preserve_utf8_spans() {
        let source = "fn f() { var 数: Int32 = 0 while 数 < 2 + 1 { 数 = 数 + 1 } }";
        let module = parse(source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("expected function")
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!("expected block")
        };
        assert!(matches!(
            &exprs[0],
            Expr::Var {
                annotation: Some(Type::Int32),
                ..
            }
        ));
        assert_eq!(exprs[0].span().slice(source), Some("var 数: Int32 = 0"));
        let Expr::While {
            condition,
            body,
            span,
        } = &exprs[1]
        else {
            panic!("expected while")
        };
        assert_eq!(span.slice(source), Some("while 数 < 2 + 1 { 数 = 数 + 1 }"));
        let Expr::BinOp {
            op: BinOp::Lt, rhs, ..
        } = condition.as_ref()
        else {
            panic!("expected comparison")
        };
        assert!(matches!(rhs.as_ref(), Expr::BinOp { op: BinOp::Add, .. }));
        let Expr::Block { exprs, .. } = body.as_ref() else {
            panic!("expected loop body")
        };
        assert!(matches!(&exprs[0], Expr::Assign { name, .. } if name == "数"));
        assert_eq!(exprs[0].span().slice(source), Some("数 = 数 + 1"));
    }

    #[test]
    fn malformed_mutable_bindings_and_loops_return_parse_errors() {
        for (source, message) in [
            ("fn f() { var x }", "Expected Eq"),
            ("fn f() { var x: Int32 }", "Expected Eq"),
            ("fn f() { var x = }", "Expected an expression"),
            ("fn f() { while true 1 }", "Expected LBrace"),
            ("fn f() { while }", "Expected an expression"),
            ("fn f() { 1 = 2 }", "Assignment target must be a variable"),
            ("fn f() { f() = 2 }", "Assignment target must be a variable"),
            ("fn f() { var x = 0 x = }", "Expected an expression"),
        ] {
            let error = parse(source).unwrap_err();
            assert!(error.message.contains(message), "{source}: {error}");
            assert!(error.span.slice(source).is_some());
        }
    }

    #[test]
    fn malformed_input_returns_located_diagnostics() {
        for (source, line, message) in [
            ("\nfn f() { # }", 2, "Unexpected character"),
            (
                "\n\nfn f() { 18446744073709551616 }",
                3,
                "outside UInt64 magnitude range",
            ),
            ("\nfn f() { \"unfinished", 2, "Unterminated string literal"),
            ("\nfn f() { \"unfinished\\", 2, "Unterminated string escape"),
            ("fn f() {\nlet x =\n}", 3, "Expected an expression"),
            ("fn f() {\n", 2, "Expected RBrace"),
        ] {
            let diagnostic = parse(source).unwrap_err();
            assert_eq!(diagnostic.line, line, "{source:?}");
            assert!(diagnostic.message.contains(message), "{diagnostic}");
            assert!(diagnostic.to_string().contains(&line.to_string()));
        }
    }

    #[test]
    fn parses_add_function() {
        let src = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
        let module = parse(src).expect("Parsing failed");
        assert_eq!(module.items.len(), 1);
        match &module.items[0] {
            Item::FnDef(f) => {
                assert_eq!(f.name, "add");
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.ret_ty, Type::Int32);
                assert!(!f.suspends);
            }
            _ => panic!("Expected FnDef"),
        }
    }

    #[test]
    fn parses_extern_function() {
        let src = "extern fn host_multiply(x: Int32, y: Int32) -> Int32";
        let module = parse(src).expect("Parsing failed");
        match &module.items[0] {
            Item::ExternFn(e) => {
                assert_eq!(e.name, "host_multiply");
                assert_eq!(e.ret_ty, Type::Int32);
            }
            _ => panic!("Expected ExternFn"),
        }
    }

    #[test]
    fn preserves_stable_spans_for_imports_and_parameters() {
        let source = "import joky/vm fn add(x: Int32, y: Bool) -> Int32 { x + 1 }";
        let module = parse(source).unwrap();
        assert_eq!(module.imports[0].path, "joky/vm");
        assert_eq!(module.imports[0].span.slice(source), Some("import joky/vm"));
        let Item::FnDef(function) = &module.items[0] else {
            panic!("expected function definition");
        };
        assert_eq!(function.params[0].span.slice(source), Some("x: Int32"));
        assert_eq!(function.params[1].span.slice(source), Some("y: Bool"));
    }

    #[test]
    fn parses_suspending_function() {
        let src = "fn delayed_add(x: Int32, y: Int32) @suspends -> Int32 { x + y }";
        let module = parse(src).expect("Parsing failed");
        match &module.items[0] {
            Item::FnDef(f) => {
                assert!(f.suspends);
                assert_eq!(f.ret_ty, Type::Int32);
            }
            _ => panic!("Expected FnDef"),
        }
    }

    #[test]
    fn parses_multiple_items() {
        let src = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
extern fn host_multiply(x: Int32, y: Int32) -> Int32
"#;
        let module = parse(src).expect("Parsing failed");
        assert_eq!(module.items.len(), 2);
    }

    #[test]
    fn preserves_nested_expression_spans() {
        let source = "fn main() { let value = 1 + 2 }";
        let module = parse(source).expect("Parsing failed");
        let Item::FnDef(function) = &module.items[0] else {
            panic!("Expected function definition");
        };
        assert_eq!(function.span, Span::new(0, source.len()));
        let Expr::Block { exprs, span } = &function.body else {
            panic!("Expected function block");
        };
        assert_eq!(span, &Span::new(source.find('{').unwrap(), source.len()));
        let Expr::Let { value, span, .. } = &exprs[0] else {
            panic!("Expected let expression");
        };
        assert_eq!(span.slice(source), Some("let value = 1 + 2"));
        let Expr::BinOp { lhs, rhs, span, .. } = value.as_ref() else {
            panic!("Expected binary expression");
        };
        assert_eq!(span.slice(source), Some("1 + 2"));
        assert_eq!(lhs.span().slice(source), Some("1"));
        assert_eq!(rhs.span().slice(source), Some("2"));
    }
}

#[cfg(test)]
mod generic_tests {
    use super::*;

    #[test]
    fn generic_parameters_have_function_local_identity_and_explicit_calls_keep_spans() {
        let source = "// 🙂\nfn id<T,>(x: T) -> T { x } fn next<T>(x: T) -> T { id::<T,>(x) } fn run() { (id::<(Int64, Bool),>((1, true))) 8 >> 1 1 < 2 }";
        let module = parse(source).unwrap();
        let Item::FnDef(first) = &module.items[0] else {
            panic!()
        };
        let Item::FnDef(second) = &module.items[1] else {
            panic!()
        };
        assert_ne!(first.params[0].ty, second.params[0].ty);
        assert_eq!(first.ret_ty, first.params[0].ty);
        assert_eq!(second.ret_ty, second.params[0].ty);
        let Item::FnDef(run) = &module.items[2] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &run.body else {
            panic!()
        };
        let Expr::Call {
            type_args, span, ..
        } = &exprs[0]
        else {
            panic!()
        };
        assert_eq!(type_args, &[Type::tuple(vec![Type::Int64, Type::Bool])]);
        assert_eq!(
            span.slice(source),
            Some("(id::<(Int64, Bool),>((1, true)))")
        );
        assert!(matches!(exprs[1], Expr::BinOp { op: BinOp::Shr, .. }));
        assert!(matches!(exprs[2], Expr::BinOp { op: BinOp::Lt, .. }));
    }

    #[test]
    fn generic_syntax_rejects_empty_duplicate_unseparated_and_unsupported_parameters() {
        for source in [
            "fn f<>() {}",
            "fn f<T,T>() {}",
            "fn f<_>() {}",
            "fn f<T U>() {}",
            "fn f<T: Eq>(x: T) {}",
            "extern fn f<T>(x: T)",
            "struct R<T> {}",
            "type X<T> = T",
            "fn f() { id::<>() }",
            "fn f() { id::<Int32 Bool>(1) }",
            "fn f() { id::<Int32, ,>(1) }",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let parameters = (0..65)
            .map(|i| format!("T{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&format!("fn f<{parameters}>() {{}}"))
            .unwrap_err()
            .message
            .contains("64 type parameters"));
    }
}

#[cfg(test)]
mod generic_enum_tests {
    use super::*;

    #[test]
    fn generic_enum_type_applications_split_closers_without_changing_shift_tokens() {
        let source = "// 🙂\nenum Option<T,> { None Some(T) } fn f(x: Option<Option<Int32>>) { let a: Option<Option<Int32>>=x Option.None::<Option<Int32>> Option.Some::<Option<Int32>,>(x) 8 >> 1 8 >= 1 }";
        let module = parse(source).unwrap();
        let definition = &module.enums[0];
        assert_eq!(definition.type_params.len(), 1);
        assert_eq!(definition.type_params[0].span.slice(source), Some("T"));
        assert!(
            matches!(&definition.variants[1].payload, Some(Type::Parameter(p)) if p == &definition.type_params[0])
        );
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert_eq!(function.params[0].ty.to_string(), "Option<Option<Int32>>");
        assert!(matches!(&exprs[1], Expr::Field { type_args, .. } if type_args.len() == 1));
        assert_eq!(
            exprs[1].span().slice(source),
            Some("Option.None::<Option<Int32>>")
        );
        assert!(matches!(exprs[3], Expr::BinOp { op: BinOp::Shr, .. }));
        assert!(matches!(exprs[4], Expr::BinOp { op: BinOp::Ge, .. }));
        for invalid in [
            "enum E<> { A }",
            "enum E<T,T> { A }",
            "enum E<_> { A }",
            "enum E<T: Copy> { A(T) }",
            "fn f(x: E<>) {}",
            "fn f(x: E<Int32 Bool>) {}",
            "fn f(x: E<E<Int32>) {}",
            "fn f() { E.None::< > }",
            "struct S<T> {}",
            "type Alias<T> = T",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}
