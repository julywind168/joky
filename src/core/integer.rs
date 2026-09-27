//! Integer width, signedness and literal bounds shared by semantic checking and lowering.
use crate::Type;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntegerType {
    Int32,
    Int64,
    UInt32,
    UInt64,
}

impl IntegerType {
    pub(crate) fn bits(self) -> u32 {
        match self {
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
            Self::UInt32 => Type::UInt32,
            Self::UInt64 => Type::UInt64,
        }
    }
}

impl Type {
    pub(crate) fn integer(&self) -> Option<IntegerType> {
        Some(match self {
            Self::Int32 => IntegerType::Int32,
            Self::Int64 => IntegerType::Int64,
            Self::UInt32 => IntegerType::UInt32,
            Self::UInt64 => IntegerType::UInt64,
            _ => return None,
        })
    }

    pub(crate) fn is_copy_scalar(&self) -> bool {
        self.integer().is_some() || matches!(self, Self::Bool | Self::Unit)
    }
}
