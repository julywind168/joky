//! Lexing, parsing, and AST layers for Joky source code
//!
//! The lexer turns the character stream into spanned tokens and the parser builds the AST;
//! this module does no name resolution or type checking, so syntax errors are
//! reported independently before entering `sema`

mod ast;
mod lexer;
mod parser;
mod token;
pub mod visitor;

#[cfg(test)]
mod lexer_snapshots;
#[cfg(test)]
mod parser_snapshots;
#[cfg(test)]
mod visitor_examples;

pub use ast::{
    ArithmeticMode, ArithmeticOp, BinaryOp, CallArgument, CastMode, Class, ClassField,
    CollectionLiteral, Constant, Effect, EffectMode, EffectOperation, Enum, EnumVariant, Expr,
    ExprKind, FieldAccess, ForeignFunction, Function, HandlerArm, Impl, ImplType, Import,
    IntrinsicMethod, IntrinsicType, IntrinsicTypeKind, MapLiteralEntry, MatchArm, NodeId,
    Parameter, Pattern, PatternField, Program, ReceiverMode, Struct, StructField, Trait, TraitType,
    TypeAnnotation, TypeArgument, TypeExpr, TypeParameter, UnaryOp, Visibility, WherePredicate,
};
pub use parser::parse_program;
pub(crate) use parser::parse_program_at;
pub(crate) use parser::parse_program_named;
pub use visitor::{walk_expr, walk_function, walk_program, ExprVisitor};

/// Maximum recursive expression nesting accepted by the parser and type checker.
///
/// Left-associated operator chains such as `a + b + c + …` are parsed and
/// checked iteratively, so they can be longer than this limit. Nested `if`,
/// `match`, calls, and similar prefix forms increment the depth and receive a
/// source-located diagnostic when they exceed it.
pub const MAX_EXPRESSION_NESTING: usize = 64;
