//! Integer metadata, literal bounds and conversion names shared across the compiler.
use crate::parser::ast::BinOp;
use crate::{Error, ErrorCode, ErrorPhase, Span, Type};

/// Trap semantics for integer division and shifts. The interpreter runtime
/// and the constant folder share one wrapping kernel (the `apply_*` functions
/// below); native lowering emits the same rules in Cranelift and raises them
/// through `runtime::error`. The message strings are observable in structured
/// errors, so they live here as the single source of truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegerTrap {
    DivisionByZero,
    ShiftOutOfRange,
}

impl IntegerTrap {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::DivisionByZero => "Division by zero",
            Self::ShiftOutOfRange => {
                "Shift count must be nonnegative and less than the integer width"
            }
        }
    }
}

/// Result of an integer operation: comparisons yield the shared `Bool` type,
/// everything else stays in the operand width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegerOutcome<T> {
    Value(T),
    Bool(bool),
}

/// The six comparison operators, shared by the wrapping kernel and the
/// interpreter's compare-branch instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Comparison {
    pub(crate) fn from_binary(op: &BinOp) -> Option<Self> {
        Some(match op {
            BinOp::Eq => Self::Eq,
            BinOp::Ne => Self::Ne,
            BinOp::Lt => Self::Lt,
            BinOp::Le => Self::Le,
            BinOp::Gt => Self::Gt,
            BinOp::Ge => Self::Ge,
            _ => return None,
        })
    }

    #[inline]
    pub(crate) fn test<T: PartialOrd>(self, lhs: T, rhs: T) -> bool {
        match self {
            Self::Eq => lhs == rhs,
            Self::Ne => lhs != rhs,
            Self::Lt => lhs < rhs,
            Self::Le => lhs <= rhs,
            Self::Gt => lhs > rhs,
            Self::Ge => lhs >= rhs,
        }
    }
}

macro_rules! integer_kernel {
    ($name:ident, $ty:ty) => {
        /// Wrapping semantics for one concrete width. Division by zero and
        /// out-of-range shifts trap; `rhs as u64` invalidates negative signed
        /// counts through sign extension and cannot wrap for `UInt64`.
        pub(crate) fn $name(
            op: &BinOp,
            lhs: $ty,
            rhs: $ty,
        ) -> Result<IntegerOutcome<$ty>, IntegerTrap> {
            Ok(match op {
                BinOp::Add => IntegerOutcome::Value(lhs.wrapping_add(rhs)),
                BinOp::Sub => IntegerOutcome::Value(lhs.wrapping_sub(rhs)),
                BinOp::Mul => IntegerOutcome::Value(lhs.wrapping_mul(rhs)),
                BinOp::Div if rhs != 0 => IntegerOutcome::Value(lhs.wrapping_div(rhs)),
                BinOp::Div => return Err(IntegerTrap::DivisionByZero),
                BinOp::BitAnd => IntegerOutcome::Value(lhs & rhs),
                BinOp::BitOr => IntegerOutcome::Value(lhs | rhs),
                BinOp::BitXor => IntegerOutcome::Value(lhs ^ rhs),
                BinOp::Shl | BinOp::Shr => {
                    if rhs as u64 >= u64::from(<$ty>::BITS) {
                        return Err(IntegerTrap::ShiftOutOfRange);
                    }
                    IntegerOutcome::Value(if matches!(op, BinOp::Shl) {
                        lhs.wrapping_shl(rhs as u32)
                    } else {
                        lhs.wrapping_shr(rhs as u32)
                    })
                }
                // The remaining operators are exactly the comparisons.
                op => IntegerOutcome::Bool(
                    Comparison::from_binary(op)
                        .expect("non-comparison operator reached the integer kernel")
                        .test(lhs, rhs),
                ),
            })
        }
    };
}
integer_kernel!(apply_i32, i32);
integer_kernel!(apply_i64, i64);
integer_kernel!(apply_u8, u8);
integer_kernel!(apply_u32, u32);
integer_kernel!(apply_u64, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegerType {
    Int32,
    Int64,
    UInt8,
    UInt32,
    UInt64,
    Rune,
}

impl IntegerType {
    /// Global intrinsic names; ordinary function declarations cannot replace them.
    pub(crate) fn conversion(name: &str) -> Option<Self> {
        Some(match name {
            "int32" => Self::Int32,
            "int64" => Self::Int64,
            "uint8" => Self::UInt8,
            "uint32" => Self::UInt32,
            "uint64" => Self::UInt64,
            _ => return None,
        })
    }

    pub(crate) fn check_declaration(name: &str, span: Span) -> Result<(), Error> {
        if matches!(name, "Ptr.null" | "Ptr.is_null") {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::ReservedName,
                format!("Reserved pointer intrinsic name {name}"),
            )
            .with_span(span));
        }
        if matches!(name, "alloc" | "share" | "memory.read" | "memory.write") {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::ReservedName,
                format!("Reserved memory intrinsic name {name}"),
            )
            .with_span(span));
        }
        if Self::conversion(name).is_some() {
            return Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::ReservedName,
                format!("Reserved integer conversion function name `{name}`"),
            )
            .with_span(span));
        }
        Ok(())
    }

    pub(crate) fn bits(self) -> u32 {
        match self {
            Self::UInt8 => 8,
            Self::Rune => 32,
            Self::Int32 | Self::UInt32 => 32,
            Self::Int64 | Self::UInt64 => 64,
        }
    }

    pub(crate) fn signed(self) -> bool {
        matches!(self, Self::Int32 | Self::Int64)
    }

    pub(crate) fn accepts_literal(self, magnitude: u64, negative: bool) -> bool {
        if negative && !self.signed() {
            return false;
        }
        let max = u64::MAX >> (64 - self.bits() + u32::from(self.signed()));
        magnitude <= max + u64::from(negative)
    }

    pub(crate) fn ty(self) -> Type {
        match self {
            Self::Int32 => Type::Int32,
            Self::Int64 => Type::Int64,
            Self::UInt8 => Type::UInt8,
            Self::UInt32 => Type::UInt32,
            Self::UInt64 => Type::UInt64,
            Self::Rune => Type::Rune,
        }
    }
}

impl Type {
    pub(crate) fn integer(&self) -> Option<IntegerType> {
        Some(match self {
            Self::Int32 => IntegerType::Int32,
            Self::Int64 => IntegerType::Int64,
            Self::UInt8 => IntegerType::UInt8,
            Self::UInt32 => IntegerType::UInt32,
            Self::UInt64 => IntegerType::UInt64,
            _ => return None,
        })
    }

    pub(crate) fn is_copy_scalar(&self) -> bool {
        self.integer().is_some()
            || matches!(self, Type::Rune)
            || matches!(
                self,
                Self::Bool | Self::Unit | Self::Ptr | Self::Parameter(_) | Self::Function(_)
            )
    }
}
