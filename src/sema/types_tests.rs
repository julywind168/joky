//! Type system tests

#[cfg(test)]
mod tests {
    use crate::sema::types::{primitive_type, type_name, Type};

    #[test]
    fn type_is_integer_identifies_all_integer_types() {
        assert!(Type::I8.is_integer());
        assert!(Type::I16.is_integer());
        assert!(Type::I32.is_integer());
        assert!(Type::I64.is_integer());
        assert!(Type::U8.is_integer());
        assert!(Type::U16.is_integer());
        assert!(Type::U32.is_integer());
        assert!(Type::U64.is_integer());
        assert!(!Type::F32.is_integer());
        assert!(!Type::F64.is_integer());
        assert!(!Type::String.is_integer());
        assert!(!Type::Bool.is_integer());
        assert!(!Type::Unit.is_integer());
    }

    #[test]
    fn type_is_signed_integer_identifies_signed_types() {
        assert!(Type::I8.is_signed_integer());
        assert!(Type::I16.is_signed_integer());
        assert!(Type::I32.is_signed_integer());
        assert!(Type::I64.is_signed_integer());
        assert!(!Type::U8.is_signed_integer());
        assert!(!Type::U16.is_signed_integer());
        assert!(!Type::U32.is_signed_integer());
        assert!(!Type::U64.is_signed_integer());
    }

    #[test]
    fn type_is_float_identifies_float_types() {
        assert!(Type::F32.is_float());
        assert!(Type::F64.is_float());
        assert!(!Type::I32.is_float());
        assert!(!Type::String.is_float());
    }

    #[test]
    fn type_is_numeric_includes_all_numbers() {
        assert!(Type::I8.is_numeric());
        assert!(Type::I16.is_numeric());
        assert!(Type::I32.is_numeric());
        assert!(Type::I64.is_numeric());
        assert!(Type::U8.is_numeric());
        assert!(Type::U16.is_numeric());
        assert!(Type::U32.is_numeric());
        assert!(Type::U64.is_numeric());
        assert!(Type::F32.is_numeric());
        assert!(Type::F64.is_numeric());
        assert!(!Type::String.is_numeric());
        assert!(!Type::Bool.is_numeric());
        assert!(!Type::Unit.is_numeric());
        assert!(!Type::Duration.is_numeric());
    }

    #[test]
    fn type_name_returns_correct_names() {
        assert_eq!(type_name(Type::I8), "Int8");
        assert_eq!(type_name(Type::I16), "Int16");
        assert_eq!(type_name(Type::I32), "Int32");
        assert_eq!(type_name(Type::I64), "Int64");
        assert_eq!(type_name(Type::U8), "UInt8");
        assert_eq!(type_name(Type::U16), "UInt16");
        assert_eq!(type_name(Type::U32), "UInt32");
        assert_eq!(type_name(Type::U64), "UInt64");
        assert_eq!(type_name(Type::F32), "Float32");
        assert_eq!(type_name(Type::F64), "Float64");
        assert_eq!(type_name(Type::String), "String");
        assert_eq!(type_name(Type::Bool), "Bool");
        assert_eq!(type_name(Type::Unit), "Unit");
        assert_eq!(type_name(Type::Duration), "Duration");
        for (index, name) in Type::NATIVE_TYPES.iter().enumerate() {
            assert_eq!(type_name(Type::Native(index)), *name);
        }
    }

    #[test]
    fn types_are_equal() {
        assert_eq!(Type::I32, Type::I32);
        assert_ne!(Type::I32, Type::I64);
        assert_ne!(Type::F32, Type::F64);
    }

    #[test]
    fn native_handle_names_resolve_to_distinct_types() {
        for (index, name) in Type::NATIVE_TYPES.iter().enumerate() {
            assert_eq!(primitive_type(name), Some(Type::Native(index)));
            assert!(Type::Native(index).is_native_resource());
        }
        assert_eq!(primitive_type("NotAHandle"), None);
    }
}
