use std::collections::HashSet;

use super::{checker::Checker, Type, TypeTable};

fn supports(
    ty: Type,
    visiting: &mut HashSet<Type>,
    explicit: &impl Fn(Type) -> bool,
    members: &impl Fn(Type) -> Option<Vec<Type>>,
) -> bool {
    if ty.is_numeric()
        || ty.is_native_resource()
        || matches!(
            ty,
            Type::String | Type::Bool | Type::Unit | Type::Bytes | Type::Duration
        )
        || explicit(ty)
    {
        return true;
    }
    let Some(fields) = members(ty) else {
        return false;
    };
    // Recursive nominal types are valid if every nonrecursive member has Debug.
    if !visiting.insert(ty) {
        return true;
    }
    let result = fields
        .into_iter()
        .all(|field| supports(field, visiting, explicit, members));
    visiting.remove(&ty);
    result
}

impl Checker {
    pub(super) fn supports_debug(&self, ty: Type) -> bool {
        supports(
            ty,
            &mut HashSet::new(),
            &|ty| {
                self.trait_impls
                    .get("Debug")
                    .is_some_and(|types| types.contains(&ty))
                    || matches!(ty, Type::Dyn(id) if self.dynamic_types[id].includes("Debug"))
                    || matches!(ty, Type::Param(id) if self.current_type_parameter_bounds.get(id).is_some_and(|bounds| bounds.iter().any(|name| name == "Debug")))
            },
            &|ty| {
                Some(match ty {
                    Type::Tuple(id) => self.tuple_types[id].clone(),
                    Type::List(id) => vec![self.list_types[id]],
                    Type::Option(id) => vec![self.option_types[id]],
                    Type::Result(id) => {
                        let (ok, err) = self.result_types[id];
                        vec![ok, err]
                    }
                    Type::Struct(id) => self
                        .structs
                        .values()
                        .find(|s| s.id == id)?
                        .fields
                        .iter()
                        .map(|(_, ty)| *ty)
                        .collect(),
                    Type::Class(id) => self
                        .classes
                        .values()
                        .find(|s| s.id == id)?
                        .fields
                        .iter()
                        .map(|(_, field, _)| field.ty)
                        .collect(),
                    Type::Enum(id) => self
                        .enums
                        .values()
                        .find(|s| s.id == id)?
                        .variants
                        .iter()
                        .flat_map(|v| v.fields.iter().map(|(_, ty)| *ty))
                        .collect(),
                    _ => return None,
                })
            },
        )
    }
}

impl TypeTable {
    pub(crate) fn has_explicit_debug(&self, ty: Type) -> bool {
        self.trait_implementations
            .get("Debug")
            .is_some_and(|types| types.contains(&ty))
    }

    pub(crate) fn debug_members(&self, ty: Type) -> Option<Vec<Type>> {
        Some(match ty {
            Type::Tuple(id) => self.tuple_elements(id).to_vec(),
            Type::List(id) => vec![self.list_type(id)],
            Type::Option(id) => vec![self.option_type(id)],
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                vec![ok, err]
            }
            Type::Struct(id) => self.struct_fields(id).iter().map(|(_, ty)| *ty).collect(),
            Type::Class(id) => self.class_fields(id).iter().map(|(_, ty)| *ty).collect(),
            Type::Enum(id) => self
                .enum_variants(id)
                .iter()
                .flat_map(|v| v.fields.iter().map(|(_, ty)| *ty))
                .collect(),
            _ => return None,
        })
    }

    pub(crate) fn supports_debug(&self, ty: Type) -> bool {
        supports(
            ty,
            &mut HashSet::new(),
            &|ty| {
                self.has_explicit_debug(ty)
                    || matches!(ty, Type::Dyn(id) if self.dynamic_types[id].includes("Debug"))
            },
            &|ty| self.debug_members(ty),
        )
    }

    pub(crate) fn has_default_debug(&self, ty: Type) -> bool {
        matches!(ty, Type::Struct(_) | Type::Class(_) | Type::Enum(_))
            && !self.has_explicit_debug(ty)
            && self.supports_debug(ty)
    }
}
