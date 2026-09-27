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
            if self.peek() == &TokenKind::Comma {
                self.advance();
            }
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
            // A let binding has an optional type annotation.
            TokenKind::Let => {
                self.advance();
                let name = self.expect_ident()?;
                let annotation = if self.peek() == &TokenKind::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Eq)?;
                let value = self.parse_expr()?;
                Ok(Expr::Let {
                    name,
                    annotation,
                    value: Box::new(value),
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
                    let branch = self.parse_expr()?;
                    branches.push(branch);
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
                        if self.peek() == &TokenKind::Comma {
                            self.advance();
                        }
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

    /// Parse binary operators with equal precedence, associating left.
    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_atom()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_atom()?;
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

    /// Parse a block of expressions.
    fn parse_block(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.peek_span().start();
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
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
        while self.peek() != &TokenKind::Eof {
            if self.peek() == &TokenKind::Import {
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
            } else {
                items.push(self.parse_item()?);
            }
        }
        Ok(Module { imports, items })
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
    fn malformed_input_returns_located_diagnostics() {
        for (source, line, message) in [
            ("\nfn f() { # }", 2, "Unexpected character"),
            ("\n\nfn f() { 2147483648 }", 3, "outside Int32 range"),
            (
                "\nfn f() { \"unfinished\n",
                2,
                "Unterminated string literal",
            ),
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
