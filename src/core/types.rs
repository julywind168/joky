//! Named type registration shared by every compilation backend.
//!
//! Primitive types are part of the language. Named types are opaque host values
//! and must be registered before source that mentions them is compiled.

use crate::{parser::ast::Type, Error, ErrorCode, ErrorPhase};
use std::collections::BTreeSet;

const VM_HANDLE: &str = "vm.Handle";
const VM_FUNCTION: &str = "vm.Function";

#[derive(Clone, Debug)]
pub struct TypeRegistry {
    named: BTreeSet<String>,
}

impl TypeRegistry {
    /// Create a registry containing the built-in VM opaque types.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_opaque(&mut self, name: impl Into<String>) -> Result<(), Error> {
        let name = name.into();
        if !is_type_name(&name) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Invalid opaque type name `{name}`"),
            ));
        }
        if !self.named.insert(name.clone()) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                format!("Opaque type `{name}` is already registered"),
            ));
        }
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.named.contains(name)
    }

    pub(crate) fn validate(&self, ty: &Type) -> Result<(), Error> {
        let Type::Named(name) = ty else { return Ok(()) };
        if self.contains(name) {
            Ok(())
        } else {
            Err(Error::new_with_code(
                ErrorPhase::TypeCheck,
                ErrorCode::UnknownType,
                format!("Unknown opaque type `{name}`; register it before compiling"),
            ))
        }
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self {
            named: BTreeSet::from([VM_HANDLE.into(), VM_FUNCTION.into()]),
        }
    }
}

fn is_type_name(name: &str) -> bool {
    let mut segments = name.split('.');
    let valid = |segment: &str| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    };
    !name.is_empty() && segments.all(valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_and_custom_opaque_types_are_registered_explicitly() {
        let mut types = TypeRegistry::new();
        assert!(types.contains(VM_HANDLE));
        assert!(types.contains(VM_FUNCTION));
        assert!(!types.contains("Host.Context"));
        types.register_opaque("Host.Context").unwrap();
        assert!(types.contains("Host.Context"));
        let duplicate = types.register_opaque("Host.Context").unwrap_err();
        assert_eq!(duplicate.phase(), ErrorPhase::Abi);
        assert_eq!(duplicate.code(), ErrorCode::AbiError);
        let invalid = types.register_opaque("not valid").unwrap_err();
        assert_eq!(invalid.phase(), ErrorPhase::Abi);
        assert_eq!(invalid.code(), ErrorCode::AbiError);
    }
}
