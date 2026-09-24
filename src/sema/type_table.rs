//! Backend type identities and layouts. Source facts and module APIs are separate.

use super::EffectSet;
use std::collections::{HashMap, HashSet};

use crate::module::SymbolId;

use super::symbol_table::AssociatedTypeInfo;
use super::types::{MapInfo, Type};

/// Resolved import ABI in this module's type/effect index space.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct ExternalFunction {
    pub(crate) parameters: Vec<(String, Type)>,
    pub(crate) parameter_borrows: Vec<bool>,
    pub(crate) return_type: Type,
    pub(crate) suspends: bool,
    pub(crate) region_contract: Option<crate::mir::regions::Summary>,
}

/// Persistent type information. Source-node facts live in `CheckedTypes`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub(crate) struct TypeTable {
    pub(crate) dynamic_types: Vec<super::dynamic::DynamicType>,
    tuples: Vec<Vec<Type>>,
    c_pointers: Vec<Type>,
    c_arrays: Vec<(Type, u64)>,
    structs: Vec<StructLayout>,
    classes: Vec<ClassLayout>,
    enums: Vec<EnumLayout>,
    options: Vec<Type>,
    results: Vec<(Type, Type)>,
    lists: Vec<Type>,
    maps: Vec<MapInfo>,
    cowns: Vec<Type>,
    function_types: Vec<super::symbol_table::FunctionTypeInfo>,
    show_impls: HashSet<Type>,
    associated_types: Vec<AssociatedTypeInfo>,
    trait_associated_impls: HashMap<(String, Type), HashMap<String, Type>>,
    effects: super::effects::EffectRegistry,
    pub(crate) trait_implementations: HashMap<String, HashSet<Type>>,
    pub(super) explicit_hash_impls: HashSet<Type>,
}

mod interface;
pub(crate) use interface::{ModuleInterface, ModuleTypes};

mod checked;
pub(crate) use checked::{CheckedTypes, ClosureCaptureBinding};

mod c_layout;
mod linking;
pub(crate) use linking::TypeRemap;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct StructLayout {
    pub(crate) name: String,
    pub(crate) repr_c: bool,
    pub(crate) c_layout: Option<CStructLayout>,
    pub(crate) fields: Vec<(String, Type)>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct CStructLayout {
    pub(crate) size: u64,
    pub(crate) align: u64,
    pub(crate) field_offsets: Vec<u64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct ClassLayout {
    pub(crate) drop_effects: Option<EffectSet>,
    pub(crate) name: String,
    pub(crate) fields: Vec<(String, Type)>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(crate) struct EnumLayout {
    pub(crate) name: String,
    pub(crate) variants: Vec<EnumVariantLayout>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumVariantLayout {
    pub(crate) name: String,
    pub(crate) fields: Vec<(String, Type)>,
}

impl TypeTable {
    pub(crate) fn map_key_types(&self) -> impl Iterator<Item = Type> + '_ {
        self.maps.iter().map(|info| info.key)
    }

    pub(crate) fn static_method_name(&self, target: Type, method: &str) -> String {
        let nominal = match target {
            Type::Struct(id) => self.struct_name(id),
            Type::Class(id) => self.class_name(id),
            _ => unreachable!("static implementation requires a nominal type"),
        };
        format!("@static_method/{nominal}/{method}")
    }

    pub(crate) fn static_method_target(&self, name: &str) -> Option<Type> {
        let (nominal, _) = name.strip_prefix("@static_method/")?.rsplit_once('/')?;
        self.struct_type(nominal)
            .or_else(|| self.class_type(nominal))
    }

    pub(super) fn associated_info(&self, id: usize) -> &AssociatedTypeInfo {
        &self.associated_types[id]
    }
    pub(super) fn associated_impls(
        &self,
        receiver: Type,
    ) -> impl Iterator<Item = (&String, &HashMap<String, Type>)> {
        self.trait_associated_impls
            .iter()
            .filter_map(move |((name, ty), values)| (*ty == receiver).then_some((name, values)))
    }

    /// Canonical structural identity; local table indices never enter instance IDs.
    pub(crate) fn stable_type_key(&self, ty: Type, module: crate::module::StableId) -> String {
        let nested = |ty| self.stable_type_key(ty, module);
        let nominal = |name: &str| {
            if name.starts_with('@') {
                name.to_owned()
            } else {
                format!("@{:016x}/{name}", module.0)
            }
        };
        match ty {
            Type::Dyn(id) => {
                let info = &self.dynamic_types[id];
                let mut names = info
                    .trait_names
                    .iter()
                    .map(|name| super::import_methods::trait_key(module, name))
                    .collect::<Vec<_>>();
                names.sort();
                format!(
                    "dyn:{:?}:{:?}",
                    names,
                    info.bindings
                        .iter()
                        .map(|(name, ty)| (name, nested(*ty)))
                        .collect::<Vec<_>>()
                )
            }
            Type::CPtr(id) => format!("cptr:{}", nested(self.c_pointer_type(id))),
            Type::CMutPtr(id) => format!("cmutptr:{}", nested(self.c_pointer_type(id))),
            Type::CArray(id) => {
                let (element, count) = self.c_array_info(id);
                format!("carray:{count}:{}", nested(element))
            }
            Type::Class(id) => format!("class:{}", nominal(self.class_name(id))),
            Type::Struct(id) => format!("struct:{}", nominal(self.struct_name(id))),
            Type::Enum(id) => format!("enum:{}", nominal(self.enum_name(id))),
            Type::Tuple(id) => format!(
                "tuple:{:?}",
                self.tuple_elements(id)
                    .iter()
                    .map(|t| nested(*t))
                    .collect::<Vec<_>>()
            ),
            Type::Option(id) => format!("option:{}", nested(self.option_type(id))),
            Type::Result(id) => {
                let (a, b) = self.result_types(id);
                format!("result:{:?}", [nested(a), nested(b)])
            }
            Type::List(id) => format!("list:{}", nested(self.list_type(id))),
            Type::MutList(id) => format!("mutlist:{}", nested(self.mut_list_type(id))),
            Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                let info = self.map_info(id);
                let kind = match ty {
                    Type::Map(_) => "map",
                    Type::MutMap(_) => "mutmap",
                    _ => "mutset",
                };
                format!("{kind}:{:?}", [nested(info.key), nested(info.value)])
            }
            Type::Cown(id) => format!("cown:{}", nested(self.cown_type(id))),
            Type::Function(id) => {
                let info = self.function_type(id);
                format!(
                    "fn:{:?}:{:?}:{}:{}",
                    info.parameter_names,
                    info.parameters
                        .iter()
                        .map(|t| nested(*t))
                        .collect::<Vec<_>>(),
                    nested(info.return_type),
                    info.suspends
                )
            }
            concrete => super::type_name(concrete),
        }
    }

    pub(crate) fn qualify_nominal_types(&mut self, module: crate::module::StableId) {
        self.effects.qualify(module);
        for info in &mut self.dynamic_types {
            info.qualify(module);
        }
        for info in &mut self.associated_types {
            info.trait_name = super::import_methods::trait_key(module, &info.trait_name);
        }
        self.trait_implementations = std::mem::take(&mut self.trait_implementations)
            .into_iter()
            .map(|(name, values)| (super::import_methods::trait_key(module, &name), values))
            .collect();
        self.trait_associated_impls = std::mem::take(&mut self.trait_associated_impls)
            .into_iter()
            .map(|((name, ty), values)| {
                (
                    (super::import_methods::trait_key(module, &name), ty),
                    values,
                )
            })
            .collect();
        for name in self
            .classes
            .iter_mut()
            .map(|l| &mut l.name)
            .chain(self.structs.iter_mut().map(|l| &mut l.name))
            .chain(self.enums.iter_mut().map(|l| &mut l.name))
        {
            if !name.starts_with('@') {
                *name = format!("@{:016x}/{name}", module.0);
            }
        }
    }

    pub(crate) fn class_name(&self, id: usize) -> &str {
        &self.classes[id].name
    }

    pub(crate) fn module_drop_glue(&self) -> Vec<String> {
        let mut glue = self
            .classes
            .iter()
            .map(|layout| format!("class:{}:owned:v1", layout.name))
            .collect::<Vec<_>>();
        glue.sort();
        glue.dedup();
        glue
    }

    /// Internal iteration layouts must also exist for monomorphized inputs;
    /// they need not have appeared as source-level tuple/Option expressions.
    pub(crate) fn ensure_iteration_types(&mut self, input: Type) {
        if let Some(element) = self.cursor_item_type(input) {
            let pair = Type::Tuple(super::checker::intern_tuple(
                &mut self.tuples,
                vec![element, input],
            ));
            super::checker::intern_option(&mut self.options, pair);
        }
        if let Type::List(id) = input {
            let element = self.lists[id];
            super::checker::intern_option(&mut self.options, element);
            super::checker::intern_tuple(&mut self.tuples, vec![Type::U64, input]);
        }
    }

    pub(crate) fn cursor_item_type(&self, input: Type) -> Option<Type> {
        self.associated_type_value(input, "Cursor", "Item")
    }

    fn associated_type_value(&self, receiver: Type, trait_name: &str, name: &str) -> Option<Type> {
        if let Type::List(id) = receiver {
            if trait_name == "Cursor" && name == "Item" {
                return Some(self.lists[id]);
            }
        }
        if trait_name == "Cursor" && name == "Item" {
            match receiver {
                Type::BytesCursor => return Some(Type::U8),
                // The (K, V) entry tuple was interned when the cursor type was
                // created; look it up instead of interning under &self.
                Type::MapCursor(id) => {
                    let info = self.maps[id];
                    let elements = [info.key, info.value];
                    return self
                        .tuples
                        .iter()
                        .position(|candidate| *candidate == elements)
                        .map(Type::Tuple);
                }
                Type::MapKeyCursor(id) => return Some(self.maps[id].key),
                Type::MapValueCursor(id) => return Some(self.maps[id].value),
                Type::MutListCursor(id) => return Some(self.lists[id]),
                Type::MutMapCursor(id) => {
                    let info = self.maps[id];
                    let elements = [info.key, info.value];
                    return self
                        .tuples
                        .iter()
                        .position(|candidate| *candidate == elements)
                        .map(Type::Tuple);
                }
                Type::MutSetCursor(id) => return Some(self.maps[id].key),
                _ => {}
            }
        }
        self.trait_associated_impls
            .get(&(trait_name.into(), receiver))
            .and_then(|values| values.get(name))
            .copied()
    }

    pub(crate) fn effects(&self) -> &super::effects::EffectRegistry {
        &self.effects
    }

    pub(crate) fn intern_tuple_type(&mut self, elements: Vec<Type>) -> Type {
        if elements.is_empty() {
            Type::Unit
        } else {
            Type::Tuple(super::checker::intern_tuple(&mut self.tuples, elements))
        }
    }

    pub(crate) fn intern_list_type(&mut self, element: Type) -> Type {
        Type::List(super::checker::intern_list(&mut self.lists, element))
    }

    pub(crate) fn intern_option_type(&mut self, value: Type) -> Type {
        Type::Option(super::checker::intern_option(&mut self.options, value))
    }

    pub(crate) fn intern_function_type(
        &mut self,
        parameters: Vec<Type>,
        return_type: Type,
    ) -> Type {
        let parameter_names = vec![String::new(); parameters.len()];
        Type::Function(super::checker::intern_function(
            &mut self.function_types,
            parameter_names,
            parameters,
            return_type,
        ))
    }

    #[allow(dead_code)]
    pub(crate) fn function_type(&self, id: usize) -> &super::symbol_table::FunctionTypeInfo {
        &self.function_types[id]
    }

    pub(crate) fn has_show_impl(&self, ty: Type) -> bool {
        self.show_impls.contains(&ty)
    }

    pub(crate) fn substitute(&self, ty: Type, substitutions: &[Type]) -> Option<Type> {
        self.instantiate(ty, substitutions, None)
    }

    pub(super) fn instantiate(
        &self,
        ty: Type,
        substitutions: &[Type],
        receiver: Option<Type>,
    ) -> Option<Type> {
        match ty {
            Type::SelfType => receiver.or(Some(ty)),
            Type::Param(id) => substitutions.get(id).copied(),
            Type::Tuple(id) => {
                let elements = self.tuple_elements(id);
                let concrete = elements
                    .iter()
                    .map(|element| self.instantiate(*element, substitutions, receiver))
                    .collect::<Option<Vec<_>>>()?;
                self.tuples
                    .iter()
                    .position(|candidate| candidate == &concrete)
                    .map(Type::Tuple)
            }
            Type::Option(id) => self
                .instantiate(self.option_type(id), substitutions, receiver)
                .and_then(|value| self.option_id(value))
                .map(Type::Option),
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                self.result_id(
                    self.instantiate(ok, substitutions, receiver)?,
                    self.instantiate(err, substitutions, receiver)?,
                )
                .map(Type::Result)
            }
            Type::List(id) => self
                .instantiate(self.list_type(id), substitutions, receiver)
                .and_then(|element| self.list_id(element))
                .map(Type::List),
            Type::MutList(id) => self
                .instantiate(self.mut_list_type(id), substitutions, receiver)
                .and_then(|element| self.mut_list_id(element))
                .map(Type::MutList),
            Type::Map(id) => {
                let info = self.map_info(id);
                self.map_id(
                    self.instantiate(info.key, substitutions, receiver)?,
                    self.instantiate(info.value, substitutions, receiver)?,
                )
                .map(Type::Map)
            }
            Type::MutMap(id) => {
                let info = self.map_info(id);
                self.map_id(
                    self.instantiate(info.key, substitutions, receiver)?,
                    self.instantiate(info.value, substitutions, receiver)?,
                )
                .map(Type::MutMap)
            }
            Type::MutSet(id) => {
                let info = self.map_info(id);
                self.map_id(
                    self.instantiate(info.key, substitutions, receiver)?,
                    Type::Bool,
                )
                .map(Type::MutSet)
            }
            Type::Cown(id) => self
                .instantiate(self.cown_type(id), substitutions, receiver)
                .and_then(|value| self.cown_id(value))
                .map(Type::Cown),
            Type::Associated(id) => {
                let info = &self.associated_types[id];
                let receiver = match info.parameter {
                    Some(parameter) => substitutions.get(parameter).copied(),
                    None => receiver,
                }?;
                self.associated_type_value(receiver, &info.trait_name, &info.name)
            }
            Type::Function(id) => {
                let info = self.function_type(id);
                let parameters = info
                    .parameters
                    .iter()
                    .map(|ty| self.instantiate(*ty, substitutions, receiver))
                    .collect::<Option<Vec<_>>>()?;
                let return_type = self.instantiate(info.return_type, substitutions, receiver)?;
                self.function_types
                    .iter()
                    .position(|candidate| {
                        candidate.parameter_names == info.parameter_names
                            && candidate.parameters == parameters
                            && candidate.return_type == return_type
                            && candidate.effects == info.effects
                    })
                    .map(Type::Function)
            }
            concrete => Some(concrete),
        }
    }

    pub(crate) fn contains_type_parameter(&self, ty: Type) -> bool {
        match ty {
            Type::Param(_) => true,
            Type::Associated(id) => self.associated_types[id].parameter.is_some(),
            Type::Tuple(id) => self
                .tuple_elements(id)
                .iter()
                .any(|element| self.contains_type_parameter(*element)),
            Type::Option(id) => self.contains_type_parameter(self.option_type(id)),
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                self.contains_type_parameter(ok) || self.contains_type_parameter(err)
            }
            Type::List(id) => self.contains_type_parameter(self.list_type(id)),
            Type::MutList(id) => self.contains_type_parameter(self.mut_list_type(id)),
            Type::Map(id) => {
                let info = self.map_info(id);
                self.contains_type_parameter(info.key) || self.contains_type_parameter(info.value)
            }
            Type::MutMap(id) => {
                let info = self.map_info(id);
                self.contains_type_parameter(info.key) || self.contains_type_parameter(info.value)
            }
            Type::MutSet(id) => self.contains_type_parameter(self.map_info(id).key),
            Type::Cown(id) => self.contains_type_parameter(self.cown_type(id)),
            Type::Function(id) => {
                let info = self.function_type(id);
                info.parameters
                    .iter()
                    .any(|ty| self.contains_type_parameter(*ty))
                    || self.contains_type_parameter(info.return_type)
            }
            _ => false,
        }
    }

    pub(crate) fn tuple_id(&self, elements: &[Type]) -> Option<usize> {
        self.tuples.iter().position(|tuple| tuple == elements)
    }

    pub(crate) fn tuple_elements(&self, id: usize) -> &[Type] {
        self.tuples.get(id).expect("each tuple type has a layout")
    }

    pub(crate) fn c_pointer_type(&self, id: usize) -> Type {
        self.c_pointers[id]
    }

    pub(crate) fn c_pointer_id(&self, pointee: Type) -> Option<usize> {
        self.c_pointers.iter().position(|ty| *ty == pointee)
    }
    pub(crate) fn c_array_info(&self, id: usize) -> (Type, u64) {
        self.c_arrays[id]
    }

    pub(crate) fn struct_fields(&self, id: usize) -> &[(String, Type)] {
        &self.structs[id].fields
    }
    pub(crate) fn struct_is_repr_c(&self, id: usize) -> bool {
        self.structs[id].repr_c
    }

    pub(crate) fn struct_name(&self, id: usize) -> &str {
        &self.structs[id].name
    }

    pub(crate) fn struct_type(&self, name: &str) -> Option<Type> {
        self.structs
            .iter()
            .position(|layout| layout.name == name)
            .map(Type::Struct)
    }

    pub(crate) fn class_drop_effects(&self, id: usize) -> Option<&EffectSet> {
        self.classes[id].drop_effects.as_ref()
    }

    pub(crate) fn class_fields(&self, id: usize) -> &[(String, Type)] {
        &self.classes[id].fields
    }

    pub(crate) fn class_count(&self) -> usize {
        self.classes.len()
    }

    pub(crate) fn class_type(&self, name: &str) -> Option<Type> {
        self.classes
            .iter()
            .position(|layout| layout.name == name)
            .map(Type::Class)
    }

    pub(crate) fn enum_type(&self, name: &str) -> Option<Type> {
        self.enums
            .iter()
            .position(|layout| layout.name == name)
            .map(Type::Enum)
    }

    pub(crate) fn enum_variants(&self, id: usize) -> &[EnumVariantLayout] {
        &self.enums[id].variants
    }

    pub(crate) fn enum_name(&self, id: usize) -> &str {
        &self.enums[id].name
    }

    pub(crate) fn option_type(&self, id: usize) -> Type {
        self.options[id]
    }

    pub(crate) fn option_id(&self, value: Type) -> Option<usize> {
        self.options
            .iter()
            .position(|candidate| *candidate == value)
    }

    pub(crate) fn result_types(&self, id: usize) -> (Type, Type) {
        self.results[id]
    }

    pub(crate) fn result_id(&self, ok: Type, err: Type) -> Option<usize> {
        self.results
            .iter()
            .position(|candidate| *candidate == (ok, err))
    }

    pub(crate) fn list_type(&self, id: usize) -> Type {
        self.lists[id]
    }

    pub(crate) fn list_id(&self, element: Type) -> Option<usize> {
        self.lists
            .iter()
            .position(|candidate| *candidate == element)
    }

    pub(crate) fn mut_list_type(&self, id: usize) -> Type {
        self.lists[id]
    }

    pub(crate) fn mut_list_id(&self, element: Type) -> Option<usize> {
        self.list_id(element)
    }

    pub(crate) fn map_info(&self, id: usize) -> MapInfo {
        self.maps[id]
    }

    pub(crate) fn map_id(&self, key: Type, value: Type) -> Option<usize> {
        self.maps
            .iter()
            .position(|candidate| candidate.key == key && candidate.value == value)
    }

    pub(crate) fn cown_type(&self, id: usize) -> Type {
        self.cowns[id]
    }

    pub(crate) fn cown_id(&self, payload: Type) -> Option<usize> {
        self.cowns
            .iter()
            .position(|candidate| *candidate == payload)
    }

    pub(crate) fn is_owned(&self, ty: Type) -> bool {
        self.is_owned_inner(ty, &mut Vec::new())
    }

    pub(crate) fn is_shared(&self, ty: Type) -> bool {
        !self.is_owned(ty) && self.is_shared_inner(ty, &mut Vec::new())
    }

    pub(crate) fn needs_drop(&self, ty: Type) -> bool {
        self.is_owned(ty) || self.is_shared(ty)
    }

    fn is_owned_inner(&self, ty: Type, visiting: &mut Vec<Type>) -> bool {
        if matches!(
            ty,
            Type::Class(_)
                | Type::Hasher
                | Type::MapCursor(_)
                | Type::MapKeyCursor(_)
                | Type::MapValueCursor(_)
                | Type::MutListCursor(_)
                | Type::MutMapCursor(_)
                | Type::MutSetCursor(_)
                | Type::Dyn(_)
                | Type::MutBytes
                | Type::Native(_)
                | Type::CCallback
        ) {
            return true;
        }
        if visiting.contains(&ty) {
            return false;
        }
        visiting.push(ty);
        let owned = match ty {
            Type::Tuple(id) => self
                .tuple_elements(id)
                .iter()
                .any(|field| self.is_owned_inner(*field, visiting)),
            Type::Struct(id) => self
                .struct_fields(id)
                .iter()
                .any(|(_, field)| self.is_owned_inner(*field, visiting)),
            Type::Enum(id) => self.enum_variants(id).iter().any(|variant| {
                variant
                    .fields
                    .iter()
                    .any(|(_, field)| self.is_owned_inner(*field, visiting))
            }),
            Type::Option(id) => self.is_owned_inner(self.option_type(id), visiting),
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                self.is_owned_inner(ok, visiting) || self.is_owned_inner(err, visiting)
            }
            Type::List(_) | Type::Map(_) => false,
            Type::MutList(_) | Type::MutMap(_) => true,
            Type::MutSet(_) => true,
            Type::Cown(_) => false,
            Type::Function(_) => true,
            _ => false,
        };
        visiting.pop();
        owned
    }

    fn is_shared_inner(&self, ty: Type, visiting: &mut Vec<Type>) -> bool {
        if matches!(
            ty,
            Type::String | Type::Bytes | Type::Batch | Type::SeqBuilder | Type::BytesCursor
        ) {
            return true;
        }
        if visiting.contains(&ty) {
            return false;
        }
        visiting.push(ty);
        let shared = match ty {
            Type::Tuple(id) => self
                .tuple_elements(id)
                .iter()
                .any(|field| self.is_shared_inner(*field, visiting)),
            Type::Struct(id) => self
                .struct_fields(id)
                .iter()
                .any(|(_, field)| self.is_shared_inner(*field, visiting)),
            Type::Enum(id) => self.enum_variants(id).iter().any(|variant| {
                variant
                    .fields
                    .iter()
                    .any(|(_, field)| self.is_shared_inner(*field, visiting))
            }),
            Type::Option(id) => self.is_shared_inner(self.option_type(id), visiting),
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                self.is_shared_inner(ok, visiting) || self.is_shared_inner(err, visiting)
            }
            Type::List(_) | Type::Map(_) => true,
            Type::MutList(_) | Type::MutMap(_) => false,
            Type::MutSet(_) => false,
            Type::Cown(_) => true,
            Type::Function(_) => false,
            _ => false,
        };
        visiting.pop();
        shared
    }
}

impl TypeTable {
    pub(super) fn explicit_hash_impls(checker: &super::checker::Checker) -> HashSet<Type> {
        checker
            .structs
            .values()
            .filter(|info| info.methods.contains_key(crate::sema::HASH_METHOD))
            .map(|info| Type::Struct(info.id))
            .chain(
                checker
                    .classes
                    .values()
                    .filter(|info| info.methods.contains_key(crate::sema::HASH_METHOD))
                    .map(|info| Type::Class(info.id)),
            )
            .collect()
    }

    /// Compiler-provided hashing; explicit impls always use method dispatch.
    pub(crate) fn has_builtin_hash(&self, ty: Type) -> bool {
        ty.is_integer()
            || matches!(
                ty,
                Type::String | Type::Bool | Type::Bytes | Type::Duration | Type::Unit
            )
            || matches!(ty, Type::Enum(id) if self.enum_variants(id).iter().all(|v| {
                v.fields.iter().all(|(_, field_ty)| {
                    self.has_builtin_hash(*field_ty) || self.explicit_hash_impls.contains(field_ty)
                })
            }))
            || matches!(ty, Type::Tuple(id) if self.tuple_elements(id).iter().all(|member| {
                self.has_builtin_hash(*member) || self.explicit_hash_impls.contains(member)
            }))
            || (matches!(ty, Type::Struct(id) if
            self.trait_implementations
                .get("Hash")
                .is_some_and(|types| types.contains(&ty))
            && !self.explicit_hash_impls.contains(&ty)
            && self.struct_fields(id).iter().all(|(_, field_ty)| {
                self.has_builtin_hash(*field_ty)
                    || self.explicit_hash_impls.contains(field_ty)
                    || self.has_structural_key_fields(*field_ty)
            })))
    }

    /// Compiler-provided equality; explicit impls always use method dispatch.
    pub(crate) fn has_builtin_partial_eq(&self, ty: Type) -> bool {
        if self
            .trait_implementations
            .get("PartialEq")
            .is_some_and(|types| types.contains(&ty))
        {
            return false;
        }
        if let Some(members) = self.comparison_members(ty) {
            return members.into_iter().all(|member| {
                self.has_builtin_partial_eq(member)
                    || self
                        .trait_implementations
                        .get("PartialEq")
                        .is_some_and(|types| types.contains(&member))
            });
        }
        ty.is_numeric()
            || matches!(
                ty,
                Type::String | Type::Bytes | Type::Bool | Type::Duration | Type::Unit
            )
            || matches!(ty, Type::Enum(id) if self.enum_variants(id).iter().all(|v| v.fields.is_empty()))
            || (matches!(ty, Type::Struct(_))
                && self
                    .trait_implementations
                    .get("Eq")
                    .is_some_and(|types| types.contains(&ty))
                && self.has_structural_key_fields(ty))
    }

    pub(crate) fn comparison_members(&self, ty: Type) -> Option<Vec<Type>> {
        Some(match ty {
            Type::Tuple(id) => self.tuple_elements(id).to_vec(),
            Type::Option(id) => vec![self.option_type(id)],
            Type::Result(id) => {
                let (ok, err) = self.result_types(id);
                vec![ok, err]
            }
            Type::List(id) => vec![self.list_type(id)],
            _ => return None,
        })
    }

    fn has_structural_key_fields(&self, ty: Type) -> bool {
        if self
            .trait_implementations
            .get("PartialEq")
            .is_some_and(|types| types.contains(&ty))
        {
            return false;
        }
        ty.is_integer()
            || matches!(
                ty,
                Type::String | Type::Bool | Type::Bytes | Type::Duration | Type::Unit
            )
            || matches!(ty, Type::Enum(id) if self.enum_variants(id).iter().all(|v| v.fields.is_empty()))
            || matches!(ty, Type::Tuple(id) if self.tuple_elements(id).iter().all(|member| self.has_structural_key_fields(*member)))
            || matches!(ty, Type::Struct(id) if !self.struct_fields(id).is_empty() && self.struct_fields(id).iter().all(|(_, field)| self.has_structural_key_fields(*field)))
    }
}

impl TypeTable {
    pub(crate) fn ordering_type(&self) -> Type {
        self.enum_type(crate::sema::ORDERING_NAME)
            .expect("builtin Ordering layout")
    }
    pub(crate) fn partial_ordering_type(&self) -> Type {
        Type::Option(
            self.option_id(self.ordering_type())
                .expect("builtin Option(Ordering) layout"),
        )
    }
    pub(crate) fn has_builtin_ordering(&self, ty: Type, total: bool) -> bool {
        let trait_name = if total { "Ord" } else { "PartialOrd" };
        let explicit = |ty| {
            self.trait_implementations
                .get(trait_name)
                .is_some_and(|types| types.contains(&ty))
        };
        if explicit(ty) {
            return false;
        }
        if let Some(members) = self.comparison_members(ty) {
            return members
                .into_iter()
                .all(|member| self.has_builtin_ordering(member, total) || explicit(member));
        }
        (!total || !ty.is_float())
            && (ty.is_numeric()
                || matches!(ty, Type::String | Type::Bytes | Type::Bool | Type::Duration)
                || self.enum_type(crate::sema::ORDERING_NAME) == Some(ty))
    }
}
