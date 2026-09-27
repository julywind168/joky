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
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            last_span: Span::empty(0),
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
        match self.peek().clone() {
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
                Ok(Type::Named(self.parse_dotted_name(name)?))
            }
            other => Err(self.error(format!("Expected a type, got {:?}", other))),
        }
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
            self.expect(&TokenKind::Colon)?;
            let ty = self.parse_type()?;
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
                let n = i32::try_from(n)
                    .map_err(|_| self.error("Integer literal is outside Int32 range".into()))?;
                self.advance();
                Ok(Expr::Literal {
                    literal: Literal::Int32(n),
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
                let name = self.expect_ident()?;
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
            TokenKind::While => {
                self.advance();
                let condition = Box::new(self.parse_assignment()?);
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
                    let branch = self.parse_assignment()?;
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
            // An identifier may refer to a variable or a function call.
            TokenKind::Ident(name) => {
                self.advance();
                let name = self.parse_dotted_name(name)?;
                if self.peek() == &TokenKind::LParen {
                    // Function call.
                    self.advance();
                    let mut args = Vec::new();
                    while self.peek() != &TokenKind::RParen {
                        args.push(self.parse_expr()?);
                        self.expect_list_separator()?;
                    }
                    self.expect(&TokenKind::RParen)?;
                    Ok(Expr::Call {
                        callee: name,
                        args,
                        span: Span::new(start, self.last_span.end()),
                    })
                } else {
                    Ok(Expr::Ident {
                        name,
                        span: Span::new(start, self.last_span.end()),
                    })
                }
            }
            // Parenthesized expression.
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr.with_span(Span::new(start, self.last_span.end())))
            }
            // Block
            TokenKind::LBrace => self.parse_block(),
            other => Err(self.error(format!("Expected an expression, got {:?}", other))),
        }
    }

    fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::If)?;
        let condition = Box::new(self.parse_assignment()?);
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
        let Expr::Ident { name, .. } = lhs else {
            return Err(self.error("Assignment target must be a variable".into()));
        };
        self.advance();
        let value = Box::new(self.parse_assignment()?);
        Ok(Expr::Assign {
            name,
            value,
            span: Span::new(start, self.last_span.end()),
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

    /// Comparisons bind below arithmetic and cannot chain without parentheses.
    fn parse_comparison(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.parse_additive()?;
        if let Some(op) = self.comparison_op() {
            self.advance();
            let rhs = self.parse_additive()?;
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

    fn parse_additive(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_product()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_product()?;
            let span = Span::new(lhs.span().start(), rhs.span().end());
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_product(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_unary()?;
            let span = Span::new(lhs.span().start(), rhs.span().end());
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let op = match self.peek() {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            _ => return self.parse_atom(),
        };
        let start = self.peek_span().start();
        self.advance();
        // This magnitude is only valid directly after a minus, never as a
        // positive expression (including inside parentheses).
        if op == UnaryOp::Neg && self.peek() == &TokenKind::IntLit(i32::MAX as u32 + 1) {
            self.advance();
            return Ok(Expr::Literal {
                literal: Literal::Int32(i32::MIN),
                span: Span::new(start, self.last_span.end()),
            });
        }
        let operand = Box::new(self.parse_unary()?);
        let span = Span::new(start, operand.span().end());
        Ok(Expr::Unary { op, operand, span })
    }

    /// Parse a block of expressions.
    fn parse_block(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            stmts.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RBrace)?;
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

    fn parse_fn_def(&mut self, start: usize) -> Result<FnDef, Diagnostic> {
        // The `fn` token has been consumed.
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let suspends = self.parse_suspends_attr()?;
        let ret_ty = self.parse_ret_ty()?;
        let body = self.parse_block()?;
        Ok(FnDef {
            name,
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

    /// Parse all top-level items.
    pub fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut items = Vec::new();
        let mut imports = Vec::new();
        let mut type_aliases = Vec::new();
        while self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Semi {
                self.advance();
                continue;
            }
            if self.peek() == &TokenKind::Import {
                if !items.is_empty() || !type_aliases.is_empty() {
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
                imports.push(Import {
                    path,
                    span: Span::new(start, self.last_span.end()),
                });
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
            type_aliases,
            items,
        })
    }
}

// ── Convenience API ──────────────────────────────────────────────────────────────

/// Parse a source module or return a diagnostic.
pub fn parse(source: &str) -> Result<Module, Diagnostic> {
    Parser::from_source(source)?.parse_module()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ("true & false", "Unexpected character"),
        ] {
            let source = format!("fn f() {{ {body} }}");
            let error = parse(&source).unwrap_err();
            assert!(error.message.contains(message), "{source}: {error}");
            assert!(error.span.slice(&source).is_some());
        }
        parse("fn f() { if true {}\nelse {} scope { | f() | f() } }").unwrap();
    }

    #[test]
    fn integer_literal_boundaries_keep_precise_diagnostics() {
        for expression in ["2147483647", "-2147483648", "--2147483648", "- 2147483648"] {
            parse(&format!("fn f() -> Int32 {{ {expression} }}")).unwrap();
        }
        for (expression, marked) in [
            ("2147483648", "2147483648"),
            ("-(2147483648)", "2147483648"),
            ("-2147483649", "2147483649"),
            ("4294967296", "4294967296"),
            (
                "999999999999999999999999999999",
                "999999999999999999999999999999",
            ),
        ] {
            let source = format!("// é\nfn f() -> Int32 {{\n{expression}\n}}");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.line, 3);
            assert!(error.message.contains("outside Int32 range"), "{error}");
            assert_eq!(error.span.slice(&source), Some(marked));
        }
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
            ("\n\nfn f() { 2147483648 }", 3, "outside Int32 range"),
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
