use crate::diagnostic::SemanticError;
use crate::Span;

use super::super::checker::Checker;
use super::super::types::Type;

impl Checker {
    pub(in crate::sema) fn check_list_types(
        &mut self,
        ty: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        self.check_list_types_inner(ty, span, &mut Vec::new())
    }

    fn check_list_types_inner(
        &mut self,
        ty: Type,
        span: Span,
        visiting: &mut Vec<Type>,
    ) -> Result<(), SemanticError> {
        if visiting.contains(&ty) {
            return Ok(());
        }
        visiting.push(ty);
        let result = match ty {
            Type::List(id) => self.check_list_element(self.list_types[id], span),
            Type::MutList(id) => self.check_mut_list_element(self.list_types[id], span),
            Type::Map(id) => {
                let info = self.maps[id];
                self.check_map_key(info.key, span)
                    .and_then(|()| self.check_list_element(info.value, span))
            }
            Type::MutMap(id) => {
                let info = self.maps[id];
                self.check_map_key(info.key, span)
                    .and_then(|()| self.check_mut_list_element(info.value, span))
            }
            Type::MutSet(id) => self.check_map_key(self.maps[id].key, span),
            Type::Cown(id) => self.check_list_types_inner(self.cowns[id], span, visiting),
            Type::Tuple(id) => self.tuple_types[id]
                .clone()
                .into_iter()
                .try_for_each(|element| self.check_list_types_inner(element, span, visiting)),
            Type::Struct(id) => self
                .structs
                .values()
                .find(|structure| structure.id == id)
                .map(|structure| {
                    structure
                        .fields
                        .iter()
                        .map(|(_, field)| *field)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .try_for_each(|field| self.check_list_types_inner(field, span, visiting)),
            Type::Enum(id) => self
                .enums
                .values()
                .find(|enumeration| enumeration.id == id)
                .map(|enumeration| {
                    enumeration
                        .variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter().map(|(_, field)| *field))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .try_for_each(|field| self.check_list_types_inner(field, span, visiting)),
            Type::Option(id) => self.check_list_types_inner(self.option_types[id], span, visiting),
            Type::Result(id) => {
                let (ok, err) = self.result_types[id];
                self.check_list_types_inner(ok, span, visiting)
                    .and_then(|()| self.check_list_types_inner(err, span, visiting))
            }
            Type::Class(_)
            | Type::CPtr(_)
            | Type::CMutPtr(_)
            | Type::CArray(_)
            | Type::CStr
            | Type::Native(_)
            | Type::CCallback
            | Type::Unit
            | Type::Duration
            | Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::F32
            | Type::F64
            | Type::String
            | Type::Bytes
            | Type::MutBytes
            | Type::Bool
            | Type::Param(_)
            | Type::SelfType
            | Type::Associated(_) => Ok(()),
            Type::Batch | Type::SeqBuilder | Type::Hasher | Type::Function(_) | Type::Dyn(_) => {
                Ok(())
            }
            Type::BytesCursor => Ok(()),
            Type::MapCursor(_) | Type::MapKeyCursor(_) | Type::MapValueCursor(_) => Ok(()),
            Type::MutListCursor(_) | Type::MutMapCursor(_) | Type::MutSetCursor(_) => Ok(()),
        };
        visiting.pop();
        result
    }

    pub(in crate::sema) fn check_list_element(
        &mut self,
        element: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        if !self.is_supported_list_element(element, &mut Vec::new()) {
            return Err(SemanticError::TypeMismatch {
                expected: "an immutable value without class references".to_owned(),
                actual: super::super::type_name(element),
                span,
            });
        }
        Ok(())
    }

    pub(in crate::sema) fn check_mut_list_element(
        &self,
        element: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        if !self.is_supported_list_element(element, &mut Vec::new()) {
            return Err(SemanticError::TypeMismatch {
                expected: "a concrete value with a runtime representation".to_owned(),
                actual: super::super::type_name(element),
                span,
            });
        }
        Ok(())
    }

    pub(in crate::sema) fn check_map_key(
        &mut self,
        key: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        if let Type::Tuple(id) = key {
            for member in self.tuple_types[id].clone() {
                self.check_map_key(member, span)?;
            }
            return Ok(());
        }
        if let Type::Param(id) = key {
            let bounds = self
                .current_type_parameter_bounds
                .get_mut(id)
                .ok_or_else(|| SemanticError::TypeMismatch {
                    expected: "a map key type".to_owned(),
                    actual: super::super::type_name(key),
                    span,
                })?;
            for trait_name in ["Hash", "Eq", "__MapKey"] {
                if !bounds.iter().any(|bound| bound == trait_name) {
                    bounds.push(trait_name.to_owned());
                }
            }
            return Ok(());
        }
        for trait_name in ["Hash", "Eq"] {
            if !self.implements_trait(key, trait_name) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: trait_name.to_owned(),
                    actual: super::super::type_name(key),
                    span,
                });
            }
        }
        if !self.is_supported_map_key(key, &mut Vec::new()) {
            return Err(SemanticError::TypeMismatch {
                expected: "an immutable key value without class references".to_owned(),
                actual: super::super::type_name(key),
                span,
            });
        }
        Ok(())
    }

    pub(in crate::sema) fn is_supported_map_key(
        &self,
        key: Type,
        visiting: &mut Vec<Type>,
    ) -> bool {
        self.check_key_layout(key, visiting, false)
    }

    pub(in crate::sema) fn is_structural_map_key(
        &self,
        key: Type,
        visiting: &mut Vec<Type>,
    ) -> bool {
        self.check_key_layout(key, visiting, true)
    }

    fn check_key_layout(&self, key: Type, visiting: &mut Vec<Type>, structural: bool) -> bool {
        if is_builtin_map_key(key) {
            return true;
        }
        if structural
            && self
                .trait_impls
                .get("PartialEq")
                .is_some_and(|types| types.contains(&key))
        {
            return false;
        }
        if visiting.contains(&key) {
            return true;
        }
        visiting.push(key);
        let supported = match key {
            Type::Tuple(id) => self.tuple_types[id]
                .iter()
                .all(|member| self.check_key_layout(*member, visiting, structural)),
            Type::Param(id) => {
                !structural
                    && self
                        .current_type_parameter_bounds
                        .get(id)
                        .is_some_and(|bounds| bounds.iter().any(|bound| bound == "__MapKey"))
            }
            Type::Enum(id) => self
                .enums
                .values()
                .find(|e| e.id == id)
                .is_some_and(|e| e.variants.iter().all(|v| v.fields.is_empty())),
            Type::Struct(id) => {
                self.structs
                    .values()
                    .find(|structure| structure.id == id)
                    .is_some_and(|structure| {
                        !structure.fields.is_empty()
                            && structure.fields.iter().all(|(_, field)| {
                                self.check_key_layout(*field, visiting, structural)
                            })
                    })
            }
            _ => false,
        };
        visiting.pop();
        supported
    }

    fn is_supported_list_element(&self, element: Type, visiting: &mut Vec<Type>) -> bool {
        if element.is_numeric()
            || matches!(
                element,
                Type::String
                    | Type::Bytes
                    | Type::Bool
                    | Type::Unit
                    | Type::Param(_)
                    | Type::Associated(_)
            )
        {
            return true;
        }
        if visiting.contains(&element) {
            return true;
        }
        visiting.push(element);
        let supported = match element {
            Type::CPtr(_) | Type::CMutPtr(_) | Type::CArray(_) | Type::CStr => false,
            Type::Tuple(id) => self.tuple_types[id]
                .iter()
                .all(|element| self.is_supported_list_element(*element, visiting)),
            Type::Struct(id) => self
                .structs
                .values()
                .find(|structure| structure.id == id)
                .is_some_and(|structure| {
                    structure
                        .fields
                        .iter()
                        .all(|(_, field)| self.is_supported_list_element(*field, visiting))
                }),
            Type::Enum(id) => self
                .enums
                .values()
                .find(|enumeration| enumeration.id == id)
                .is_some_and(|enumeration| {
                    enumeration.variants.iter().all(|variant| {
                        variant
                            .fields
                            .iter()
                            .all(|(_, field)| self.is_supported_list_element(*field, visiting))
                    })
                }),
            Type::Option(id) => self.is_supported_list_element(self.option_types[id], visiting),
            Type::Result(id) => {
                let (ok, err) = self.result_types[id];
                self.is_supported_list_element(ok, visiting)
                    && self.is_supported_list_element(err, visiting)
            }
            Type::List(id) => self.is_supported_list_element(self.list_types[id], visiting),
            Type::MutBytes | Type::MutList(_) | Type::MutMap(_) | Type::MutSet(_) => false,
            Type::Map(id) => {
                let info = self.maps[id];
                self.implements_trait(info.key, "Hash")
                    && self.implements_trait(info.key, "Eq")
                    && self.is_supported_list_element(info.value, visiting)
            }
            Type::Class(_) | Type::Native(_) | Type::CCallback | Type::Unit => false,
            Type::Duration | Type::Cown(_) => true,
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::F32
            | Type::F64
            | Type::String
            | Type::Bytes
            | Type::Bool
            | Type::Param(_)
            | Type::SelfType
            | Type::Associated(_) => unreachable!("handled above"),
            Type::Batch | Type::SeqBuilder | Type::Hasher | Type::Function(_) | Type::Dyn(_) => {
                false
            }
            Type::BytesCursor => false,
            Type::MapCursor(_) | Type::MapKeyCursor(_) | Type::MapValueCursor(_) => false,
            Type::MutListCursor(_) | Type::MutMapCursor(_) | Type::MutSetCursor(_) => false,
        };
        visiting.pop();
        supported
    }
}

pub(super) fn is_builtin_map_key(ty: Type) -> bool {
    matches!(
        ty,
        Type::String | Type::Bool | Type::Bytes | Type::Duration | Type::Unit
    ) || ty.is_integer()
}
