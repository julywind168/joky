//! Joky-Lite abstract syntax tree.

/// Built-in types.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int32,
    String,
    Bool,
    Unit,
    /// Opaque host types such as VmHandle and FnPtr.
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

/// Literals.
#[derive(Debug, Clone)]
pub enum Literal {
    Int32(i32),
    Bool(bool),
    /// Template segments: literal text or interpolated expressions.
    String(Vec<StringPart>),
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal(String),
    Interpolated(Box<Expr>),
}

/// Expression
#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    /// Variable reference.
    Ident(String),
    /// Function call.
    Call {
        callee: String,
        args: Vec<Expr>,
    },
    /// `let name = expr` evaluates to Unit.
    Let {
        name: String,
        value: Box<Expr>,
    },
    /// Binary arithmetic: +, -, *, /.
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// A block evaluates to its final expression.
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

/// Function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// Top-level declaration.
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

/// Parsed source module.
#[derive(Debug, Clone)]
pub struct Module {
    pub items: Vec<Item>,
}
