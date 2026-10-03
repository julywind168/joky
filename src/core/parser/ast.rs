//! Joky abstract syntax tree.
use crate::Span;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Completion policy for structured concurrent branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Join,
    Race,
}

impl ScopeKind {
    pub(crate) const fn keyword(self) -> &'static str {
        match self {
            Self::Join => "scope",
            Self::Race => "race",
        }
    }
}

/// Parsed type names; aliases are normalized before semantic and ABI checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// A non-capturing language function value with an explicit suspension effect.
    Function(Arc<FunctionType>),
    Int32,
    Int64,
    UInt8,
    UInt32,
    Rune,
    UInt64,
    String,
    /// Opaque, nullable C object pointer; copying does not transfer ownership.
    Ptr,
    Bool,
    Unit,
    /// An unresolved source type name or a registered opaque host type.
    Named(String),
    /// An unresolved application of a generic record, enum or alias.
    Applied(Arc<TypeApplication>),
    /// A rigid, declaration-local Copy type parameter; never an executable value tag.
    Parameter(Arc<TypeParameter>),
    /// A resolved module-local nominal record with a finite scalar layout.
    Record(Arc<RecordType>),
    /// A resolved nominal tagged union with Copy or owning payloads.
    Enum(Arc<EnumType>),
    /// A structural tuple, normalized before planning.
    Tuple(Arc<TupleType>),
    /// Task-local reference; crosses only an explicitly checked C borrowing boundary.
    Reference(Arc<ReferenceType>),
    /// Internal, task-bound buffer ownership. No source spelling or host ABI yet.
    #[doc(hidden)]
    Buffer(Arc<Type>),
    /// Borrow-only targets with no source-visible representation or owning value.
    #[doc(hidden)]
    BufferElement(Arc<Type>),
    #[doc(hidden)]
    BufferSlice(Arc<Type>),
}

/// Structural callable type; host signatures keep their separate calling contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionType {
    pub signature: crate::FunctionSignature,
    pub suspends: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceType {
    pub target: Type,
    pub mutable: bool,
}

impl Type {
    pub fn function(params: Vec<Type>, ret: Type) -> Self {
        Self::function_with_effect(params, ret, false)
    }
    pub fn function_with_effect(params: Vec<Type>, ret: Type, suspends: bool) -> Self {
        Self::Function(Arc::new(FunctionType {
            signature: crate::FunctionSignature::new(params, ret),
            suspends,
        }))
    }
    pub(crate) fn contains_functions(&self) -> bool {
        self.scalar_types().iter().any(|ty| {
            matches!(ty, Self::Function(_))
                || ty
                    .reference_type()
                    .is_some_and(|r| r.target.contains_functions())
        })
    }
    pub(crate) fn is_buffer(&self) -> bool {
        matches!(self, Self::Buffer(_))
    }
    /// A movable buffer capability, including aggregates with owning contents.
    pub(crate) fn is_owner(&self) -> bool {
        self.is_buffer()
            || matches!(self, Self::Record(r) if !r.owners.is_empty())
            || matches!(self, Self::Enum(e) if !e.owners.is_empty())
            || matches!(self, Self::Tuple(t) if !t.owners.is_empty())
    }
    /// Mixed records, enums and tuples borrow their inline storage. One-field wrapper chains
    /// retain the buffer handle representation used by Array.
    pub(crate) fn is_inline_owner(&self) -> bool {
        if !self.is_owner() {
            return false;
        }
        let mut ty = self;
        while let Self::Record(record) = ty {
            if record.fields.len() != 1 {
                return true;
            }
            ty = &record.fields[0].ty;
        }
        matches!(ty, Self::Enum(_) | Self::Tuple(_))
    }
    pub(crate) fn is_buffer_target(&self) -> bool {
        self.is_owner() || matches!(self, Self::BufferElement(_) | Self::BufferSlice(_))
    }
    pub(crate) fn is_buffer_view(&self) -> bool {
        self.reference_type()
            .is_some_and(|r| r.target.is_buffer_target())
    }
    pub(crate) fn with_buffer_element(&self, element: Type) -> Self {
        match self {
            Self::Buffer(_) => Self::Buffer(Arc::new(element)),
            Self::BufferElement(_) => Self::BufferElement(Arc::new(element)),
            Self::BufferSlice(_) => Self::BufferSlice(Arc::new(element)),
            _ => unreachable!("internal buffer target"),
        }
    }
    pub(crate) fn contains_buffers(&self) -> bool {
        self.is_buffer_target()
            || self
                .scalar_types()
                .iter()
                .any(|ty| ty.is_buffer_target() || ty.is_buffer_view())
    }
    /// Buffer views can be copied locally, but cannot be stored or generalized.
    pub(crate) fn is_copy_storage_value(&self) -> bool {
        self.is_copy_value() && !self.contains_buffers()
    }
    /// All underlying element types, including shallow nominal owner views.
    pub(crate) fn buffer_elements(&self) -> &[Arc<Type>] {
        match self.reference_type().map_or(self, |r| &r.target) {
            Self::Buffer(element) | Self::BufferElement(element) | Self::BufferSlice(element) => {
                std::slice::from_ref(element)
            }
            Self::Record(record) => &record.owners,
            Self::Enum(enumeration) => &enumeration.owners,
            Self::Tuple(tuple) => &tuple.owners,
            _ => &[],
        }
    }
    /// Copy elements may retain ordinary shared references. Owning aggregate
    /// elements move as a whole and recursively require reference-free contents.
    /// Rigid parameters are checked again after specialization.
    pub(crate) fn is_buffer_element(&self) -> bool {
        if self.is_owner() {
            // Nested arrays can share their element DAG many times. Validate
            // each aggregate identity once rather than recursively expanding it.
            let mut pending = vec![self];
            let mut seen = std::collections::HashSet::new();
            while let Some(ty) = pending.pop() {
                let identity = match ty {
                    Self::Record(record) => Some(Arc::as_ptr(record).cast::<()>()),
                    Self::Enum(enumeration) => Some(Arc::as_ptr(enumeration).cast::<()>()),
                    Self::Tuple(tuple) => Some(Arc::as_ptr(tuple).cast::<()>()),
                    _ => None,
                };
                if identity.is_some_and(|identity| !seen.insert(identity)) {
                    continue;
                }
                if ty.is_owner() {
                    if !matches!(ty, Self::Record(_) | Self::Enum(_) | Self::Tuple(_))
                        || (!ty
                            .scalar_types()
                            .iter()
                            .any(|ty| matches!(ty, Self::Parameter(_)))
                            && ty.storage_layout().is_err())
                    {
                        return false;
                    }
                    pending.extend(ty.buffer_elements().iter().map(Arc::as_ref));
                } else if !ty.is_buffer_element() || ty.contains_references() {
                    return false;
                }
            }
            return true;
        }
        self.is_copy_storage_value()
            && self.scalar_types().iter().all(|ty| {
                matches!(
                    ty,
                    Self::Int32
                        | Self::Int64
                        | Self::UInt8
                        | Self::UInt32
                        | Self::Rune
                        | Self::UInt64
                        | Self::Bool
                        | Self::Ptr
                        | Self::Function(_)
                        | Self::Parameter(_)
                ) || ty
                    .reference_type()
                    .is_some_and(|r| !r.mutable && !r.target.contains_buffers())
            })
            && (self
                .scalar_types()
                .iter()
                .any(|ty| matches!(ty, Self::Parameter(_)))
                || self.memory_layout().is_ok())
    }
    pub fn reference(target: Type, mutable: bool) -> Self {
        Self::Reference(Arc::new(ReferenceType { target, mutable }))
    }
    /// Shared references are Copy values; exclusive capabilities never are.
    pub(crate) fn is_copy_value(&self) -> bool {
        self.is_copy_scalar()
            || self.is_aggregate() && !self.is_owner()
            || self.reference_type().is_some_and(|r| !r.mutable)
    }
    pub(crate) fn contains_references(&self) -> bool {
        self.scalar_types()
            .iter()
            .any(|ty| ty.reference_type().is_some())
    }
    pub(crate) fn reference_type(&self) -> Option<&ReferenceType> {
        match self {
            Self::Reference(r) => Some(r),
            _ => None,
        }
    }
}

/// Unresolved generic type application; boxed to keep typed IR slots compact.
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
                Type::Buffer(element)
                | Type::BufferElement(element)
                | Type::BufferSlice(element) => {
                    f.write_str(match ty {
                        Type::Buffer(_) => "Buffer<",
                        Type::BufferElement(_) => "Element<",
                        _ => "Slice<",
                    })?;
                    display(element, f, remaining)?;
                    f.write_str(">")
                }
                Type::Reference(r) => {
                    f.write_str(if r.mutable { "&mut " } else { "&" })?;
                    display(&r.target, f, remaining)
                }
                Type::Function(signature) => {
                    f.write_str("fn(")?;
                    for (i, param) in signature.signature.params.iter().enumerate() {
                        if i != 0 {
                            f.write_str(", ")?;
                        }
                        display(param, f, remaining)?;
                    }
                    f.write_str(if signature.suspends {
                        ") @suspends -> "
                    } else {
                        ") -> "
                    })?;
                    display(&signature.signature.ret, f, remaining)
                }
                Type::Int32 => f.write_str("Int32"),
                Type::Int64 => f.write_str("Int64"),
                Type::UInt8 => f.write_str("UInt8"),
                Type::UInt32 => f.write_str("UInt32"),
                Type::Rune => f.write_str("Rune"),
                Type::UInt64 => f.write_str("UInt64"),
                Type::String => f.write_str("String"),
                Type::Ptr => f.write_str("Ptr"),
                Type::Bool => f.write_str("Bool"),
                Type::Unit => f.write_str("Unit"),
                Type::Named(name) => f.write_str(name),
                Type::Parameter(parameter) => f.write_str(&parameter.name),
                Type::Record(record) => {
                    f.write_str(&record.name)?;
                    arguments(&record.arguments, f, remaining)
                }
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
    UInt8(u8),
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
        type_args: Vec<Type>,
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
    /// `scope { | branch1 | branch2 ... }` or `race { | branch1 | branch2 ... }`.
    Scope {
        kind: ScopeKind,
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
            | Self::Invoke { span, .. }
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
            | Self::Unsafe { span, .. }
            | Self::Block { span, .. }
            | Self::Graph { span, .. }
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
            Self::Invoke { callee, args, .. } => Self::Invoke { callee, args, span },
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
            Self::Scope { kind, branches, .. } => Self::Scope {
                kind,
                branches,
                span,
            },
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

/// An inherent or trait implementation for a concrete or full generic nominal type.
#[derive(Debug, Clone)]
pub struct ImplDef {
    pub trait_name: Option<String>,
    pub(crate) origin: Option<String>,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub target: Type,
    pub target_name: String,
    pub methods: Vec<FnDef>,
    pub constants: Vec<AssociatedConst>,
    pub span: Span,
}

/// A static capability. Self is a rigid parameter in requirement signatures.
#[derive(Debug, Clone)]
pub struct TraitDef {
    pub(crate) is_drop: bool,
    pub public: bool,
    pub name: String,
    pub self_parameter: Arc<TypeParameter>,
    pub methods: Vec<FnDef>,
    pub span: Span,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
}

/// A typed scalar constant, evaluated before function planning.
#[derive(Debug, Clone)]
pub struct AssociatedConst {
    pub public: bool,
    pub(crate) scope: Option<Arc<crate::core::imports::DefinitionScope>>,
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
    /// Borrow an existing local place; lowered before arithmetic instructions.
    Borrow,
    BorrowMut,
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
    pub public: bool,
    /// Set only on compiler-bound definitions; never parsed from source.
    pub(crate) buffer_intrinsic: Option<BufferIntrinsic>,
    pub(crate) compiler_intrinsic: Option<CompilerIntrinsic>,
    /// Compiler-owned element argument, retained even by layout-only primitives.
    pub(crate) buffer_element: Option<Type>,
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

/// Identities installed on trusted embedded definitions, never parsed from text.
#[derive(Debug, Clone)]
pub(crate) enum CompilerIntrinsic {
    Require,
    PlainCopy(Type),
    /// Only the trusted Rune.from_u32 implementation can construct this scalar.
    RuneFromValidU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferIntrinsic {
    Allocate,
    Transfer,
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
}

/// A distinct rigid parameter owned by a generic declaration or trait Self.
#[derive(Debug, Clone)]
pub struct TypeParameter {
    pub name: String,
    pub bounds: Vec<String>,
    pub span: Span,
    pub(crate) id: u64,
}
impl TypeParameter {
    pub fn new(name: String, span: Span) -> Self {
        Self {
            name,
            span,
            id: fresh_type_id(),
            bounds: Vec::new(),
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
    /// A synchronous, explicitly unsafe C borrowing boundary.
    pub c_abi: bool,
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
    pub(crate) sources: Arc<crate::SourceMap>,
    pub imports: Vec<Import>,
    /// Bound imports survive normalization without exposing private declarations.
    pub(crate) scope: Option<Arc<crate::core::imports::ImportScope>>,
    pub type_aliases: Vec<TypeAlias>,
    pub records: Vec<RecordDef>,
    pub enums: Vec<EnumDef>,
    pub implementations: Vec<ImplDef>,
    pub traits: Vec<TraitDef>,
    pub items: Vec<Item>,
}

impl Module {
    /// Source snapshots used by diagnostics, including dependencies.
    pub fn sources(&self) -> &crate::SourceMap {
        &self.sources
    }
}

/// A transparent alias, optionally parameterized as `type Name<T> = Target`.
#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub public: bool,
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
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

/// Structured declaration metadata. Only #repr(C) is currently accepted.
#[derive(Debug, Clone)]
pub struct Attribute {
    pub path: String,
    pub arguments: Option<Vec<AttributeArgument>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct AttributeArgument {
    pub name: Option<String>,
    pub value: AttributeValue,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum AttributeValue {
    Literal(Literal),
    Name(Attribute),
}

/// A parsed record declaration. Clones preserve its nominal identity.
#[derive(Debug, Clone)]
pub struct RecordDef {
    pub attributes: Vec<Attribute>,
    pub public: bool,
    pub name: String,
    pub type_params: Vec<Arc<TypeParameter>>,
    pub fields: Vec<Param>,
    /// Explicitly private fields; unmarked fields retain their existing visibility.
    pub private_fields: Vec<String>,
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
            public: false,
            attributes: Vec::new(),
            name,
            type_params: Vec::new(),
            fields,
            private_fields: Vec::new(),
            imported: false,
            name_span,
            span,
            id,
        }
    }
}

/// A resolved nominal type. Only repr_c records promise a C memory layout.
pub struct RecordType {
    /// Underlying buffer elements in declaration order, retained in shallow views.
    pub(crate) owners: Vec<Arc<Type>>,
    pub(crate) repr_c: bool,
    pub(crate) indirect: bool,
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) parameters: Vec<Arc<TypeParameter>>,
    pub(crate) arguments: Vec<Type>,
    pub(crate) template: Option<Arc<RecordType>>,
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
            && self.arguments.len() == other.arguments.len()
            && equal_types(self.arguments.iter().zip(&other.arguments))
    }
}
impl Eq for RecordType {}

#[derive(Debug)]
pub(crate) struct RecordField {
    pub private: bool,
    pub name: String,
    pub ty: Type,
    /// Offset in the flattened scalar slots, never a byte offset.
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
            Self::Function(signature) => signature.signature.type_depth(),
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
    pub(crate) owners: Vec<Arc<Type>>,
    pub(crate) indirect: bool,
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
    /// Payload offset in the flattened scalar slots, never a byte offset.
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
            Self::Wildcard { span } | Self::Variant { span, .. } => *span,
        }
    }
}

/// Tuple syntax and canonical layout. Equality compares ordered element types.
pub struct TupleType {
    pub(crate) elements: Vec<Type>,
    pub(crate) owners: Vec<Arc<Type>>,
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

// Aggregate arguments can form shared DAGs, including phantom types.
// Compare all kinds in one traversal instead of recursively expanding each edge.
pub(crate) fn equal_types<'a>(pairs: impl Iterator<Item = (&'a Type, &'a Type)>) -> bool {
    let mut pending: Vec<_> = pairs.collect();
    let mut tuples = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    let mut records = std::collections::HashSet::new();
    let mut functions = std::collections::HashSet::new();
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (Type::Function(a), Type::Function(b)) => {
                if a.suspends != b.suspends || a.signature.params.len() != b.signature.params.len()
                {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && functions.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.signature.params.iter().zip(&b.signature.params));
                    pending.push((&a.signature.ret, &b.signature.ret));
                }
            }
            (Type::Reference(a), Type::Reference(b)) => {
                if a.mutable != b.mutable {
                    return false;
                }
                pending.push((&a.target, &b.target));
            }
            (Type::Record(a), Type::Record(b)) => {
                if a.id != b.id || a.arguments.len() != b.arguments.len() {
                    return false;
                }
                if !Arc::ptr_eq(a, b) && records.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
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
    /// Build a tuple type annotation. Resolution checks nonempty Copy or owning elements
    /// and computes its layout before compilation; this is not a host ABI type.
    pub fn tuple(elements: Vec<Type>) -> Self {
        Self::Tuple(Arc::new(TupleType {
            elements,
            owners: Vec::new(),
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

/// Recursive match payload syntax; bare names are resolved using the payload type.
#[derive(Debug, Clone)]
pub enum PayloadPattern {
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
}
impl PayloadPattern {
    pub fn span(&self) -> Span {
        match self {
            Self::Name { span, .. }
            | Self::Discard { span }
            | Self::Tuple { span, .. }
            | Self::Variant { span, .. } => *span,
        }
    }
    pub(crate) fn names(&self, out: &mut Vec<String>) {
        match self {
            Self::Name { name, .. } => out.push(name.clone()),
            Self::Discard { .. } => {}
            Self::Tuple { elements, .. } => elements.iter().for_each(|element| element.names(out)),
            Self::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.names(out);
                }
            }
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
