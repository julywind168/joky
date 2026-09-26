/// Joky-Lite AST 节点定义

/// 基础类型
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int32,
    String,
    Bool,
    Unit,
    /// 用于 VmHandle、FnPtr 等不透明宿主类型
    Named(String),
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int32 => write!(f, "Int32"),
            Type::String => write!(f, "String"),
            Type::Bool => write!(f, "Bool"),
            Type::Unit => write!(f, "Unit"),
            Type::Named(n) => write!(f, "{}", n),
        }
    }
}

/// 字面量
#[derive(Debug, Clone)]
pub enum Literal {
    Int32(i32),
    Bool(bool),
    /// 已处理插值的模板段列表：纯字符串段或插值表达式
    String(Vec<StringPart>),
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal(String),
    Interpolated(Box<Expr>),
}

/// 表达式
#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    /// 变量引用
    Ident(String),
    /// 函数调用
    Call {
        callee: String,
        args: Vec<Expr>,
    },
    /// `let name = expr`（作为表达式，值为 Unit）
    Let {
        name: String,
        value: Box<Expr>,
    },
    /// 二元运算（仅 +、-、*、/ 四则）
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// 块：多个表达式，值为最后一个
    Block(Vec<Expr>),
    /// `scope { | branch1 | branch2 ... }`
    Scope(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for BinOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
        };
        write!(f, "{}", s)
    }
}

/// 函数参数
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// 顶层定义
#[derive(Debug, Clone)]
pub enum Item {
    /// `fn name(params) @suspends? -> RetType { body }`
    FnDef(FnDef),
    /// `extern fn name(params) -> RetType`
    ExternFn(ExternFnDecl),
}

#[derive(Debug, Clone)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub struct ExternFnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
}

/// 整个源文件
#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
}
