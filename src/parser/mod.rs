//! Joky-Lite parser.
//!
//! Recursive descent from tokens to a Module AST.
pub mod ast;
mod lexer;

use self::ast::*;
use self::lexer::{Lexer, Token, TokenKind};

/// A lexical or syntactic error at a one-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}

impl Diagnostic {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
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
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Create a parser from source text.
    fn from_source(source: &str) -> Result<Self, Diagnostic> {
        Ok(Parser::new(Lexer::tokenize(source)?))
    }

    // ── Parser helpers ──────────────────────────────────────────────────────────

    fn error(&self, message: String) -> Diagnostic {
        Diagnostic::new(self.peek_line(), message)
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_line(&self) -> usize {
        self.tokens[self.pos].line
    }

    fn advance(&mut self) -> &TokenKind {
        let tok = &self.tokens[self.pos].kind;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
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
                Ok(Type::Named(name))
            }
            other => Err(self.error(format!("Expected a type, got {:?}", other))),
        }
    }

    // ── Parameter lists ──────────────────────────────────────────────────────────

    fn parse_params(&mut self) -> Result<Vec<Param>, Diagnostic> {
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &TokenKind::RParen {
            let name = self.expect_ident()?;
            self.expect(&TokenKind::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty });
            if self.peek() == &TokenKind::Comma {
                self.advance();
            }
        }
        self.expect(&TokenKind::RParen)?;
        Ok(params)
    }

    // ── String interpolation ─────────────────────────────────────────────────────

    /// Split `"Hello, {name}!"` into literal and interpolated segments.
    fn split_string(raw: &str) -> Vec<StringPart> {
        let mut parts = Vec::new();
        let mut cur = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '{' {
                if !cur.is_empty() {
                    parts.push(StringPart::Literal(cur.clone()));
                    cur.clear();
                }
                i += 1; // Skip the opening brace.
                let mut ident = String::new();
                while i < chars.len() && chars[i] != '}' {
                    ident.push(chars[i]);
                    i += 1;
                }
                i += 1; // Skip the closing brace.
                parts.push(StringPart::Interpolated(Box::new(Expr::Ident(
                    ident.trim().to_string(),
                ))));
            } else {
                cur.push(chars[i]);
                i += 1;
            }
        }
        if !cur.is_empty() {
            parts.push(StringPart::Literal(cur));
        }
        parts
    }

    // ── Expression parsing ────────────────────────────────────────────────────────

    /// Parse an atomic expression without binary operators.
    fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek().clone() {
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(Expr::Literal(Literal::Int32(n)))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(false)))
            }
            TokenKind::StrLit(s) => {
                self.advance();
                let parts = Self::split_string(&s);
                Ok(Expr::Literal(Literal::String(parts)))
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
                Ok(Expr::Scope(branches))
            }
            // An identifier may refer to a variable or a function call.
            TokenKind::Ident(name) => {
                self.advance();
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
                    Ok(Expr::Call { callee: name, args })
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            // Parenthesized expression.
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
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
            lhs = Expr::BinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// Parse a block of expressions.
    fn parse_block(&mut self) -> Result<Expr, Diagnostic> {
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
            stmts.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::Block(stmts))
    }

    // ── Top-level parsing ──────────────────────────────────────────────────────────

    fn parse_fn_def(&mut self) -> Result<FnDef, Diagnostic> {
        // The `fn` token has been consumed.
        let name = self.expect_ident()?;
        let params = self.parse_params()?;

        // Optional `@suspends` attribute.
        let suspends = if self.peek() == &TokenKind::At {
            self.advance();
            let attr = self.expect_ident()?;
            if attr != "suspends" {
                return Err(self.error(format!("Unknown attribute @{}", attr)));
            }
            true
        } else {
            false
        };

        // `-> RetType`
        let ret_ty = if self.peek() == &TokenKind::Arrow {
            self.advance();
            self.parse_type()?
        } else {
            Type::Unit
        };

        let body = self.parse_block()?;
        Ok(FnDef {
            name,
            params,
            ret_ty,
            suspends,
            body,
        })
    }

    fn parse_extern_fn(&mut self) -> Result<ExternFnDecl, Diagnostic> {
        // The `extern fn` tokens have been consumed.
        let name = self.expect_ident()?;
        let params = self.parse_params()?;

        let suspends = if self.peek() == &TokenKind::At {
            self.advance();
            let attr = self.expect_ident()?;
            if attr != "suspends" {
                return Err(self.error(format!("Unknown attribute @{}", attr)));
            }
            true
        } else {
            false
        };

        let ret_ty = if self.peek() == &TokenKind::Arrow {
            self.advance();
            self.parse_type()?
        } else {
            Type::Unit
        };

        Ok(ExternFnDecl {
            name,
            params,
            ret_ty,
            suspends,
        })
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        match self.peek().clone() {
            TokenKind::Fn => {
                self.advance();
                Ok(Item::FnDef(self.parse_fn_def()?))
            }
            TokenKind::Extern => {
                self.advance();
                self.expect(&TokenKind::Fn)?;
                Ok(Item::ExternFn(self.parse_extern_fn()?))
            }
            other => Err(self.error(format!("Expected fn or extern fn, got {:?}", other))),
        }
    }

    /// Parse all top-level items.
    pub fn parse_module(&mut self) -> Result<Module, Diagnostic> {
        let mut items = Vec::new();
        while self.peek() != &TokenKind::Eof {
            items.push(self.parse_item()?);
        }
        Ok(Module { items })
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
}
