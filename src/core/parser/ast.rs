//! Joky abstract syntax tree.
use crate::core::types::fresh_type_id;
use crate::Span;
use std::sync::Arc;

// Preserve the public AST paths for callers constructing type annotations.
pub use crate::core::types::{
    AssociatedEquality, ConstArgument, DynInterface, DynMethodSlot, DynTraitType, EnumType,
    FixedArrayType, FunctionType, GenericArgument, ParameterKind, RecordType, ReferenceType,
    TraitBound, TupleType, Type, TypeApplication, TypeParameter, TypeProjection,
};

/// Completion policy for structured concurrent branches.
#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentKind {
    Join,
    Race,
    /// A compiler-generated single-child failure boundary.
    Isolate,
}

impl ConcurrentKind {
    pub(crate) const fn keyword(self) -> &'static str {
        match self {
            Self::Join => "join",
            Self::Race => "race",
            Self::Isolate => "isolate",
        }
    }
}

/// Literals.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub enum Literal {
    /// Rigid UInt64 value, replaced by an immediate during specialization.
    ConstParameter(Arc<TypeParameter>),
    Float32(f32),
    Float64(f64),
    /// Decimal spelling retained until contextual precision is known.
    Float(String),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    Rune(u32),
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
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub enum Expr {
    /// Compiler-authored indexed loop body; integer type is inferred before lowering.
    IndexedLoop(Box<IndexedLoop>),
    /// An independently queued, compiler-checked state operation.
    When(Box<When>),
    /// A synchronous anonymous function with declaration-local capture identity.
    AnonymousFunction(Box<AnonymousFunction>),
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
        type_args: Vec<GenericArgument>,
        fields: Vec<FieldInit>,
        span: Span,
    },
    /// Checked element place in an array or slice.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    /// Contextually typed homogeneous sequence, evaluated from left to right.
    Array {
        elements: Vec<Expr>,
        span: Span,
    },
    /// Sequence expansion, permitted only as an array literal entry.
    Spread {
        value: Box<Expr>,
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
        /// Two compiler-authored arms for if let / while let / for; an irrefutable
        /// success pattern may make the implicit wildcard fallback unreachable.
        conditional: bool,
        /// Parser-authored for step, resolved after the cursor type is known.
        iteration: bool,
        value: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// Project a field from any record expression.
    Field {
        base: Box<Expr>,
        name: String,
        /// Explicit arguments for a nullary enum constructor; empty for projections.
        type_args: Vec<GenericArgument>,
        span: Span,
    },
    /// Assign through a field path rooted in a mutable binding.
    FieldAssign {
        /// None for `=`, otherwise the binary operation before writing back.
        op: Option<BinOp>,
        target: Box<Expr>,
        value: Box<Expr>,
        span: Span,
    },
    /// A statically resolved function address used by host registration.
    FunctionRef {
        name: String,
        span: Span,
    },
    /// Call a function-valued expression.
    Invoke {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    /// Function call.
    Call {
        callee: String,
        /// Empty for inferred calls; explicit calls use `::<Type, ...>`.
        type_args: Vec<GenericArgument>,
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
        /// None for `=`, otherwise the binary operation before writing back.
        op: Option<BinOp>,
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
    /// Consecutive plain if arms, kept flat for parsing, planning and destruction.
    IfChain {
        arms: Vec<(Expr, Expr)>,
        fallback: Option<Box<Expr>>,
        span: Span,
    },
    /// Left fold of operators at one precedence level.
    BinaryChain {
        first: Box<Expr>,
        rest: Vec<(BinOp, Expr)>,
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
    /// Lexical C call permission; borrowing remains checked.
    Unsafe {
        exprs: Vec<Expr>,
        span: Span,
    },
    /// A block evaluates to its final expression.
    Block {
        exprs: Vec<Expr>,
        span: Span,
    },
    /// A checked construction group, publishing only its sealed root.
    Graph {
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
    /// `join { | branch1 | branch2 ... }` or `race { | branch1 | branch2 ... }`.
    Concurrent {
        kind: ConcurrentKind,
        branches: Vec<Expr>,
        span: Span,
    },
    /// A sequential task scope: the body runs inline and may register child
    /// tasks with `branch`; the scope completes after the body and all children.
    Isolate {
        body: Box<Expr>,
        span: Span,
    },
    TaskScope {
        body: Box<Expr>,
        span: Span,
    },
    /// Register a child task running `body` in the innermost enclosing task
    /// scope; the expression returns Unit immediately.
    Branch {
        body: Box<Expr>,
        span: Span,
    },
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct When {
    /// Set only by checked handle-method lowering, never parsed.
    pub(crate) method: Option<String>,
    pub label: Option<String>,
    pub accesses: Vec<(String, bool)>,
    pub paths: Vec<Option<WhenPath>>,
    pub missing: Option<Expr>,
    pub alias: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct WhenPath {
    pub create: Option<Expr>,
    pub ty: Type,
    pub directory: Expr,
    pub key: Expr,
    pub optional: bool,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct AnonymousFunction {
    pub moving: bool,
    pub suspends: bool,
    pub params: Vec<ClosureParameter>,
    pub ret: Option<Type>,
    pub body: Expr,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct ClosureParameter {
    pub mutable: bool,
    pub name: String,
    pub ty: Option<Type>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct IndexedLoop {
    pub name: String,
    pub counter: String,
    pub overflow: String,
    pub body: Expr,
    pub span: Span,
}

impl Expr {
    pub const fn span(&self) -> Span {
        match self {
            Self::IndexedLoop(loop_) => loop_.span,
            Self::When(block) => block.span,
            Self::AnonymousFunction(function) => function.span,
            Self::GenericName(name) => name.span,
            Self::MethodCall(call) => call.span,
            Self::Index { span, .. }
            | Self::Array { span, .. }
            | Self::Spread { span, .. }
            | Self::Tuple { span, .. }
            | Self::Destructure { span, .. }
            | Self::Match { span, .. }
            | Self::Record { span, .. }
            | Self::Field { span, .. }
            | Self::FieldAssign { span, .. }
            | Self::Literal { span, .. }
            | Self::Ident { span, .. }
            | Self::FunctionRef { span, .. }
            | Self::Invoke { span, .. }
            | Self::Call { span, .. }
            | Self::Let { span, .. }
            | Self::Var { span, .. }
            | Self::Assign { span, .. }
            | Self::While { span, .. }
            | Self::Return { span, .. }
            | Self::Break { span }
            | Self::Continue { span }
            | Self::IfChain { span, .. }
            | Self::BinaryChain { span, .. }
            | Self::If { span, .. }
            | Self::Logical { span, .. }
            | Self::Unary { span, .. }
            | Self::BinOp { span, .. }
            | Self::Unsafe { span, .. }
            | Self::Block { span, .. }
            | Self::Graph { span, .. }
            | Self::Sequence { span, .. }
            | Self::Concurrent { span, .. }
            | Self::Isolate { span, .. }
            | Self::TaskScope { span, .. }
            | Self::Branch { span, .. } => *span,
        }
    }

    pub(crate) fn with_span(self, span: Span) -> Self {
        match self {
            Self::IndexedLoop(mut loop_) => {
                loop_.span = span;
                Self::IndexedLoop(loop_)
            }
            Self::When(mut block) => {
                block.span = span;
                Self::When(block)
            }
            Self::AnonymousFunction(mut function) => {
                function.span = span;
                Self::AnonymousFunction(function)
            }
            Self::GenericName(mut name) => {
                name.span = span;
                Self::GenericName(name)
            }
            Self::MethodCall(mut call) => {
                call.span = span;
                Self::MethodCall(call)
            }
            Self::Invoke { callee, args, .. } => Self::Invoke { callee, args, span },
            Self::Index { base, index, .. } => Self::Index { base, index, span },
            Self::Array { elements, .. } => Self::Array { elements, span },
            Self::Spread { value, .. } => Self::Spread { value, span },
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
            Self::Match {
                value,
                arms,
                conditional,
                iteration,
                ..
            } => Self::Match {
                value,
                arms,
                conditional,
                iteration,
                span,
            },
            Self::Record {
                name,
                type_args,
                fields,
                ..
            } => Self::Record {
                name,
                type_args,
                fields,
                span,
            },
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
            Self::FieldAssign {
                target, value, op, ..
            } => Self::FieldAssign {
                op,
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
            Self::BinaryChain { first, rest, .. } => Self::BinaryChain { first, rest, span },
            Self::IfChain { arms, fallback, .. } => Self::IfChain {
                arms,
                fallback,
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
            Self::Assign {
                name, value, op, ..
            } => Self::Assign {
                name,
                value,
                op,
                span,
            },
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
            Self::Unsafe { exprs, .. } => Self::Unsafe { exprs, span },
            Self::Block { exprs, .. } => Self::Block { exprs, span },
            Self::Graph { exprs, .. } => Self::Graph { exprs, span },
            Self::Sequence {
                exprs, trailing, ..
            } => Self::Sequence {
                exprs,
                trailing,
                span,
            },
            Self::Concurrent { kind, branches, .. } => Self::Concurrent {
                kind,
                branches,
                span,
            },
            Self::Isolate { body, .. } => Self::Isolate { body, span },
            Self::TaskScope { body, .. } => Self::TaskScope { body, span },
            Self::Branch { body, .. } => Self::Branch { body, span },
        }
    }
}

/// A method receiver is evaluated once, before its explicit arguments.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct MethodCall {
    pub receiver: Expr,
    pub name: String,
    pub type_args: Vec<GenericArgument>,
    pub args: Vec<Expr>,
    pub span: Span,
}

/// An inherent or trait implementation for a concrete or full generic nominal type.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct ImplDef {
    /// Original fields/payloads for compiler-generated conditional derive bounds.
    pub(crate) derive_fields: Option<Vec<Param>>,
    pub trait_name: Option<String>,
    pub(crate) origin: Option<String>,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub target: Type,
    pub target_name: String,
    pub associated_types: Vec<AssociatedTypeDef>,
    pub methods: Vec<FnDef>,
    pub constants: Vec<AssociatedConst>,
    pub span: Span,
}

/// A static capability. Self is a rigid parameter in requirement signatures.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TraitDef {
    pub(crate) is_drop: bool,
    pub(crate) is_add: bool,
    pub public: bool,
    pub name: String,
    pub self_parameter: Arc<TypeParameter>,
    pub associated_types: Vec<AssociatedTypeDecl>,
    pub methods: Vec<TraitMethod>,
    pub span: Span,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
}

/// A trait signature with an optional default implementation. Empty bodies are real bodies.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub name_span: Span,
    pub signature_span: Span,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    pub noescape: Option<Box<NoEscape>>,
    pub body: Option<Expr>,
    pub span: Span,
}

impl TraitMethod {
    pub(crate) fn definition(&self) -> FnDef {
        FnDef {
            every: None,
            kind: crate::parser::ast::FunctionKind::Ordinary,
            name: self.name.clone(),
            params: self.params.clone(),
            ret_ty: self.ret_ty.clone(),
            suspends: self.suspends,
            noescape: self.noescape.clone(),
            body: self.body.clone().unwrap_or(Expr::Block {
                exprs: Vec::new(),
                span: self.span,
            }),
            span: self.span,
            public: false,
            type_params: Vec::new(),
            scope: None,
            buffer_intrinsic: None,
            compiler_intrinsic: None,
            buffer_element: None,
        }
    }
}

/// A typed scalar constant, evaluated before function planning.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct AssociatedConst {
    pub public: bool,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
    pub name: String,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum UnaryOp {
    /// Postfix extraction; lowered through enum patterns before backend selection.
    Unwrap,
    /// Postfix extraction with an early return of the failure variant.
    Try,
    Neg,
    Not,
    BitNot,
    /// Borrow an existing local place; lowered before arithmetic instructions.
    Borrow,
    BorrowMut,
    /// Access a checked reference place; lowered before arithmetic instructions.
    Deref,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
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
        matches!(self, Self::Div | Self::Rem | Self::Shl | Self::Shr)
    }

    pub(crate) fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }
}

/// Function parameter.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct Param {
    /// Whether the callee may mutate this binding; not part of its type.
    pub mutable: bool,
    pub name: String,
    pub ty: Type,
    /// UTF-8 byte range covering the parameter declaration.
    pub span: Span,
}

/// An imported module path.
#[derive(joky_image_derive::Image, Debug, Clone)]
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
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct ImportMember {
    pub name: String,
    pub kind: ImportMemberKind,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMemberKind {
    Type,
    Value,
}

/// Top-level declaration.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub enum Item {
    /// `fn name(params) @suspends? -> RetType { body }`
    FnDef(FnDef),
    /// `extern fn name(params) -> RetType`
    ExternFn(ExternFnDecl),
}

#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FunctionKind {
    Ordinary,
    Test,
    JsonEncode,
    Clone,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct FnDef {
    /// Positive nanosecond period for an automatically scheduled state method.
    pub every: Option<std::num::NonZeroU64>,
    /// Ordinary, test-only, or generated body requiring derive-specific borrows.
    pub(crate) kind: FunctionKind,
    /// Checked promise that borrowed arguments do not escape this call.
    pub noescape: Option<Box<NoEscape>>,
    pub public: bool,
    /// Set only on compiler-bound definitions; never parsed from source.
    pub(crate) buffer_intrinsic: Option<BufferIntrinsic>,
    pub(crate) compiler_intrinsic: Option<CompilerIntrinsic>,
    /// Compiler-owned element argument, retained even by layout-only primitives.
    pub(crate) buffer_element: Option<Box<Type>>,
    /// Imported functions retain the namespace in which they were defined.
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    pub body: Expr,
    /// UTF-8 byte range covering this declaration.
    pub span: Span,
}

impl FnDef {
    /// Whether source resolution must retain this function only for testing.
    pub fn is_test(&self) -> bool {
        self.kind == FunctionKind::Test
    }
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub(crate) struct IntrinsicAttribute {
    pub name: String,
    pub has_body: bool,
    pub span: Span,
}

/// A source-level synchronous borrowing contract, inherited by trait impls.
#[derive(joky_image_derive::Image, Debug, Clone, Default)]
pub struct NoEscape {
    pub all: bool,
    pub params: Vec<String>,
}

/// Unary float primitives that must stay bit-identical across backends and
/// cannot be composed from Joky arithmetic without losing IEEE exactness.
#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatKind {
    /// Parse a bounded decimal String borrow; invalid input yields NaN.
    FromText,
    FromText32,
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    /// Round to the nearest integer, ties to even.
    RoundTiesEven,
    /// Reinterpret the float's exact bit pattern as a same-width integer.
    ToBits,
    /// Reinterpret integer bits as a float of the matching width.
    FromBits,
}

/// Read access may carry an input independently of its mutation capability.
#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DomainAccessKind {
    Read,
    Query,
    Write,
}

/// Identities installed on trusted embedded definitions, never parsed from text.
/// Rare type payloads are boxed to keep ordinary function declarations compact.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub(crate) enum CompilerIntrinsic {
    /// Compiler-owned container adapters may return a checked fat dyn borrow.
    DynBorrowAdapter,
    OutboxEmit,
    FixedArrayBegin,
    FixedArrayWriteNext,
    FixedArrayFinish,
    FixedArrayLoad,
    FixedArrayStore,
    FixedArrayBorrow,
    FixedArraySlice,
    ArenaNew,
    ArenaAlloc,
    ScriptSchema(Box<Type>),
    ScriptPack,
    DomainValuePack,
    DomainValueUnpack,
    ScriptUnpack,
    DomainAccess(DomainAccessKind),
    DomainModule,
    DomainWhenFinish,
    DomainPathsFinish,
    DomainReadOnly,
    DomainCreate,
    DomainCreateFinish,
    TimerStart(bool),
    TimerFinish,
    TimerValueFinish,
    DirectoryType(Box<Type>),
    DirectoryWitness,
    DirectoryRemoveIf,
    DirectoryRemoveIfFinish,
    DomainSnapshot,
    DomainSnapshotPair,
    DomainSnapshotValue,
    DomainSetReadFinish,
    DomainCandidate,
    DomainCandidateFinish,
    DomainCandidateValueFinish,
    DomainCandidatePair,
    DomainCandidatePairFinish,
    DomainCandidatePairValueFinish,
    DomainSetRead,
    DomainSetCandidate(bool),
    DomainSetEdit(bool),
    DomainSetEditFinish(bool),
    DomainSetFinish(bool),
    DomainSetValueFinish(bool),
    DomainAt(DomainAccessKind),
    DomainPair(DomainAccessKind),
    DomainRequest,
    DomainFinish,
    DomainRequestValueFinish,
    DomainValueFinish(bool),
    DomainScalarFinish(bool),
    DomainPack,
    DomainUnpack,
    MemDrop,
    MemReplace,
    MemSwap,
    Assert,
    Panic,
    PlainCopy(Box<Type>),
    ReferenceFree(Box<Type>),
    /// Only the trusted Rune.from_u32 implementation can construct this scalar.
    RuneFromValidU32,
    TextClone,
    TextFreeze,
    TextIntoBytes,
    TextBytes,
    /// Backend-shared IEEE float primitive with an exact width.
    Float(FloatKind),
    /// Locale-independent float formatting into an owned String.
    FloatToText,
    /// Trusted float-to-integer truncation; the caller has range-checked the
    /// rounded value against the target's exact power-of-two bounds.
    IntFromFloat,
}

#[derive(joky_image_derive::Image, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferIntrinsic {
    Allocate,
    Transfer,
    AppendBytes,
    FindByte,
    Utf8Prefix,
    RandomFill,
    Sha256,
    HmacSha256,
    VerifyHmacSha256,
    Sha256New,
    HmacSha256New,
    Sha256Update,
    HmacSha256Update,
    Sha256Finish,
    HmacSha256Finish,
    HmacSha256Verify,
    PasswordNeedsRehash,
    TakeLast,
    SwapElements,
    Len,
    Capacity,
    CapacityLimit,
    Load,
    Store,
    InitializeNext,
    Truncate,
    Replace,
    Release,
    Borrow,
    BorrowRef,
    BorrowMutRef,
    BorrowMut,
    Slice,
    SliceMut,
    ElementLoad,
    ElementStore,
    SliceLen,
    SliceLoad,
    SliceStore,
    SliceBorrow,
    SliceBorrowMut,
    SliceBorrowRef,
    SliceBorrowMutRef,
    Subslice,
    SubsliceMut,
    Iter,
    IterMut,
    SliceIter,
    SliceIterMut,
    IterAdvance,
    IterCurrent,
    IterCurrentMut,
    PairIter,
    PairIterMut,
    PairAdvance,
    PairCurrent,
    PairCurrentMut,
    EntryKey,
    EntryValue,
    EntryValueMut,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct AssociatedTypeDecl {
    pub name: String,
    pub bounds: Vec<TraitBound>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct AssociatedTypeDef {
    pub name: String,
    pub ty: Type,
    pub span: Span,
    pub target_span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct ExternFnDecl {
    /// A synchronous, explicitly unsafe C borrowing boundary.
    pub c_abi: bool,
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub suspends: bool,
    /// UTF-8 byte range covering this declaration.
    pub span: Span,
}

/// A module-level scalar constant, evaluated before function planning.
///
/// Module constants carry no receiver type, so they stay a separate list
/// instead of joining `items`, whose entries all become function signatures.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct ModuleConst {
    pub public: bool,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
    pub name: String,
    /// The declaring module's canonical path, stamped when the module is
    /// prepared. It turns the constant name into a declaration identity that
    /// survives imports, aliases and dependency appending.
    pub(crate) path: Option<String>,
    pub ty: Type,
    pub value: Expr,
    /// UTF-8 byte range covering this declaration.
    pub span: Span,
}

/// Parsed source module.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct Module {
    /// Stable publication namespace, independent of source file locations.
    pub logical_name: String,
    /// Item-indexed source annotations, consumed by the trusted library loader.
    pub(crate) intrinsic_attributes: Vec<(usize, Box<IntrinsicAttribute>)>,
    pub(crate) sources: Arc<crate::SourceMap>,
    pub imports: Vec<Import>,
    /// Bound imports survive normalization without exposing private declarations.
    pub(crate) scope: Option<Arc<crate::core::imports::ImportScope>>,
    pub type_aliases: Vec<TypeAlias>,
    pub records: Vec<RecordDef>,
    pub enums: Vec<EnumDef>,
    pub implementations: Vec<ImplDef>,
    pub traits: Vec<TraitDef>,
    pub constants: Vec<ModuleConst>,
    pub items: Vec<Item>,
}

impl Module {
    pub(crate) fn expression_roots(&self) -> impl Iterator<Item = &Expr> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::FnDef(f) => Some(&f.body),
                Item::ExternFn(_) => None,
            })
            .chain(self.implementations.iter().flat_map(|implementation| {
                implementation
                    .methods
                    .iter()
                    .map(|method| &method.body)
                    .chain(
                        implementation
                            .constants
                            .iter()
                            .map(|constant| &constant.value),
                    )
            }))
            .chain(self.traits.iter().flat_map(|definition| {
                definition
                    .methods
                    .iter()
                    .filter_map(|method| method.body.as_ref())
            }))
            .chain(self.constants.iter().map(|constant| &constant.value))
    }

    /// Source snapshots used by diagnostics, including dependencies.
    pub fn sources(&self) -> &crate::SourceMap {
        &self.sources
    }
}

/// A transparent alias, optionally parameterized as `type Name<T> = Target`.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct TypeAlias {
    pub public: bool,
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub target: Type,
    pub name_span: Span,
    pub target_span: Span,
    pub span: Span,
}

/// One named field initializer, with shorthand expanded to its value expression.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

/// Structured declaration metadata, including #repr(C) and JSON #derive.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct Attribute {
    pub path: String,
    pub arguments: Option<Vec<AttributeArgument>>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct AttributeArgument {
    pub name: Option<String>,
    pub value: AttributeValue,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub enum AttributeValue {
    /// Exact nanosecond count, distinct from untyped integer metadata.
    Duration(u64),
    Literal(Literal),
    Name(Attribute),
}

/// A parsed record declaration. Clones preserve its nominal identity.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct RecordDef {
    pub(crate) array: bool,
    pub(crate) list: bool,
    pub(crate) map: bool,
    pub(crate) set: bool,
    pub attributes: Vec<Attribute>,
    pub public: bool,
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub fields: Vec<Param>,
    /// Fields explicitly marked `pub`; unmarked fields are module-private.
    pub public_fields: Vec<String>,
    pub(crate) imported: bool,
    pub name_span: Span,
    pub span: Span,
    pub(crate) id: u64,
}

impl RecordDef {
    /// Attribute arguments are checked before this representation flag is used.
    pub(crate) fn repr_c(&self) -> bool {
        self.attributes
            .iter()
            .any(|attribute| attribute.path == "repr")
    }

    /// Create a fresh declaration identity; cloning preserves that identity.
    pub fn new(name: String, fields: Vec<Param>, name_span: Span, span: Span) -> Self {
        let id = fresh_type_id();
        Self {
            array: false,
            list: false,
            map: false,
            set: false,
            public: false,
            attributes: Vec::new(),
            name,
            type_params: Vec::new(),
            fields,
            public_fields: Vec::new(),
            imported: false,
            name_span,
            span,
            id,
        }
    }
}

/// A module-local nominal enum declaration with optional Copy type parameters.
#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct EnumDef {
    pub attributes: Vec<Attribute>,
    pub public: bool,
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
            public: false,
            attributes: Vec::new(),
            name,
            type_params: Vec::new(),
            variants,
            name_span,
            span,
            id: fresh_type_id(),
        }
    }
}

#[derive(joky_image_derive::Image, Debug, Clone)]
pub struct VariantDef {
    pub name: String,
    pub payload: Option<Type>,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub enum Pattern {
    Value(Box<PayloadPattern>),
    Wildcard {
        span: Span,
    },
    /// `Variant`, `Enum.Variant`, or either followed by a recursive payload pattern.
    Variant {
        name: String,
        binding: Option<Box<PayloadPattern>>,
        span: Span,
    },
}
impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Value(p) => p.span(),
            Self::Wildcard { span } | Self::Variant { span, .. } => *span,
        }
    }
}

/// One `field: pattern` entry of a record pattern; shorthand `field` is
/// desugared at parse time to `field: field`.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct RecordFieldPattern {
    pub name: String,
    pub pattern: BindingPattern,
    pub span: Span,
}

/// Irrefutable binding patterns; enum and literal patterns are excluded.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub enum BindingPattern {
    Sequence(Box<SequencePattern<BindingPattern>>),
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
    Record {
        inner: Box<RecordPattern>,
        span: Span,
    },
}
/// Head, type arguments, and field entries of a record pattern; boxed to keep
/// small binding patterns cheap to store inside expressions.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct RecordPattern {
    pub resolved: Option<Box<Type>>,
    pub name: String,
    pub type_args: Vec<GenericArgument>,
    pub fields: Vec<RecordFieldPattern>,
    /// `..` ignores every unlisted field; at most one, in final position.
    pub rest: bool,
}

impl BindingPattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Sequence(p) => p.span,
            Self::Name { span, .. }
            | Self::Discard { span }
            | Self::Tuple { span, .. }
            | Self::Record { span, .. } => *span,
        }
    }
}

/// Recursive match payload syntax. Name binds a value and Variant tests a constructor.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub enum PayloadPattern {
    Sequence(Box<SequencePattern<PayloadPattern>>),
    Literal {
        literal: Literal,
        span: Span,
    },
    Range {
        start: Literal,
        end: Literal,
        inclusive: bool,
        span: Span,
    },
    Name {
        name: String,
        span: Span,
    },
    Discard {
        span: Span,
    },
    Tuple {
        elements: Vec<PayloadPattern>,
        span: Span,
    },
    Variant {
        name: String,
        payload: Option<Box<PayloadPattern>>,
        span: Span,
    },
    Record {
        name: String,
        resolved: Option<Box<Type>>,
        type_args: Vec<GenericArgument>,
        fields: Vec<RecordFieldPattern>,
        rest: bool,
        span: Span,
    },
}
impl PayloadPattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Sequence(p) => p.span,
            Self::Literal { span, .. } | Self::Range { span, .. } => *span,
            Self::Name { span, .. }
            | Self::Discard { span }
            | Self::Tuple { span, .. }
            | Self::Variant { span, .. }
            | Self::Record { span, .. } => *span,
        }
    }
    pub(crate) fn names(&self, out: &mut Vec<String>) {
        match self {
            Self::Sequence(p) => {
                for item in p.prefix.iter().chain(&p.suffix) {
                    item.names(out);
                }
                if let Some(Some(name)) = &p.rest {
                    out.push(name.clone());
                }
            }
            Self::Literal { .. } | Self::Range { .. } => {}
            Self::Name { name, .. } => out.push(name.clone()),
            Self::Discard { .. } => {}
            Self::Tuple { elements, .. } => elements.iter().for_each(|element| element.names(out)),
            Self::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.names(out);
                }
            }
            Self::Record { fields, .. } => {
                fields
                    .iter()
                    .for_each(|field| pattern_names(&field.pattern, out));
            }
        }
    }
}

fn pattern_names(pattern: &BindingPattern, out: &mut Vec<String>) {
    match pattern {
        BindingPattern::Sequence(p) => {
            for item in p.prefix.iter().chain(&p.suffix) {
                pattern_names(item, out);
            }
            if let Some(Some(name)) = &p.rest {
                out.push(name.clone());
            }
        }
        BindingPattern::Name { name, .. } => out.push(name.clone()),
        BindingPattern::Discard { .. } => {}
        BindingPattern::Tuple { elements, .. } => elements
            .iter()
            .for_each(|element| pattern_names(element, out)),
        BindingPattern::Record { inner, .. } => inner
            .fields
            .iter()
            .for_each(|field| pattern_names(&field.pattern, out)),
    }
}

/// An explicitly instantiated name without call parentheses.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct GenericName {
    pub name: String,
    pub type_args: Vec<GenericArgument>,
    pub span: Span,
}

/// A single optional rest separates prefix and suffix patterns.
#[derive(joky_image_derive::Image, Debug, Clone, PartialEq)]
pub struct SequencePattern<P> {
    pub prefix: Vec<P>,
    pub suffix: Vec<P>,
    pub rest: Option<Option<String>>,
    pub span: Span,
}
impl Pattern {
    pub(crate) fn payload(&self) -> Option<&PayloadPattern> {
        match self {
            Self::Value(p) => Some(p),
            Self::Variant { binding, .. } => binding.as_deref(),
            _ => None,
        }
    }
    pub(crate) fn payload_mut(&mut self) -> Option<&mut PayloadPattern> {
        match self {
            Self::Value(p) => Some(p),
            Self::Variant { binding, .. } => binding.as_deref_mut(),
            _ => None,
        }
    }
}

impl Expr {
    /// Return the first over-deep node without recursing on caller-supplied syntax.
    pub(crate) fn excessive_depth(&self, limit: usize) -> Option<Span> {
        let mut pending = vec![(self, 1)];
        while let Some((node, depth)) = pending.pop() {
            if depth > limit {
                return Some(node.span());
            }
            pending.extend(
                node.children()
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth + 1)),
            );
        }
        None
    }

    /// Visit expression children in source order. Declaration scopes are handled by callers.
    pub(crate) fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::IndexedLoop(loop_) => vec![&loop_.body],
            Expr::When(w) => w
                .paths
                .iter()
                .flatten()
                .flat_map(|p| [&p.directory, &p.key].into_iter().chain(p.create.iter()))
                .chain(std::iter::once(&w.body))
                .chain(w.missing.iter())
                .chain(w.alias.iter())
                .collect(),
            Expr::AnonymousFunction(f) => vec![&f.body],
            Expr::Invoke { callee, args, .. } => {
                std::iter::once(callee.as_ref()).chain(args).collect()
            }
            Expr::MethodCall(c) => std::iter::once(&c.receiver).chain(&c.args).collect(),
            Expr::BinaryChain { first, rest, .. } => std::iter::once(first.as_ref())
                .chain(rest.iter().map(|(_, e)| e))
                .collect(),
            Expr::IfChain { arms, fallback, .. } => arms
                .iter()
                .flat_map(|(test, body)| [test, body])
                .chain(fallback.as_deref())
                .collect(),
            Expr::Call { args, .. } => args.iter().collect(),
            Expr::Let { value, .. }
            | Expr::Var { value, .. }
            | Expr::Assign { value, .. }
            | Expr::Destructure { value, .. }
            | Expr::Spread { value, .. }
            | Expr::Unary { operand: value, .. }
            | Expr::Field { base: value, .. }
            | Expr::Isolate { body: value, .. }
            | Expr::TaskScope { body: value, .. }
            | Expr::Branch { body: value, .. } => vec![value],
            Expr::Return { value, .. } => value.iter().map(Box::as_ref).collect(),
            Expr::Index {
                base: lhs,
                index: rhs,
                ..
            }
            | Expr::BinOp { lhs, rhs, .. }
            | Expr::Logical { lhs, rhs, .. } => vec![lhs, rhs],
            Expr::FieldAssign { target, value, .. } => vec![target, value],
            Expr::While {
                condition, body, ..
            } => vec![condition, body],
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => std::iter::once(condition.as_ref())
                .chain(std::iter::once(then_branch.as_ref()))
                .chain(else_branch.iter().map(Box::as_ref))
                .collect(),
            Expr::Match { value, arms, .. } => std::iter::once(value.as_ref())
                .chain(arms.iter().map(|a| &a.body))
                .collect(),
            Expr::Record { fields, .. } => fields.iter().map(|f| &f.value).collect(),
            Expr::Array { elements, .. } | Expr::Tuple { elements, .. } => {
                elements.iter().collect()
            }
            Expr::Unsafe { exprs, .. }
            | Expr::Block { exprs, .. }
            | Expr::Graph { exprs, .. }
            | Expr::Sequence { exprs, .. } => exprs.iter().collect(),
            Expr::Concurrent { branches, .. } => branches.iter().collect(),
            _ => Vec::new(),
        }
    }
}
