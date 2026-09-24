//! Specialized error types for each compiler stage

use crate::Span;

/// Lexing error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// Unexpected character
    UnexpectedCharacter { character: char, span: Span },
    /// Unterminated string literal
    UnterminatedString { span: Span },
    /// Unterminated block comment
    UnterminatedBlockComment { span: Span },
    /// Invalid escape sequence
    InvalidEscapeSequence { sequence: String, span: Span },
    /// A byte string literal contains a non-ASCII source character
    NonAsciiByteString { span: Span },
    /// Multiline string opener shares its line with other content
    MultilineStringOpenerNotAlone { span: Span },
    /// Multiline string closer is not on its own line
    MultilineStringCloserNotAlone { span: Span },
    /// A multiline string line is less indented than the baseline
    MultilineStringIndentMismatch { span: Span },
    /// Missing digits after a float exponent
    MissingExponentDigits { span: Span },
    /// Invalid float literal
    InvalidFloatLiteral { literal: String, span: Span },
    /// Float is out of range
    FloatOutOfRange { literal: String, span: Span },
    /// Integer is out of range
    IntegerOutOfRange { literal: String, span: Span },
    /// Invalid base or separator in a numeric literal
    InvalidNumericLiteral { literal: String, span: Span },
    /// Invalid duration literal
    InvalidDurationLiteral { literal: String, span: Span },
}

impl LexError {
    pub fn span(&self) -> Span {
        match self {
            Self::UnexpectedCharacter { span, .. }
            | Self::UnterminatedString { span }
            | Self::UnterminatedBlockComment { span }
            | Self::InvalidEscapeSequence { span, .. }
            | Self::NonAsciiByteString { span }
            | Self::MultilineStringOpenerNotAlone { span }
            | Self::MultilineStringCloserNotAlone { span }
            | Self::MultilineStringIndentMismatch { span }
            | Self::MissingExponentDigits { span }
            | Self::InvalidFloatLiteral { span, .. }
            | Self::FloatOutOfRange { span, .. }
            | Self::IntegerOutOfRange { span, .. }
            | Self::InvalidNumericLiteral { span, .. }
            | Self::InvalidDurationLiteral { span, .. } => *span,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::UnexpectedCharacter { character, .. } => {
                format!("unexpected character: '{character}'")
            }
            Self::UnterminatedString { .. } => "unterminated string literal".to_owned(),
            Self::UnterminatedBlockComment { .. } => "unterminated block comment".to_owned(),
            Self::InvalidEscapeSequence { sequence, .. } => {
                format!("invalid escape sequence: \\{sequence}")
            }
            Self::NonAsciiByteString { .. } => {
                "byte string literals may contain only ASCII characters".to_owned()
            }
            Self::MultilineStringOpenerNotAlone { .. } => {
                "multiline string must start on the line after \"\"\"".to_owned()
            }
            Self::MultilineStringCloserNotAlone { .. } => {
                "closing \"\"\" of a multiline string must be on its own line".to_owned()
            }
            Self::MultilineStringIndentMismatch { .. } => {
                "line is less indented than the closing \"\"\"".to_owned()
            }
            Self::MissingExponentDigits { .. } => "expected digits after float exponent".to_owned(),
            Self::InvalidFloatLiteral { literal, .. } => {
                format!("invalid float literal: {literal}")
            }
            Self::FloatOutOfRange { literal, .. } => {
                format!("float is out of range: {literal}")
            }
            Self::IntegerOutOfRange { literal, .. } => {
                format!("integer is out of range: {literal}")
            }
            Self::InvalidNumericLiteral { literal, .. } => {
                format!("invalid numeric literal: {literal}")
            }
            Self::InvalidDurationLiteral { literal, .. } => {
                format!("invalid duration literal: {literal}")
            }
        }
    }
}

/// Parsing error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Program is empty
    EmptyProgram,
    /// Expected token not found
    ExpectedToken {
        expected: String,
        span: Option<Span>,
    },
    /// Expression is not callable
    NotCallable { span: Span },
    /// Expected an expression
    ExpectedExpression { span: Option<Span> },
    /// Expected a function name
    ExpectedFunctionName { span: Span },
    /// Expression nesting exceeds the compiler-defined limit
    ExpressionTooDeep { limit: usize, span: Option<Span> },
}

impl ParseError {
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::EmptyProgram => None,
            Self::ExpectedToken { span, .. }
            | Self::ExpectedExpression { span }
            | Self::ExpressionTooDeep { span, .. } => *span,
            Self::NotCallable { span } | Self::ExpectedFunctionName { span } => Some(*span),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::EmptyProgram => "program is empty".to_owned(),
            Self::ExpectedToken { expected, .. } => expected.clone(),
            Self::NotCallable { .. } => "expression is not callable".to_owned(),
            Self::ExpectedExpression { .. } => "expected an expression".to_owned(),
            Self::ExpectedFunctionName { .. } => "expected a function name".to_owned(),
            Self::ExpressionTooDeep { limit, .. } => {
                format!("expression nesting exceeds {limit}")
            }
        }
    }
}

/// Semantic analysis error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticError {
    /// Unknown value
    UnknownValue {
        name: String,
        span: Span,
    },
    /// Unknown function
    UnknownFunction {
        name: String,
        span: Span,
    },
    AmbiguousMethod {
        name: String,
        candidates: Vec<String>,
        span: Span,
    },
    /// Unknown type
    UnknownType {
        name: String,
        span: Span,
    },
    /// Type mismatch
    TypeMismatch {
        expected: String,
        actual: String,
        span: Span,
    },
    /// Non-tail expression in a block produces a value that is never used
    UnusedValue {
        actual: String,
        span: Span,
    },
    /// Cannot negate an unsigned type
    CannotNegateUnsigned {
        type_name: String,
        span: Span,
    },
    /// Numeric operator requires numeric operands
    NumericOperatorRequiresNumeric {
        span: Span,
    },
    /// Division by zero
    DivisionByZero {
        span: Span,
    },
    /// Shift or bitwise operation requires integer operands
    BitwiseOperatorRequiresInteger {
        span: Span,
    },
    /// Minimum signed integer has no representable absolute value
    AbsoluteValueOverflow {
        span: Span,
    },
    /// Target of `to` is not a numeric type
    NumericConversionTarget {
        span: Span,
    },
    /// Ordered comparison requires PartialOrd operands of the same type
    OrderedComparisonRequiresPartialOrd {
        span: Span,
    },
    /// Comparison operands must have the same comparable type
    ComparisonTypeMismatch {
        span: Span,
    },
    /// Integer literal is out of range for the type
    IntegerLiteralOutOfRange {
        type_name: String,
        span: Span,
    },
    /// Float literal is out of range for the type
    FloatLiteralOutOfRange {
        span: Span,
    },
    /// Cannot negate the type
    CannotNegateType {
        type_name: String,
        span: Span,
    },
    /// Program must define a main function
    MissingMainFunction,
    /// Main function is defined more than once
    DuplicateMainFunction {
        span: Span,
    },
    /// Duplicate function definition
    DuplicateFunctionDefinition {
        name: String,
        span: Span,
    },
    DuplicateConstantDefinition {
        name: String,
        span: Span,
    },
    InvalidConstantExpression {
        span: Span,
    },
    ConstantDependencyCycle {
        name: String,
        span: Span,
    },
    DuplicateTypeParameter {
        name: String,
        span: Span,
    },
    UnknownTrait {
        name: String,
        span: Span,
    },
    TraitBoundNotSatisfied {
        trait_name: String,
        actual: String,
        span: Span,
    },
    TraitEffectBoundNotSatisfied {
        trait_name: String,
        method: String,
        actual: String,
        effects: Vec<String>,
        span: Span,
    },
    InvalidTraitDefinition {
        name: String,
        span: Span,
    },
    DuplicateEffectDefinition {
        name: String,
        span: Span,
    },
    InvalidEffectDefinition {
        name: String,
        span: Span,
    },
    UnhandledEffect {
        effect: String,
        operation: String,
        span: Span,
    },
    UnknownEffectOperation {
        effect: String,
        operation: String,
        span: Span,
    },
    InvalidTraitImpl {
        trait_name: String,
        type_name: String,
        span: Span,
    },
    DuplicateTraitImpl {
        trait_name: String,
        type_name: String,
        span: Span,
    },
    GenericParametersNotSupported {
        target: String,
        span: Span,
    },
    /// Duplicate struct definition
    DuplicateStructDefinition {
        name: String,
        span: Span,
    },
    /// Duplicate class definition
    DuplicateClassDefinition {
        name: String,
        span: Span,
    },
    DuplicateEnumDefinition {
        name: String,
        span: Span,
    },
    EmptyEnumDefinition {
        name: String,
        span: Span,
    },
    RecursiveEnumDefinition {
        name: String,
        span: Span,
    },
    DuplicateEnumVariant {
        name: String,
        span: Span,
    },
    DuplicateEnumField {
        name: String,
        span: Span,
    },
    UnknownEnumVariant {
        enum_name: String,
        variant: String,
        span: Span,
    },
    DuplicateMatchArm {
        variant: String,
        span: Span,
    },
    DuplicatePatternBinding {
        name: String,
        span: Span,
    },
    UnreachableMatchArm {
        span: Span,
    },
    NonExhaustiveMatch {
        enum_name: String,
        span: Span,
    },
    /// Duplicate struct field
    DuplicateStructField {
        name: String,
        span: Span,
    },
    RecursiveStructDefinition {
        name: String,
        span: Span,
    },
    /// Invalid field in struct construction
    UnknownStructField {
        struct_name: String,
        field: String,
        span: Span,
    },
    /// Missing field in struct construction
    MissingStructField {
        struct_name: String,
        field: String,
        span: Span,
    },
    /// Duplicate class field
    DuplicateClassField {
        name: String,
        span: Span,
    },
    /// Invalid field in class construction
    UnknownClassField {
        class_name: String,
        field: String,
        span: Span,
    },
    /// Missing field in class construction
    MissingClassField {
        class_name: String,
        field: String,
        span: Span,
    },
    /// Invalid assignment target
    InvalidAssignmentTarget {
        span: Span,
    },
    ImmutableBinding {
        name: String,
        span: Span,
    },
    MutableTypeBinding {
        name: String,
        span: Span,
    },
    MutableCapture {
        name: String,
        boundary: String,
        span: Span,
    },
    UseAfterMove {
        name: String,
        span: Span,
    },
    InconsistentInitialization {
        name: String,
        span: Span,
    },
    /// Cannot write to an immutable class field
    ImmutableClassField {
        name: String,
        span: Span,
    },
    /// Function is not supported yet
    FunctionNotSupported {
        name: String,
        span: Span,
    },
    /// Wrong number of function arguments
    WrongArgumentCount {
        function: String,
        expected: usize,
        span: Span,
    },
    /// break is only valid inside a loop
    BreakOutsideLoop {
        span: Span,
    },
    /// continue is only valid inside a loop
    ContinueOutsideLoop {
        span: Span,
    },
    /// Expression is not callable
    NotCallable {
        span: Span,
    },
    /// if used in value position without an else branch
    IfMissingElse {
        then_type: String,
        span: Span,
    },
    /// Expression nesting exceeds the compiler-defined limit
    ExpressionTooDeep {
        limit: usize,
        span: Span,
    },
}

impl SemanticError {
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::MissingMainFunction => None,
            Self::UnknownValue { span, .. }
            | Self::UnknownFunction { span, .. }
            | Self::AmbiguousMethod { span, .. }
            | Self::UnknownType { span, .. }
            | Self::TypeMismatch { span, .. }
            | Self::UnusedValue { span, .. }
            | Self::CannotNegateUnsigned { span, .. }
            | Self::NumericOperatorRequiresNumeric { span }
            | Self::DivisionByZero { span }
            | Self::BitwiseOperatorRequiresInteger { span }
            | Self::AbsoluteValueOverflow { span }
            | Self::NumericConversionTarget { span }
            | Self::OrderedComparisonRequiresPartialOrd { span }
            | Self::ComparisonTypeMismatch { span }
            | Self::IntegerLiteralOutOfRange { span, .. }
            | Self::FloatLiteralOutOfRange { span }
            | Self::CannotNegateType { span, .. }
            | Self::DuplicateMainFunction { span }
            | Self::DuplicateFunctionDefinition { span, .. }
            | Self::DuplicateConstantDefinition { span, .. }
            | Self::InvalidConstantExpression { span }
            | Self::ConstantDependencyCycle { span, .. }
            | Self::DuplicateTypeParameter { span, .. }
            | Self::UnknownTrait { span, .. }
            | Self::TraitBoundNotSatisfied { span, .. }
            | Self::TraitEffectBoundNotSatisfied { span, .. }
            | Self::InvalidTraitDefinition { span, .. }
            | Self::DuplicateEffectDefinition { span, .. }
            | Self::InvalidEffectDefinition { span, .. }
            | Self::UnhandledEffect { span, .. }
            | Self::UnknownEffectOperation { span, .. }
            | Self::InvalidTraitImpl { span, .. }
            | Self::DuplicateTraitImpl { span, .. }
            | Self::GenericParametersNotSupported { span, .. }
            | Self::DuplicateStructDefinition { span, .. }
            | Self::DuplicateClassDefinition { span, .. }
            | Self::DuplicateEnumDefinition { span, .. }
            | Self::EmptyEnumDefinition { span, .. }
            | Self::RecursiveEnumDefinition { span, .. }
            | Self::DuplicateEnumVariant { span, .. }
            | Self::DuplicateEnumField { span, .. }
            | Self::UnknownEnumVariant { span, .. }
            | Self::DuplicateMatchArm { span, .. }
            | Self::DuplicatePatternBinding { span, .. }
            | Self::UnreachableMatchArm { span }
            | Self::NonExhaustiveMatch { span, .. }
            | Self::DuplicateStructField { span, .. }
            | Self::RecursiveStructDefinition { span, .. }
            | Self::UnknownStructField { span, .. }
            | Self::MissingStructField { span, .. }
            | Self::DuplicateClassField { span, .. }
            | Self::UnknownClassField { span, .. }
            | Self::MissingClassField { span, .. }
            | Self::InvalidAssignmentTarget { span }
            | Self::ImmutableBinding { span, .. }
            | Self::MutableTypeBinding { span, .. }
            | Self::MutableCapture { span, .. }
            | Self::UseAfterMove { span, .. }
            | Self::InconsistentInitialization { span, .. }
            | Self::ImmutableClassField { span, .. }
            | Self::FunctionNotSupported { span, .. }
            | Self::WrongArgumentCount { span, .. }
            | Self::BreakOutsideLoop { span }
            | Self::ContinueOutsideLoop { span }
            | Self::NotCallable { span }
            | Self::IfMissingElse { span, .. }
            | Self::ExpressionTooDeep { span, .. } => Some(*span),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::UnknownValue { name, .. } => format!("unknown value '{name}'"),
            Self::UnknownFunction { name, .. } => format!("unknown function '{name}'"),
            Self::AmbiguousMethod {
                name, candidates, ..
            } => format!(
                "ambiguous method '{name}' ({}); use Trait.{name}(value, ...) to select a trait",
                candidates.join(", ")
            ),

            Self::UnknownType { name, .. } => format!("unknown type '{name}'"),
            Self::TypeMismatch {
                expected, actual, ..
            } => format!("expected {expected}, found {actual}"),
            Self::UnusedValue { actual, .. } => {
                format!(
                    "this {actual} value is never used; assign it, discard it with 'let _', or delete the expression"
                )
            }
            Self::CannotNegateUnsigned { type_name, .. } => {
                format!("cannot negate unsigned type {type_name}")
            }
            Self::NumericOperatorRequiresNumeric { .. } => {
                "numeric operator requires numeric operands".to_owned()
            }
            Self::DivisionByZero { .. } => "division by zero".to_owned(),
            Self::BitwiseOperatorRequiresInteger { .. } => {
                "bitwise operator requires integer operands".to_owned()
            }
            Self::AbsoluteValueOverflow { .. } => {
                "absolute value of the minimum integer is not representable".to_owned()
            }
            Self::NumericConversionTarget { .. } => {
                "numeric conversion target must be a numeric type".to_owned()
            }
            Self::OrderedComparisonRequiresPartialOrd { .. } => {
                "ordered comparison requires operands of the same type implementing PartialOrd"
                    .to_owned()
            }
            Self::ComparisonTypeMismatch { .. } => {
                "comparison operands must have the same comparable type".to_owned()
            }
            Self::IntegerLiteralOutOfRange { type_name, .. } => {
                format!("integer literal is out of range for {type_name}")
            }
            Self::FloatLiteralOutOfRange { .. } => {
                "float literal is out of range for Float32".to_owned()
            }
            Self::CannotNegateType { type_name, .. } => {
                format!("cannot negate type {type_name}")
            }
            Self::MissingMainFunction => "program must define a main function".to_owned(),
            Self::DuplicateMainFunction { .. } => {
                "main function is defined more than once".to_owned()
            }
            Self::DuplicateFunctionDefinition { name, .. } => {
                format!("function '{name}' is already defined")
            }
            Self::DuplicateConstantDefinition { name, .. } => {
                format!("constant '{name}' is already defined")
            }
            Self::InvalidConstantExpression { .. } => {
                "constant value must be a pure constant expression".to_owned()
            }
            Self::ConstantDependencyCycle { name, .. } => {
                format!("constant '{name}' is part of a dependency cycle")
            }
            Self::DuplicateTypeParameter { name, .. } => {
                format!("type parameter '{name}' is defined more than once")
            }
            Self::UnknownTrait { name, .. } => format!("unknown trait '{name}'"),
            Self::TraitBoundNotSatisfied {
                trait_name, actual, ..
            } => format!("type '{actual}' does not implement trait '{trait_name}'"),
            Self::TraitEffectBoundNotSatisfied {
                trait_name,
                method,
                actual,
                effects,
                ..
            } => format!(
                "type '{actual}' does not satisfy trait '{trait_name}': method '{method}' declares effects not allowed by the trait: {}",
                effects.join(", ")
            ),
            Self::InvalidTraitDefinition { name, .. } => {
                format!("trait '{name}' does not match its builtin definition")
            }
            Self::DuplicateEffectDefinition { name, .. } => {
                format!("effect '{name}' is already defined")
            }
            Self::InvalidEffectDefinition { name, .. } => {
                format!("effect operation '{name}' is invalid")
            }
            Self::UnhandledEffect {
                effect, operation, ..
            } => format!("effect '{effect}.{operation}' is not declared by this function"),
            Self::UnknownEffectOperation {
                effect, operation, ..
            } => format!("unknown effect operation '{effect}.{operation}'"),
            Self::InvalidTraitImpl {
                trait_name,
                type_name,
                ..
            } => format!("invalid implementation of trait '{trait_name}' for '{type_name}'"),
            Self::DuplicateTraitImpl {
                trait_name,
                type_name,
                ..
            } => format!("trait '{trait_name}' is implemented more than once for '{type_name}'"),
            Self::GenericParametersNotSupported { target, .. } => {
                format!("generic parameters are not supported on {target}")
            }
            Self::DuplicateStructDefinition { name, .. } => {
                format!("struct '{name}' is already defined")
            }
            Self::DuplicateClassDefinition { name, .. } => {
                format!("class '{name}' is already defined")
            }
            Self::DuplicateEnumDefinition { name, .. } => {
                format!("enum '{name}' is already defined")
            }
            Self::EmptyEnumDefinition { name, .. } => {
                format!("enum '{name}' must define at least one variant")
            }
            Self::RecursiveEnumDefinition { name, .. } => {
                format!("enum '{name}' has an infinitely recursive value layout")
            }
            Self::DuplicateEnumVariant { name, .. } => {
                format!("enum variant '{name}' is defined more than once")
            }
            Self::DuplicateEnumField { name, .. } => {
                format!("enum variant field '{name}' is defined more than once")
            }
            Self::UnknownEnumVariant {
                enum_name, variant, ..
            } => {
                format!("enum '{enum_name}' has no variant '{variant}'")
            }
            Self::DuplicateMatchArm { variant, .. } => {
                format!("match arm for variant '{variant}' is defined more than once")
            }
            Self::DuplicatePatternBinding { name, .. } => {
                format!("pattern binding '{name}' is defined more than once")
            }
            Self::UnreachableMatchArm { .. } => "match arm is unreachable".to_owned(),
            Self::NonExhaustiveMatch { enum_name, .. } => {
                format!("match on '{enum_name}' is not exhaustive")
            }
            Self::DuplicateStructField { name, .. } => {
                format!("struct field '{name}' is defined more than once")
            }
            Self::RecursiveStructDefinition { name, .. } => {
                format!("struct '{name}' has an infinitely recursive value layout")
            }
            Self::UnknownStructField {
                struct_name, field, ..
            } => format!("struct '{struct_name}' has no field '{field}'"),
            Self::MissingStructField {
                struct_name, field, ..
            } => format!("missing field '{field}' in struct '{struct_name}'"),
            Self::DuplicateClassField { name, .. } => {
                format!("class field '{name}' is defined more than once")
            }
            Self::UnknownClassField {
                class_name, field, ..
            } => format!("class '{class_name}' has no field '{field}'"),
            Self::MissingClassField {
                class_name, field, ..
            } => format!("missing field '{field}' in class '{class_name}'"),
            Self::InvalidAssignmentTarget { .. } => {
                "assignment requires a local 'var' or 'self.var_field'".to_owned()
            }
            Self::ImmutableBinding { name, .. } => format!("cannot assign to immutable binding '{name}'; declare a local 'var' to reassign it"),
            Self::MutableTypeBinding { name, .. } => format!("mutable binding '{name}' cannot hold a compile-time type; use 'let'"),
            Self::MutableCapture { name, boundary, .. } if boundary == "closure" => format!("closure cannot capture outer mutable binding '{name}'; use 'move fn' to move it, create an explicit 'let' snapshot, or use Cown"),
            Self::MutableCapture { name, boundary, .. } => format!("{boundary} cannot capture outer mutable binding '{name}'; create an explicit 'let' snapshot or use Cown"),
            Self::UseAfterMove { name, .. } => format!("use of moved binding '{name}'; reinitialize it before use"),
            Self::InconsistentInitialization { name, .. } => format!("binding '{name}' has inconsistent initialization across control-flow paths; reinitialize it or move it on every incoming path"),
            Self::ImmutableClassField { name, .. } => {
                format!("class field '{name}' is immutable")
            }
            Self::FunctionNotSupported { name, .. } => {
                format!("function '{name}' is not supported yet")
            }
            Self::WrongArgumentCount {
                function, expected, ..
            } => {
                format!("{function} expects exactly {expected} argument(s)")
            }
            Self::BreakOutsideLoop { .. } => "break is only valid inside a loop".to_owned(),
            Self::ContinueOutsideLoop { .. } => "continue is only valid inside a loop".to_owned(),
            Self::NotCallable { .. } => "expression is not callable".to_owned(),
            Self::IfMissingElse { then_type, .. } => format!(
                "else can be omitted only when the then-branch is Unit or never returns, but this then-branch is {then_type}"
            ),
            Self::ExpressionTooDeep { limit, .. } => {
                format!("expression nesting exceeds {limit}")
            }
        }
    }
}

/// Code generation error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenError {
    /// Cranelift initialization failed
    BackendInitialization { message: String },
    /// Runtime error
    RuntimeError { message: String },
}

impl CodegenError {
    pub fn message(&self) -> String {
        match self {
            Self::BackendInitialization { message } | Self::RuntimeError { message } => {
                message.clone()
            }
        }
    }
}
