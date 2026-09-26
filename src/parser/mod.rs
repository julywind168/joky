/// Joky-Lite 解析器
///
/// 递归下降，消费 token 列表，产出 Module AST。
pub mod ast;
pub mod lexer;

use self::ast::*;
use self::lexer::{Lexer, Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// 从源码字符串直接构建解析器
    pub fn from_source(source: &str) -> Self {
        Parser::new(Lexer::tokenize(source))
    }

    // ── 内部辅助 ──────────────────────────────────────────────────────────

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

    fn expect(&mut self, expected: &TokenKind) -> Result<(), String> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(format!(
                "行 {}: 期望 {:?}，遇到 {:?}",
                self.peek_line(),
                expected,
                self.peek()
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.peek().clone() {
            TokenKind::Ident(s) => {
                self.advance();
                Ok(s)
            }
            other => Err(format!(
                "行 {}: 期望标识符，遇到 {:?}",
                self.peek_line(),
                other
            )),
        }
    }

    // ── 类型解析 ──────────────────────────────────────────────────────────

    fn parse_type(&mut self) -> Result<Type, String> {
        match self.peek().clone() {
            TokenKind::TyInt32 => { self.advance(); Ok(Type::Int32) }
            TokenKind::TyString => { self.advance(); Ok(Type::String) }
            TokenKind::TyBool => { self.advance(); Ok(Type::Bool) }
            TokenKind::TyUnit => { self.advance(); Ok(Type::Unit) }
            TokenKind::Ident(name) => { self.advance(); Ok(Type::Named(name)) }
            other => Err(format!(
                "行 {}: 期望类型，遇到 {:?}",
                self.peek_line(), other
            )),
        }
    }

    // ── 参数列表 ──────────────────────────────────────────────────────────

    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
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

    // ── 字符串插值拆分 ─────────────────────────────────────────────────────

    /// 把 `"Hello, {name}!"` 拆成 `[Literal("Hello, "), Interpolated(Ident("name")), Literal("!")]`
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
                i += 1; // 跳过 {
                let mut ident = String::new();
                while i < chars.len() && chars[i] != '}' {
                    ident.push(chars[i]);
                    i += 1;
                }
                i += 1; // 跳过 }
                parts.push(StringPart::Interpolated(Box::new(
                    Expr::Ident(ident.trim().to_string()),
                )));
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

    // ── 表达式解析 ────────────────────────────────────────────────────────

    /// 解析一个原子表达式（不含二元运算符）
    fn parse_atom(&mut self) -> Result<Expr, String> {
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
            // let 绑定：`let name` 或 `let name: Type` 然后 `= expr`
            TokenKind::Let => {
                self.advance();
                let name = self.expect_ident()?;
                // 可选的类型注解 `: Type`（阶段1：解析后忽略）
                if self.peek() == &TokenKind::Colon {
                    self.advance();
                    self.parse_type()?;
                }
                self.expect(&TokenKind::Eq)?;
                let value = self.parse_expr()?;
                Ok(Expr::Let { name, value: Box::new(value) })
            }
            // scope { | branch ... }
            TokenKind::Scope => {
                self.advance();
                self.expect(&TokenKind::LBrace)?;
                let mut branches = Vec::new();
                while self.peek() == &TokenKind::Pipe {
                    self.advance(); // 消费 |
                    let branch = self.parse_expr()?;
                    branches.push(branch);
                }
                self.expect(&TokenKind::RBrace)?;
                Ok(Expr::Scope(branches))
            }
            // 标识符：可能是变量引用或函数调用
            TokenKind::Ident(name) => {
                self.advance();
                if self.peek() == &TokenKind::LParen {
                    // 函数调用
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
            // 括号分组
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            // 块
            TokenKind::LBrace => {
                self.parse_block()
            }
            other => Err(format!(
                "行 {}: 期望表达式，遇到 {:?}",
                self.peek_line(), other
            )),
        }
    }

    /// 解析二元运算表达式（左结合，单优先级）
    fn parse_expr(&mut self) -> Result<Expr, String> {
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
            lhs = Expr::BinOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    /// 解析 `{ expr1\n expr2\n ... }`
    fn parse_block(&mut self) -> Result<Expr, String> {
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &TokenKind::RBrace && self.peek() != &TokenKind::Eof {
            stmts.push(self.parse_expr()?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::Block(stmts))
    }

    // ── 顶层解析 ──────────────────────────────────────────────────────────

    fn parse_fn_def(&mut self) -> Result<FnDef, String> {
        // 已消费 `fn`
        let name = self.expect_ident()?;
        let params = self.parse_params()?;

        // 可选 `@suspends`
        let suspends = if self.peek() == &TokenKind::At {
            self.advance();
            let attr = self.expect_ident()?;
            if attr != "suspends" {
                return Err(format!("未知属性 @{}", attr));
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
        Ok(FnDef { name, params, ret_ty, suspends, body })
    }

    fn parse_extern_fn(&mut self) -> Result<ExternFnDecl, String> {
        // 已消费 `extern fn`
        let name = self.expect_ident()?;
        let params = self.parse_params()?;

        let suspends = if self.peek() == &TokenKind::At {
            self.advance();
            let attr = self.expect_ident()?;
            if attr != "suspends" {
                return Err(format!("未知属性 @{}", attr));
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

        Ok(ExternFnDecl { name, params, ret_ty, suspends })
    }

    fn parse_item(&mut self) -> Result<Item, String> {
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
            other => Err(format!(
                "行 {}: 期望顶层定义（fn / extern fn），遇到 {:?}",
                self.peek_line(), other
            )),
        }
    }

    /// 解析整个模块（顶层 item 列表）
    pub fn parse_module(&mut self) -> Result<Module, String> {
        let mut items = Vec::new();
        while self.peek() != &TokenKind::Eof {
            items.push(self.parse_item()?);
        }
        Ok(Module { items })
    }
}

// ── 便捷接口 ──────────────────────────────────────────────────────────────

/// 从源码解析出 Module，失败时返回错误信息
pub fn parse(source: &str) -> Result<Module, String> {
    Parser::from_source(source).parse_module()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析简单加法函数() {
        let src = "fn add(x: Int32, y: Int32) -> Int32 { x + y }";
        let module = parse(src).expect("解析失败");
        assert_eq!(module.items.len(), 1);
        match &module.items[0] {
            Item::FnDef(f) => {
                assert_eq!(f.name, "add");
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.ret_ty, Type::Int32);
                assert!(!f.suspends);
            }
            _ => panic!("期望 FnDef"),
        }
    }

    #[test]
    fn 解析extern函数() {
        let src = "extern fn host_multiply(x: Int32, y: Int32) -> Int32";
        let module = parse(src).expect("解析失败");
        match &module.items[0] {
            Item::ExternFn(e) => {
                assert_eq!(e.name, "host_multiply");
                assert_eq!(e.ret_ty, Type::Int32);
            }
            _ => panic!("期望 ExternFn"),
        }
    }

    #[test]
    fn 解析可挂起函数() {
        let src = "fn delayed_add(x: Int32, y: Int32) @suspends -> Int32 { x + y }";
        let module = parse(src).expect("解析失败");
        match &module.items[0] {
            Item::FnDef(f) => {
                assert!(f.suspends);
                assert_eq!(f.ret_ty, Type::Int32);
            }
            _ => panic!("期望 FnDef"),
        }
    }

    #[test]
    fn 解析多个顶层定义() {
        let src = r#"
fn add(x: Int32, y: Int32) -> Int32 { x + y }
extern fn host_multiply(x: Int32, y: Int32) -> Int32
"#;
        let module = parse(src).expect("解析失败");
        assert_eq!(module.items.len(), 2);
    }
}
