//! Joky abstract syntax tree.
use crate::Span;

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
        span: Span,
    },
    /// Variable reference.
    Ident {
        name: String,
        span: Span,
    },
    /// A statically resolved function address used by host registration.
    FunctionRef {
        name: String,
        span: Span,
    },
    /// Function call.
    Call {
        callee: String,
        args: Vec<Expr>,
        span: Span,
    },
    /// `let name = expr` evaluates to Unit.
    Let {
        name: String,
        annotation: Option<Type>,
        value: Box<Expr>,
        span: Span,
    },
    /// `var name = expr` declares a mutable binding and evaluates to Unit.
    Var {
        name: String,
        annotation: Option<Type>,
        value: Box<Expr>,
        span: Span,
    },
    /// Assignment to an existing mutable binding evaluates to Unit.
    Assign {
        name: String,
        value: Box<Expr>,
        span: Span,
    },
    /// Re-evaluate a Bool condition before each iteration; evaluates to Unit.
    While {
        condition: Box<Expr>,
        body: Box<Expr>,
        span: Span,
    },
    /// Select exactly one branch; both branches must have the same type.
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Option<Box<Expr>>,
        span: Span,
    },
    /// Short-circuit Boolean conjunction or disjunction.
    Logical {
        op: LogicalOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// Integer negation or Boolean not.
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    /// Binary arithmetic or comparison.
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// A block evaluates to its final expression.
    Block {
        exprs: Vec<Expr>,
        span: Span,
    },
    /// `a; b` evaluates left to right without opening a scope. A trailing
    /// semicolon discards the final value, so `a;` evaluates to Unit.
    Sequence {
        exprs: Vec<Expr>,
        trailing: bool,
        span: Span,
    },
    /// `scope { | branch1 | branch2 ... }`
    Scope {
        branches: Vec<Expr>,
        span: Span,
    },
}

impl Expr {
    pub const fn span(&self) -> Span {
        match self {
            Self::Literal { span, .. }
            | Self::Ident { span, .. }
            | Self::FunctionRef { span, .. }
            | Self::Call { span, .. }
            | Self::Let { span, .. }
            | Self::Var { span, .. }
            | Self::Assign { span, .. }
            | Self::While { span, .. }
            | Self::If { span, .. }
            | Self::Logical { span, .. }
            | Self::Unary { span, .. }
            | Self::BinOp { span, .. }
            | Self::Block { span, .. }
            | Self::Sequence { span, .. }
            | Self::Scope { span, .. } => *span,
        }
    }

    pub(crate) fn with_span(self, span: Span) -> Self {
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
            Self::Unary { op, operand, .. } => Self::Unary { op, operand, span },
            Self::Logical { op, lhs, rhs, .. } => Self::Logical { op, lhs, rhs, span },
            Self::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => Self::If {
                condition,
                then_branch,
                else_branch,
                span,
            },
            Self::Var {
                name,
                annotation,
                value,
                ..
            } => Self::Var {
                name,
                annotation,
                value,
                span,
            },
            Self::Assign { name, value, .. } => Self::Assign { name, value, span },
            Self::While {
                condition, body, ..
            } => Self::While {
                condition,
                body,
                span,
            },
            Self::Block { exprs, .. } => Self::Block { exprs, span },
            Self::Sequence {
                exprs, trailing, ..
            } => Self::Sequence {
                exprs,
                trailing,
                span,
            },
            Self::Scope { branches, .. } => Self::Scope { branches, span },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub(crate) fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }
}

/// Function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    /// UTF-8 byte range covering the parameter declaration.
    pub span: Span,
}

/// An imported module path.
#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    /// UTF-8 byte range covering the complete import declaration.
    pub span: Span,
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
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ExternFnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    /// UTF-8 byte range covering this declaration.
    pub span: Span,
}

/// Parsed source module.
#[derive(Debug, Clone)]
pub struct Module {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}
