use crate::Span;

/// Unique identifier for an AST node
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// Create a new NodeId
    pub(crate) const fn new(id: u32) -> Self {
        Self(id)
    }
}

/// NodeId generator
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(crate) struct NodeIdGenerator {
    next_id: u32,
}

impl NodeIdGenerator {
    /// Create a new generator
    pub(crate) fn new() -> Self {
        Self { next_id: 0 }
    }

    pub(crate) fn starting_at(next_id: u32) -> Self {
        Self { next_id }
    }

    /// Generate the next NodeId
    pub(crate) fn next(&mut self) -> NodeId {
        let id = NodeId::new(self.next_id);
        self.next_id += 1;
        id
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Program {
    pub imports: Vec<Import>,
    pub constants: Vec<Constant>,
    pub traits: Vec<Trait>,
    pub impls: Vec<Impl>,
    pub structs: Vec<Struct>,
    pub classes: Vec<Class>,
    pub enums: Vec<Enum>,
    pub effects: Vec<Effect>,
    pub intrinsic_types: Vec<IntrinsicType>,
    pub functions: Vec<Function>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct IntrinsicType {
    pub name: String,
    pub kind: IntrinsicTypeKind,
    /// Methods forward to same-named operations in this effect group.
    pub effect: Option<String>,
    pub type_parameters: Vec<TypeParameter>,
    pub methods: Vec<IntrinsicMethod>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicTypeKind {
    Struct,
    Class,
}

impl IntrinsicTypeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Class => "class",
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct IntrinsicMethod {
    pub name: String,
    /// Optional operation alias within the declared effect group.
    pub operation: Option<String>,
    pub receiver_mode: ReceiverMode,
    pub type_parameters: Vec<TypeParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: TypeAnnotation,
    pub where_predicates: Vec<WherePredicate>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct WherePredicate {
    pub parameter: String,
    #[serde(default)]
    pub associated: Option<String>,
    pub bounds: Vec<TypeAnnotation>,
    pub span: Span,
}

/// Ownership mode of a method receiver. Class receivers borrow by default;
/// `self` is reserved for consuming operations such as `close`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverMode {
    Borrowed,
    Owned,
    Static,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Trait {
    pub name: String,
    pub associated_types: Vec<TraitType>,
    pub methods: Vec<TraitMethod>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct TraitType {
    pub name: String,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct TraitMethod {
    pub name: String,
    pub receiver_mode: ReceiverMode,
    pub parameters: Vec<Parameter>,
    pub return_type: TypeAnnotation,
    pub effect_names: Vec<TypeAnnotation>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Impl {
    pub trait_name: String,
    pub type_name: String,
    pub associated_types: Vec<ImplType>,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ImplType {
    pub name: String,
    pub ty: TypeAnnotation,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Import {
    pub path: Vec<String>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Constant {
    pub name: String,
    pub visibility: Visibility,
    pub annotation: Option<TypeAnnotation>,
    pub value: Expr,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Enum {
    pub name: String,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<Parameter>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Struct {
    pub name: String,
    pub repr_c: bool,
    pub fields: Vec<StructField>,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub ty: TypeAnnotation,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Class {
    pub name: String,
    pub fields: Vec<ClassField>,
    pub methods: Vec<Function>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ClassField {
    pub name: String,
    pub mutable: bool,
    pub ty: TypeAnnotation,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub foreign: Option<ForeignFunction>,
    pub receiver_mode: Option<ReceiverMode>,
    pub visibility: Visibility,
    pub type_parameters: Vec<TypeParameter>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<TypeAnnotation>,
    pub effect_names: Vec<TypeAnnotation>,
    #[serde(default)]
    pub where_predicates: Vec<WherePredicate>,
    pub body: Expr,
    pub span: Span,
}

/// A synchronous C symbol. Addresses and live library handles are never serialized.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ForeignFunction {
    pub library: String,
    pub symbol: String,
    /// Optional target operating system selector (for example `macos`).
    pub target_os: Option<String>,
    /// Additional platform-specific bindings attached to the same function.
    #[serde(default)]
    pub alternatives: Vec<ForeignFunction>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Effect {
    pub name: String,
    pub operations: Vec<EffectOperation>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct EffectOperation {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: TypeAnnotation,
    pub mode: EffectMode,
    pub suspends: bool,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectMode {
    Normal,
    Resumable,
    Aborts,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct TypeParameter {
    pub name: String,
    pub bounds: Vec<TypeAnnotation>,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub borrowed: bool,
    pub ty: TypeAnnotation,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeAnnotation {
    pub kind: TypeExpr,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Name(String),
    Const(u64),
    Member {
        base: Box<TypeAnnotation>,
        name: String,
    },
    Apply {
        callee: Box<TypeAnnotation>,
        arguments: Vec<TypeArgument>,
    },
    Tuple(Vec<TypeAnnotation>),
    TraitComposition(Vec<TypeAnnotation>),
    Function {
        parameters: Vec<TypeArgument>,
        result: Box<TypeAnnotation>,
    },
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeArgument {
    pub label: Option<String>,
    pub value: TypeAnnotation,
}

impl TypeAnnotation {
    pub(crate) fn named(name: &str, span: Span) -> Self {
        Self {
            kind: TypeExpr::Name(name.to_owned()),
            span,
        }
    }

    pub(crate) fn as_name(&self) -> Option<&str> {
        match &self.kind {
            TypeExpr::Name(name) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn is_name(&self, name: &str) -> bool {
        self.as_name() == Some(name)
    }
}

impl std::fmt::Display for TypeAnnotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn arguments(f: &mut std::fmt::Formatter<'_>, args: &[TypeArgument]) -> std::fmt::Result {
            for (index, argument) in args.iter().enumerate() {
                if index > 0 {
                    write!(f, ",")?;
                }
                if let Some(label) = &argument.label {
                    write!(f, "{label}:")?;
                }
                write!(f, "{}", argument.value)?;
            }
            Ok(())
        }
        match &self.kind {
            TypeExpr::Name(name) => write!(f, "{name}"),
            TypeExpr::Const(value) => write!(f, "{value}"),
            TypeExpr::Member { base, name } => write!(f, "{base}.{name}"),
            TypeExpr::TraitComposition(traits) => {
                for (index, name) in traits.iter().enumerate() {
                    if index > 0 {
                        write!(f, " + ")?;
                    }
                    write!(f, "{name}")?;
                }
                Ok(())
            }
            TypeExpr::Apply {
                callee,
                arguments: args,
            } => {
                write!(f, "{callee}(")?;
                arguments(f, args)?;
                write!(f, ")")
            }
            TypeExpr::Tuple(elements) => {
                write!(f, "(")?;
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{element}")?;
                }
                if elements.len() == 1 {
                    write!(f, ",")?;
                }
                write!(f, ")")
            }
            TypeExpr::Function { parameters, result } => {
                write!(f, "fn(")?;
                arguments(f, parameters)?;
                write!(f, ")->{result}")
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    BitNot,
}

/// The mode of a cast expression. The lossless form is only accepted where
/// the target value range contains the source range; the checked form yields
/// a `Result`, the wrapping form truncates two's-complement low bits, and
/// the saturating form clamps the source value to the target range.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastMode {
    Lossless,
    Checked,
    Wrapping,
    Saturating,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    ShiftLeft,
    ShiftRight,
    BitAnd,
    BitXor,
    BitOr,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Arithmetic(ArithmeticOp, ArithmeticMode),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticMode {
    Panic,
    Checked,
    Wrapping,
    Saturating,
}

impl BinaryOp {
    pub(crate) const fn arithmetic(self) -> Option<(ArithmeticOp, ArithmeticMode)> {
        let op = match self {
            Self::Add => ArithmeticOp::Add,
            Self::Subtract => ArithmeticOp::Subtract,
            Self::Multiply => ArithmeticOp::Multiply,
            Self::Divide => ArithmeticOp::Divide,
            Self::Remainder => ArithmeticOp::Remainder,
            Self::Arithmetic(op, mode) => return Some((op, mode)),
            _ => return None,
        };
        Some((op, ArithmeticMode::Panic))
    }

    pub(crate) const fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Equal
                | Self::NotEqual
                | Self::Less
                | Self::LessEqual
                | Self::Greater
                | Self::GreaterEqual
        )
    }

    /// Shift and bitwise operators only accept integers
    pub(crate) const fn is_bitwise(self) -> bool {
        matches!(
            self,
            Self::ShiftLeft | Self::ShiftRight | Self::BitAnd | Self::BitXor | Self::BitOr
        )
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub(crate) const fn new(id: NodeId, kind: ExprKind, span: Span) -> Self {
        Self { id, kind, span }
    }
}

/// A fixed integer type written directly on a literal.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerSuffix {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntegerSuffix {
    pub(super) fn parse(suffix: &str) -> Option<Self> {
        match suffix {
            "i8" => Some(Self::I8),
            "i16" => Some(Self::I16),
            "i32" => Some(Self::I32),
            "i64" => Some(Self::I64),
            "u8" => Some(Self::U8),
            "u16" => Some(Self::U16),
            "u32" => Some(Self::U32),
            "u64" => Some(Self::U64),
            _ => None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum ExprKind {
    Integer(u64),
    TypedInteger(u64, IntegerSuffix),
    Float(f64),
    Duration(u64),
    String(String),
    /// Byte string literal (`b"…"`); ASCII-only contents, typed `Bytes`
    Bytes(String),
    /// Compile-time resource, resolved from the defining module snapshot.
    IncludeBytes {
        path: String,
        data: Option<Vec<u8>>,
    },
    /// Literal text and an optional expression.  The literal is the text
    /// preceding the expression, or the trailing text for the final part.
    InterpolatedString(Vec<(String, Option<Box<Expr>>)>),
    Boolean(bool),
    Name(String),
    Let {
        name: String,
        mutable: bool,
        annotation: Option<TypeAnnotation>,
        value: Box<Expr>,
    },
    LetPattern {
        pattern: Box<Pattern>,
        binding_ids: Vec<(String, NodeId)>,
        mutable: bool,
        value: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        expression: Box<Expr>,
    },
    Unwrap {
        value: Box<Expr>,
        propagate: bool,
    },
    Cast {
        value: Box<Expr>,
        mode: CastMode,
        target: TypeAnnotation,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        step: Option<Box<Expr>>,
        inclusive: bool,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<CallArgument>,
    },
    /// An anonymous function expression. `move_capture` opts into moving
    /// unique values into the closure environment.
    Closure {
        move_capture: bool,
        parameters: Vec<Parameter>,
        return_type: TypeAnnotation,
        body: Box<Expr>,
    },
    Tuple(Vec<Expr>),
    CollectionLiteral(CollectionLiteral),
    StructInit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    /// Anonymous struct type literal, only meaningful in compile-time type
    /// function bodies.
    AnonymousStruct {
        fields: Vec<StructField>,
    },
    /// Anonymous enum type literal, only meaningful in compile-time type
    /// function bodies.
    AnonymousEnum {
        variants: Vec<EnumVariant>,
    },
    Field {
        value: Box<Expr>,
        access: FieldAccess,
    },
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    CompoundAssign {
        target: Box<Expr>,
        operator: BinaryOp,
        value: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    Match {
        value: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// Collect the values of completed iterations; annotations select execution.
    For {
        index: Option<String>,
        item: String,
        iterable: Box<Expr>,
        body: Box<Expr>,
        limit: Option<Box<Expr>>,
    },
    While {
        condition: Box<Expr>,
        body: Box<Expr>,
    },
    Loop {
        body: Box<Expr>,
    },
    Break {
        value: Option<Box<Expr>>,
    },
    Abort {
        value: Box<Expr>,
    },
    Continue,
    When {
        cowns: Vec<Expr>,
        bindings: Option<Vec<Pattern>>,
        until: Option<Box<Expr>>,
        body: Box<Expr>,
    },
    Do {
        body: Box<Expr>,
        handlers: Vec<HandlerArm>,
    },
    Parallel(Vec<Expr>),
    Race(Vec<Expr>),
    Branch(Box<Expr>),
    Region(Box<Expr>),
    Block(Vec<Expr>),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum CollectionLiteral {
    List(Vec<Expr>),
    MutList(Vec<Expr>),
    Set(Vec<Expr>),
    MutSet(Vec<Expr>),
    Map(Vec<MapLiteralEntry>),
    MutMap(Vec<MapLiteralEntry>),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct MapLiteralEntry {
    pub key: Expr,
    pub value: Expr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct CallArgument {
    pub label: Option<String>,
    pub value: Expr,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct HandlerArm {
    pub id: NodeId,
    pub effect: String,
    pub operation: String,
    pub parameters: Vec<Pattern>,
    pub value: Expr,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub value: Expr,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    EnumVariant {
        enum_name: String,
        variant: String,
        fields: Vec<PatternField>,
        span: Span,
    },
    Binding {
        name: String,
        span: Span,
    },
    Tuple {
        elements: Vec<Pattern>,
        span: Span,
    },
}

impl Pattern {
    pub const fn span(&self) -> Span {
        match self {
            Self::Wildcard { span }
            | Self::EnumVariant { span, .. }
            | Self::Binding { span, .. }
            | Self::Tuple { span, .. } => *span,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PatternField {
    pub label: Option<String>,
    pub pattern: Pattern,
    pub span: Span,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum FieldAccess {
    Index(usize),
    Name(String),
}
