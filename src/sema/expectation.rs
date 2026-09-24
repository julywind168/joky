//! Type inference expectation

use super::types::Type;

/// Type inference expectation
///
/// Used to express type checking expectations more explicitly
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeExpectation {
    /// No expectation - infer the type freely
    ///
    /// Used for:
    /// - Inferring variable types
    /// - Operands of `echo` used as a statement
    None,

    /// Hint type - try to infer as this type, but other compatible types are allowed
    ///
    /// Used for:
    /// - Contextual inference of numeric literals (e.g. `let x: Int64 = 42`)
    /// - Type hints for function arguments
    ///
    /// If the expression can infer as this type, this type is used;
    /// otherwise the expression's default type applies
    Hint(Type),

    /// Required type - must be this type, otherwise an error
    ///
    /// Used for:
    /// - Variable assignment (type already determined)
    /// - if branch types that must match
    /// - Function argument types that must match
    Require(Type),
}

impl TypeExpectation {
    /// Creates a None expectation
    pub(super) const fn none() -> Self {
        Self::None
    }

    /// Creates a Hint expectation
    pub(super) const fn hint(ty: Type) -> Self {
        Self::Hint(ty)
    }

    /// Creates a Require expectation
    pub(super) const fn require(ty: Type) -> Self {
        Self::Require(ty)
    }

    /// Creates an expectation from Option<Type> (legacy compatibility)
    ///
    /// Some(ty) -> Require(ty)
    /// None -> None
    pub(super) const fn from_option(expected: Option<Type>) -> Self {
        match expected {
            Some(ty) => Self::Require(ty),
            None => Self::None,
        }
    }

    /// Returns the expected type, if any
    pub(super) const fn ty(self) -> Option<Type> {
        match self {
            Self::None => None,
            Self::Hint(ty) | Self::Require(ty) => Some(ty),
        }
    }

    /// Returns true if this is a Require
    pub(super) const fn is_required(self) -> bool {
        matches!(self, Self::Require(_))
    }
}

impl From<Option<Type>> for TypeExpectation {
    fn from(opt: Option<Type>) -> Self {
        Self::from_option(opt)
    }
}
