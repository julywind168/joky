//! Type conversion utilities
//!
//! Provides conversions between Joky types and Cranelift types.

use cranelift_codegen::ir::types;

use crate::diagnostic::CodegenError;
use crate::sema::Type;

/// Convert a Joky type to a Cranelift type
///
/// Only numeric types can be converted. String, Bool, Unit, and composite types
/// (Tuple, Struct, Class, Enum) are handled specially in codegen and don't have
/// direct Cranelift equivalents.
///
/// # Errors
///
/// Returns an error if the type cannot be converted to a Cranelift type
/// (e.g., String, Bool, Unit, Tuple, Struct, Class, Enum).
pub(super) fn cranelift_type(ty: Type) -> Result<cranelift_codegen::ir::Type, CodegenError> {
    match ty {
        Type::I8 | Type::U8 => Ok(types::I8),
        Type::I16 | Type::U16 => Ok(types::I16),
        Type::I32 | Type::U32 => Ok(types::I32),
        Type::I64 | Type::U64 | Type::Duration => Ok(types::I64),
        Type::F32 => Ok(types::F32),
        Type::F64 => Ok(types::F64),
        Type::String
        | Type::CPtr(_)
        | Type::CMutPtr(_)
        | Type::CArray(_)
        | Type::CStr
        | Type::Batch
        | Type::SeqBuilder
        | Type::Hasher
        | Type::Bytes
        | Type::BytesCursor
        | Type::MapCursor(_)
        | Type::MapKeyCursor(_)
        | Type::MapValueCursor(_)
        | Type::MutListCursor(_)
        | Type::MutMapCursor(_)
        | Type::MutSetCursor(_)
        | Type::MutBytes
        | Type::Native(_)
        | Type::CCallback
        | Type::Bool
        | Type::Unit
        | Type::Tuple(_)
        | Type::Struct(_)
        | Type::Class(_)
        | Type::Cown(_)
        | Type::Enum(_)
        | Type::Option(_)
        | Type::Result(_)
        | Type::List(_)
        | Type::MutList(_)
        | Type::Map(_)
        | Type::MutMap(_)
        | Type::MutSet(_)
        | Type::Param(_)
        | Type::SelfType
        | Type::Associated(_)
        | Type::Function(_)
        | Type::Dyn(_) => Err(CodegenError::RuntimeError {
            message: format!(
                "non-numeric type {:?} cannot be converted to Cranelift type",
                ty
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cranelift_type_handles_all_numeric_types() {
        assert!(cranelift_type(Type::I8).is_ok());
        assert!(cranelift_type(Type::I16).is_ok());
        assert!(cranelift_type(Type::I32).is_ok());
        assert!(cranelift_type(Type::I64).is_ok());
        assert!(cranelift_type(Type::U8).is_ok());
        assert!(cranelift_type(Type::U16).is_ok());
        assert!(cranelift_type(Type::U32).is_ok());
        assert!(cranelift_type(Type::U64).is_ok());
        assert!(cranelift_type(Type::F32).is_ok());
        assert!(cranelift_type(Type::F64).is_ok());
    }

    #[test]
    fn cranelift_type_rejects_non_numeric() {
        for ty in [
            Type::String,
            Type::Bool,
            Type::Unit,
            Type::Tuple(0),
            Type::Struct(0),
            Type::Class(0),
            Type::Enum(0),
            Type::Option(0),
            Type::Result(0),
        ] {
            assert!(cranelift_type(ty).is_err());
        }
    }
}
