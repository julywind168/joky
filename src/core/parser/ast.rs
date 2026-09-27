//! Joky abstract syntax tree.
use std::ops::Range;

/// Built-in types.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Immutable UTF-8 text; braces are literal characters.
    String(String),
}

/// Expression
#[derive(Debug, Clone)]
pub enum Expr {
    Literal {
        literal: Literal,
        span: Range<usize>,
    },
    /// Variable reference.
    Ident { name: String, span: Range<usize> },
    /// A statically resolved function address used by host registration.
    FunctionRef { name: String, span: Range<usize> },
    /// Function call.
    Call {
        callee: String,
        args: Vec<Expr>,
        span: Range<usize>,
    },
    /// `let name = expr` evaluates to Unit.
    Let {
        name: String,
        annotation: Option<Type>,
        value: Box<Expr>,
        span: Range<usize>,
    },
    /// Binary arithmetic: +, -, *, /.
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Range<usize>,
    },
    /// A block evaluates to its final expression.
    Block {
        exprs: Vec<Expr>,
        span: Range<usize>,
    },
    /// `scope { | branch1 | branch2 ... }`
    Scope {
        branches: Vec<Expr>,
        span: Range<usize>,
    },
}

impl Expr {
    pub fn span(&self) -> Range<usize> {
        match self {
            Self::Literal { span, .. }
            | Self::Ident { span, .. }
            | Self::FunctionRef { span, .. }
            | Self::Call { span, .. }
            | Self::Let { span, .. }
            | Self::BinOp { span, .. }
            | Self::Block { span, .. }
            | Self::Scope { span, .. } => span.clone(),
        }
    }

    pub(crate) fn with_span(self, span: Range<usize>) -> Self {
        match self {
            Self::Literal { literal, .. } => Self::Literal { literal, span },
            Self::Ident { name, .. } => Self::Ident { name, span },
            Self::FunctionRef { name, .. } => Self::FunctionRef { name, span },
            Self::Call { callee, args, .. } => Self::Call { callee, args, span },
            Self::Let {
                name,
                annotation,
                value,
                ..
            } => Self::Let {
                name,
                annotation,
                value,
                span,
            },
            Self::BinOp { op, lhs, rhs, .. } => Self::BinOp { op, lhs, rhs, span },
            Self::Block { exprs, .. } => Self::Block { exprs, span },
            Self::Scope { branches, .. } => Self::Scope { branches, span },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
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
    /// UTF-8 byte range covering this declaration.
    pub span: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct ExternFnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    /// UTF-8 byte range covering this declaration.
    pub span: Range<usize>,
}

/// Parsed source module.
#[derive(Debug, Clone)]
pub struct Module {
    pub imports: Vec<String>,
    pub items: Vec<Item>,
}
