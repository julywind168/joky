//! Joky abstract syntax tree.
use crate::Span;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Parsed type names; aliases are normalized before semantic and ABI checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int32,
    Int64,
    UInt32,
    UInt64,
    String,
    Bool,
    Unit,
    /// An unresolved source type name or a registered opaque host type.
    Named(String),
    /// An unresolved application of a module-local generic enum.
    Applied(Arc<TypeApplication>),
    /// A rigid, declaration-local Copy type parameter; never an executable value tag.
    Parameter(Arc<TypeParameter>),
    /// A resolved module-local nominal record with a finite scalar layout.
    Record(Arc<RecordType>),
    /// A resolved nominal tagged union with copyable payloads.
    Enum(Arc<EnumType>),
    /// A structural tuple, normalized before planning.
    Tuple(Arc<TupleType>),
}

/// Unresolved generic enum application; boxed to keep typed IR slots compact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeApplication {
    pub name: String,
    pub arguments: Vec<Type>,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Alias expansion can produce shared, zero-sized tuple DAGs whose
        // fully expanded spelling is exponentially larger than the source.
        fn display(
            ty: &Type,
            f: &mut std::fmt::Formatter<'_>,
            remaining: &mut usize,
        ) -> std::fmt::Result {
            if *remaining == 0 {
                return f.write_str("...");
            }
            *remaining -= 1;
            match ty {
                Type::Int32 => f.write_str("Int32"),
                Type::Int64 => f.write_str("Int64"),
                Type::UInt32 => f.write_str("UInt32"),
                Type::UInt64 => f.write_str("UInt64"),
                Type::String => f.write_str("String"),
                Type::Bool => f.write_str("Bool"),
                Type::Unit => f.write_str("Unit"),
                Type::Named(name) => f.write_str(name),
                Type::Parameter(parameter) => f.write_str(&parameter.name),
                Type::Record(record) => f.write_str(&record.name),
                Type::Enum(enumeration) => {
                    f.write_str(&enumeration.name)?;
                    arguments(&enumeration.arguments, f, remaining)
                }
                Type::Applied(application) => {
                    f.write_str(&application.name)?;
                    arguments(&application.arguments, f, remaining)
                }
                Type::Tuple(tuple) => {
                    f.write_str("(")?;
                    for (index, ty) in tuple.elements.iter().enumerate() {
                        if index != 0 {
                            f.write_str(", ")?;
                        }
                        if *remaining == 0 {
                            f.write_str("...")?;
                            break;
                        }
                        display(ty, f, remaining)?;
                    }
                    if tuple.elements.len() == 1 {
                        f.write_str(",")?;
                    }
                    f.write_str(")")
                }
            }
        }
        fn arguments(
            args: &[Type],
            f: &mut std::fmt::Formatter<'_>,
            remaining: &mut usize,
        ) -> std::fmt::Result {
            if args.is_empty() {
                return Ok(());
            }
            f.write_str("<")?;
            for (index, ty) in args.iter().enumerate() {
                if index != 0 {
                    f.write_str(", ")?;
                }
                if *remaining == 0 {
                    f.write_str("...")?;
                    break;
                }
                display(ty, f, remaining)?;
            }
            f.write_str(">")
        }
        display(self, f, &mut 256)
    }
}

/// Literals.
#[derive(Debug, Clone)]
pub enum Literal {
    Int32(i32),
    Int64(i64),
    UInt32(u32),
    UInt64(u64),
    /// Untyped integer magnitude, resolved from context before execution.
    Integer {
        magnitude: u64,
        negative: bool,
    },
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
    /// A generic name without a call, used by nullary constructors.
    GenericName(Box<GenericName>),
    /// Variable reference.
    Ident {
        name: String,
        span: Span,
    },
    /// Construct a nominal record; initializer order is significant.
    Record {
        name: String,
        fields: Vec<FieldInit>,
        span: Span,
    },
    /// Nonempty positional aggregate, evaluated from left to right.
    Tuple {
        elements: Vec<Expr>,
        span: Span,
    },
    /// An irrefutable tuple or discard binding, evaluated once.
    Destructure {
        pattern: BindingPattern,
        mutable: bool,
        annotation: Option<Type>,
        value: Box<Expr>,
        span: Span,
    },
    /// Evaluate once and select an exhaustive variant arm.
    Match {
        value: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// Project a field from any record expression.
    Field {
        base: Box<Expr>,
        name: String,
        /// Explicit arguments for a nullary enum constructor; empty for projections.
        type_args: Vec<Type>,
        span: Span,
    },
    /// Assign through a field path rooted in a mutable binding.
    FieldAssign {
        target: Box<Expr>,
        value: Box<Expr>,
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
        /// Empty for inferred calls; explicit calls use `::<Type, ...>`.
        type_args: Vec<Type>,
        args: Vec<Expr>,
        span: Span,
    },
    /// A call on an arbitrary receiver expression, resolved statically.
    MethodCall(Box<MethodCall>),
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
    /// Finish the current function, optionally with a value.
    Return {
        value: Option<Box<Expr>>,
        span: Span,
    },
    /// Exit the nearest loop in this function.
    Break {
        span: Span,
    },
    /// Restart the nearest loop's condition in this function.
    Continue {
        span: Span,
    },
    /// Select exactly one branch; normally completing branches must agree in type.
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
            Self::GenericName(name) => name.span,
            Self::MethodCall(call) => call.span,
            Self::Tuple { span, .. }
            | Self::Destructure { span, .. }
            | Self::Match { span, .. }
            | Self::Record { span, .. }
            | Self::Field { span, .. }
            | Self::FieldAssign { span, .. }
            | Self::Literal { span, .. }
            | Self::Ident { span, .. }
            | Self::FunctionRef { span, .. }
            | Self::Call { span, .. }
            | Self::Let { span, .. }
            | Self::Var { span, .. }
            | Self::Assign { span, .. }
            | Self::While { span, .. }
            | Self::Return { span, .. }
            | Self::Break { span }
            | Self::Continue { span }
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
            Self::GenericName(mut name) => {
                name.span = span;
                Self::GenericName(name)
            }
            Self::MethodCall(mut call) => {
                call.span = span;
                Self::MethodCall(call)
            }
            Self::Tuple { elements, .. } => Self::Tuple { elements, span },
            Self::Destructure {
                pattern,
                mutable,
                annotation,
                value,
                ..
            } => Self::Destructure {
                pattern,
                mutable,
                annotation,
                value,
                span,
            },
            Self::Match { value, arms, .. } => Self::Match { value, arms, span },
            Self::Record { name, fields, .. } => Self::Record { name, fields, span },
            Self::Field {
                base,
                name,
                type_args,
                ..
            } => Self::Field {
                base,
                name,
                type_args,
                span,
            },
            Self::FieldAssign { target, value, .. } => Self::FieldAssign {
                target,
                value,
                span,
            },
            Self::Literal { literal, .. } => Self::Literal { literal, span },
            Self::Ident { name, .. } => Self::Ident { name, span },
            Self::FunctionRef { name, .. } => Self::FunctionRef { name, span },
            Self::Call {
                callee,
                type_args,
                args,
                ..
            } => Self::Call {
                callee,
                type_args,
                args,
                span,
            },
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
            Self::Return { value, .. } => Self::Return { value, span },
            Self::Break { .. } => Self::Break { span },
            Self::Continue { .. } => Self::Continue { span },
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

/// A method receiver is evaluated once, before its explicit arguments.
#[derive(Debug, Clone)]
pub struct MethodCall {
    pub receiver: Expr,
    pub name: String,
    pub type_args: Vec<Type>,
    pub args: Vec<Expr>,
    pub span: Span,
}

/// An inherent implementation for a concrete type.
#[derive(Debug, Clone)]
pub struct ImplDef {
    pub target: Type,
    pub target_name: String,
    pub methods: Vec<FnDef>,
    pub constants: Vec<AssociatedConst>,
    pub span: Span,
}

/// A typed scalar constant, evaluated before function planning.
#[derive(Debug, Clone)]
pub struct AssociatedConst {
    pub name: String,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
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
    BitNot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub(crate) fn is_shift(&self) -> bool {
        matches!(self, Self::Shl | Self::Shr)
    }

    pub(crate) fn can_fail(&self) -> bool {
        matches!(self, Self::Div | Self::Shl | Self::Shr)
    }

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
    /// An optional module alias, reserved for qualified module references.
    pub alias: Option<String>,
    /// Explicitly imported names. An empty list means the whole module alias.
    pub members: Vec<ImportMember>,
    /// UTF-8 byte range covering the complete import declaration.
    pub span: Span,
}

/// A name selected from an imported module.
#[derive(Debug, Clone)]
pub struct ImportMember {
    pub name: String,
    pub kind: ImportMemberKind,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMemberKind {
    Type,
    Value,
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
    pub type_params: Vec<Arc<TypeParameter>>,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    pub body: Expr,
    /// UTF-8 byte range covering this declaration.
    pub span: Span,
}

/// A distinct rigid parameter owned by one generic function or enum declaration.
#[derive(Debug, Clone)]
pub struct TypeParameter {
    pub name: String,
    pub span: Span,
    pub(crate) id: u64,
}
impl TypeParameter {
    pub fn new(name: String, span: Span) -> Self {
        Self {
            name,
            span,
            id: fresh_type_id(),
        }
    }
}
impl PartialEq for TypeParameter {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TypeParameter {}

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
    /// Bound imports survive normalization without exposing private declarations.
    pub(crate) scope: Option<Arc<crate::core::imports::ImportScope>>,
    pub type_aliases: Vec<TypeAlias>,
    pub records: Vec<RecordDef>,
    pub enums: Vec<EnumDef>,
    pub implementations: Vec<ImplDef>,
    pub items: Vec<Item>,
}

/// A module-local transparent alias, `type Name = Target`.
#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: String,
    pub target: Type,
    pub name_span: Span,
    pub target_span: Span,
    pub span: Span,
}

/// One explicitly named field initializer.
#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// A parsed record declaration. Clones preserve its nominal identity.
#[derive(Debug, Clone)]
pub struct RecordDef {
    pub name: String,
    pub fields: Vec<Param>,
    pub name_span: Span,
    pub span: Span,
    pub(crate) id: u64,
}

impl RecordDef {
    /// Create a fresh declaration identity; cloning preserves that identity.
    pub fn new(name: String, fields: Vec<Param>, name_span: Span, span: Span) -> Self {
        let id = fresh_type_id();
        Self {
            name,
            fields,
            name_span,
            span,
            id,
        }
    }
}

/// A resolved nominal type. Its layout is internal, not a host ABI contract.
pub struct RecordType {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) fields: Vec<RecordField>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}

// Avoid recursively formatting shared layouts (which may expand exponentially).
impl std::fmt::Debug for RecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordType")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

impl PartialEq for RecordType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for RecordType {}

#[derive(Debug)]
pub(crate) struct RecordField {
    pub name: String,
    pub ty: Type,
    pub offset: usize,
}

impl Type {
    pub(crate) fn scalar_types(&self) -> &[Type] {
        match self {
            Self::Record(record) => &record.scalars,
            Self::Enum(enumeration) => &enumeration.scalars,
            Self::Tuple(tuple) => &tuple.scalars,
            Self::Unit => &[],
            _ => std::slice::from_ref(self),
        }
    }

    pub(crate) fn is_aggregate(&self) -> bool {
        matches!(self, Self::Record(_) | Self::Enum(_) | Self::Tuple(_))
    }

    pub(crate) fn nominal(&self) -> Option<(&str, u64)> {
        match self {
            Self::Record(record) => Some((&record.name, record.id)),
            Self::Enum(enumeration) => Some((&enumeration.name, enumeration.id)),
            _ => None,
        }
    }

    pub(crate) fn layout_depth(&self) -> usize {
        match self {
            Self::Record(record) => record.depth,
            Self::Enum(enumeration) => enumeration.depth,
            Self::Tuple(tuple) => tuple.depth,
            _ => 0,
        }
    }

    /// Keep legacy Unit parameter placeholders, but flatten aggregate parameters.
    pub(crate) fn parameter_slots(&self) -> usize {
        if *self == Self::Unit {
            1
        } else {
            self.scalar_types().len()
        }
    }
}

fn fresh_type_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("nominal type identity space exhausted")
}

/// A module-local nominal enum declaration with optional Copy type parameters.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub variants: Vec<VariantDef>,
    pub name_span: Span,
    pub span: Span,
    pub(crate) id: u64,
}
impl EnumDef {
    /// Create a fresh declaration identity; cloning preserves it.
    pub fn new(name: String, variants: Vec<VariantDef>, name_span: Span, span: Span) -> Self {
        Self {
            name,
            type_params: Vec::new(),
            variants,
            name_span,
            span,
            id: fresh_type_id(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VariantDef {
    pub name: String,
    pub payload: Option<Type>,
    pub span: Span,
}

/// A resolved nominal enum. Its layout is private to the compiler.
pub struct EnumType {
    pub(crate) id: u64,
    pub(crate) parameters: Vec<Arc<TypeParameter>>,
    pub(crate) arguments: Vec<Type>,
    pub(crate) template: Option<Arc<EnumType>>,
    pub(crate) name: String,
    pub(crate) variants: Vec<EnumVariant>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}
impl std::fmt::Debug for EnumType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnumType")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}
impl PartialEq for EnumType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.arguments.len() == other.arguments.len()
            && equal_types(self.arguments.iter().zip(&other.arguments))
    }
}
impl Eq for EnumType {}

pub(crate) struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    /// `Variant`, `Enum.Variant`, or either followed by one payload binding.
    Variant {
        name: String,
        binding: Option<String>,
        span: Span,
    },
}
impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Wildcard { span } | Self::Variant { span, .. } => *span,
        }
    }
}

/// Tuple syntax and canonical layout. Equality compares ordered element types.
pub struct TupleType {
    pub(crate) elements: Vec<Type>,
    pub(crate) scalars: Vec<Type>,
    pub(crate) depth: usize,
}
// Shared zero-sized layouts can form deep DAGs. Do not expand those DAGs
// recursively in comparisons or debug output.
impl std::fmt::Debug for TupleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TupleType")
            .field("arity", &self.elements.len())
            .field("scalar_slots", &self.scalars.len())
            .field("depth", &self.depth)
            .finish()
    }
}
impl PartialEq for TupleType {
    fn eq(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && equal_types(self.elements.iter().zip(&other.elements))
    }
}

// Tuple and generic-enum arguments can form shared DAGs, including phantom types.
// Compare both kinds in one traversal instead of recursively expanding each edge.
fn equal_types<'a>(pairs: impl Iterator<Item = (&'a Type, &'a Type)>) -> bool {
    let mut pending: Vec<_> = pairs.collect();
    let mut tuples = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (Type::Tuple(a), Type::Tuple(b)) => {
                if a.elements.len() != b.elements.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && tuples.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.elements.iter().zip(&b.elements));
                }
            }
            (Type::Enum(a), Type::Enum(b)) => {
                if a.id != b.id || a.arguments.len() != b.arguments.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && enums.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
            _ if a != b => return false,
            _ => {}
        }
    }
    true
}
impl Eq for TupleType {}
impl Type {
    /// Build a tuple type annotation. Resolution checks nonempty Copy elements
    /// and computes its layout before compilation; this is not a host ABI type.
    pub fn tuple(elements: Vec<Type>) -> Self {
        Self::Tuple(Arc::new(TupleType {
            elements,
            scalars: Vec::new(),
            depth: 0,
        }))
    }
}

/// Irrefutable binding patterns; enum and literal patterns are excluded.
#[derive(Debug, Clone)]
pub enum BindingPattern {
    Name {
        name: String,
        span: Span,
    },
    Discard {
        span: Span,
    },
    Tuple {
        elements: Vec<BindingPattern>,
        span: Span,
    },
}
impl BindingPattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Name { span, .. } | Self::Discard { span } | Self::Tuple { span, .. } => *span,
        }
    }
}

/// An explicitly instantiated name without call parentheses.
#[derive(Debug, Clone)]
pub struct GenericName {
    pub name: String,
    pub type_args: Vec<Type>,
    pub span: Span,
}
