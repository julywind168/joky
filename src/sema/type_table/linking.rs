//! Relocate artifact-local type indices into a single link-time table.

use super::*;
use crate::module::StableId;
use crate::sema::EffectOperationId;

#[derive(Debug, Default)]
pub(crate) struct TypeRemap {
    pub types: HashMap<Type, Type>,
    pub operations: HashMap<EffectOperationId, EffectOperationId>,
    pub groups: HashMap<crate::sema::EffectId, crate::sema::EffectId>,
}

impl TypeRemap {
    pub fn ty(&self, ty: Type) -> Type {
        self.types.get(&ty).copied().unwrap_or(ty)
    }
    pub fn operation(&self, id: EffectOperationId) -> EffectOperationId {
        self.operations[&id]
    }
}

impl TypeTable {
    pub(crate) fn merge_module(
        &mut self,
        source: &Self,
        module: StableId,
    ) -> Result<TypeRemap, String> {
        let mut map = TypeRemap::default();
        let existing_classes = self.classes.len();
        let existing_structs = self.structs.len();
        let existing_enums = self.enums.len();
        // Reserve nominal layouts before walking their fields, allowing recursive classes.
        for (i, layout) in source.classes.iter().enumerate() {
            let name = if layout.name.starts_with('@') {
                layout.name.clone()
            } else {
                format!("@{:016x}/{}", module.0, layout.name)
            };
            let id = self
                .classes
                .iter()
                .position(|l| l.name == name)
                .unwrap_or_else(|| {
                    let id = self.classes.len();
                    self.classes.push(ClassLayout {
                        drop_effects: None,
                        name,
                        fields: vec![],
                    });
                    id
                });
            map.types.insert(Type::Class(i), Type::Class(id));
        }
        for (i, layout) in source.structs.iter().enumerate() {
            let name = nominal_name(&layout.name, module);
            let id = self
                .structs
                .iter()
                .position(|l| l.name == name)
                .unwrap_or_else(|| {
                    let id = self.structs.len();
                    self.structs.push(StructLayout {
                        repr_c: layout.repr_c,
                        c_layout: layout.c_layout.clone(),
                        name,
                        fields: vec![],
                    });
                    id
                });
            map.types.insert(Type::Struct(i), Type::Struct(id));
        }
        for (i, layout) in source.enums.iter().enumerate() {
            let name = nominal_name(&layout.name, module);
            let id = self
                .enums
                .iter()
                .position(|l| l.name == name)
                .unwrap_or_else(|| {
                    let id = self.enums.len();
                    self.enums.push(EnumLayout {
                        name,
                        variants: vec![],
                    });
                    id
                });
            map.types.insert(Type::Enum(i), Type::Enum(id));
        }
        // Reserve effect groups and operation IDs before mapping function types.
        let mut new_effects = HashSet::new();
        for effect in source.effects.iter() {
            let name = effect_link_name(effect, module);
            let existing = self.effects.by_name(&name);
            if existing.is_some_and(|id| {
                self.effects.effect(id).unwrap().operations.len() != effect.operations.len()
            }) {
                return Err(format!(
                    "effect operation count ABI mismatch for {}",
                    effect.name
                ));
            }
            let id = self.effects.define(name);
            map.groups.insert(effect.id, id);
            if existing.is_none() {
                new_effects.insert(id);
            }
            for operation in &effect.operations {
                let target = if existing.is_none() {
                    self.effects.operation(
                        id,
                        operation.name.clone(),
                        vec![],
                        Type::Unit,
                        operation.mode,
                        operation.suspends,
                    )
                } else {
                    self.effects
                        .operation_by_name(id, &operation.name)
                        .ok_or_else(|| format!("effect ABI mismatch for {}", effect.name))?
                };
                map.operations.insert(operation.id, target);
            }
        }
        let roots = (0..source.tuples.len())
            .map(Type::Tuple)
            .chain((0..source.options.len()).map(Type::Option))
            .chain((0..source.results.len()).map(Type::Result))
            .chain((0..source.lists.len()).flat_map(|i| [Type::List(i), Type::MutList(i)]))
            .chain(
                (0..source.maps.len())
                    .flat_map(|i| [Type::Map(i), Type::MutMap(i), Type::MutSet(i)]),
            )
            .chain((0..source.cowns.len()).map(Type::Cown))
            .chain((0..source.c_pointers.len()).flat_map(|i| [Type::CPtr(i), Type::CMutPtr(i)]))
            .chain((0..source.c_arrays.len()).map(Type::CArray))
            .chain((0..source.function_types.len()).map(Type::Function))
            .chain((0..source.dynamic_types.len()).map(Type::Dyn));
        for ty in roots {
            self.import_type(source, ty, &mut map)?;
        }
        for (i, layout) in source.classes.iter().enumerate() {
            let Type::Class(id) = map.ty(Type::Class(i)) else {
                unreachable!()
            };
            let fields: Vec<_> = layout
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), map.ty(*ty)))
                .collect();
            if id < existing_classes && self.classes[id].fields != fields {
                return Err(format!(
                    "class layout ABI mismatch for '{}'",
                    self.classes[id].name
                ));
            }
            self.classes[id].fields = fields;
        }
        for (i, layout) in source.structs.iter().enumerate() {
            let Type::Struct(id) = map.ty(Type::Struct(i)) else {
                unreachable!()
            };
            let fields: Vec<_> = layout
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), map.ty(*ty)))
                .collect();
            if id < existing_structs
                && (self.structs[id].fields != fields
                    || self.structs[id].repr_c != layout.repr_c
                    || self.structs[id].c_layout != layout.c_layout)
            {
                return Err(format!(
                    "struct layout ABI mismatch for {}",
                    self.structs[id].name
                ));
            }
            self.structs[id].fields = fields;
        }
        for (i, layout) in source.enums.iter().enumerate() {
            let Type::Enum(id) = map.ty(Type::Enum(i)) else {
                unreachable!()
            };
            let variants: Vec<_> = layout
                .variants
                .iter()
                .map(|v| EnumVariantLayout {
                    name: v.name.clone(),
                    fields: v
                        .fields
                        .iter()
                        .map(|(n, t)| (n.clone(), map.ty(*t)))
                        .collect(),
                })
                .collect();
            if id < existing_enums && self.enums[id].variants != variants {
                return Err(format!(
                    "enum layout ABI mismatch for {}",
                    self.enums[id].name
                ));
            }
            self.enums[id].variants = variants;
        }
        for effect in source.effects.iter() {
            for op in &effect.operations {
                let mut mapped = op.clone();
                mapped.id = map.operation(op.id);
                mapped.parameters = op.parameters.iter().map(|ty| map.ty(*ty)).collect();
                mapped.return_type = map.ty(op.return_type);
                if !new_effects.contains(&mapped.id.effect)
                    && self.effects.operation_info(mapped.id) != Some(&mapped)
                {
                    return Err(format!(
                        "effect operation ABI mismatch for {}.{}",
                        effect.name, op.name
                    ));
                }
                self.effects.replace_operation(mapped);
            }
        }
        for (source_id, layout) in source.classes.iter().enumerate() {
            let Type::Class(id) = map.ty(Type::Class(source_id)) else {
                unreachable!()
            };
            let drop_effects = layout.drop_effects.as_ref().map(|effects| {
                let mut mapped = EffectSet::new();
                for op in effects.iter() {
                    mapped.insert(map.operation(op));
                }
                mapped
            });
            if drop_effects.is_some() {
                self.classes[id].drop_effects = drop_effects;
            }
        }
        self.show_impls
            .extend(source.show_impls.iter().map(|ty| map.ty(*ty)));
        self.explicit_hash_impls
            .extend(source.explicit_hash_impls.iter().map(|ty| map.ty(*ty)));
        for (name, implementations) in &source.trait_implementations {
            self.trait_implementations
                .entry(super::super::trait_key(module, name))
                .or_default()
                .extend(implementations.iter().map(|ty| map.ty(*ty)));
        }
        Ok(map)
    }

    fn import_type(
        &mut self,
        source: &Self,
        ty: Type,
        map: &mut TypeRemap,
    ) -> Result<Type, String> {
        if let Some(mapped) = map.types.get(&ty) {
            return Ok(*mapped);
        }
        fn intern<T: PartialEq>(table: &mut Vec<T>, value: T) -> usize {
            table.iter().position(|v| v == &value).unwrap_or_else(|| {
                let id = table.len();
                table.push(value);
                id
            })
        }
        let mapped = match ty {
            Type::Dyn(id) => {
                let mut info = source.dynamic_types[id].clone();
                info.bindings = info
                    .bindings
                    .iter()
                    .map(|(name, ty)| Ok((name.clone(), self.import_type(source, *ty, map)?)))
                    .collect::<Result<_, String>>()?;
                for method in &mut info.methods {
                    let Type::Function(signature) =
                        self.import_type(source, Type::Function(method.signature), map)?
                    else {
                        unreachable!()
                    };
                    method.signature = signature;
                }
                let id = if let Some(id) = self.dynamic_types.iter().position(|item| {
                    item.trait_names == info.trait_names && item.bindings == info.bindings
                }) {
                    let previous = &self.dynamic_types[id];
                    if previous
                        .methods
                        .iter()
                        .map(|m| (&m.name, m.receiver, m.signature, &m.parameter_borrows))
                        .ne(info
                            .methods
                            .iter()
                            .map(|m| (&m.name, m.receiver, m.signature, &m.parameter_borrows)))
                    {
                        return Err("dynamic vtable ABI mismatch".into());
                    }
                    id
                } else {
                    let id = self.dynamic_types.len();
                    self.dynamic_types.push(info);
                    id
                };
                Type::Dyn(id)
            }
            Type::Tuple(id) => {
                let elements = source.tuples[id]
                    .iter()
                    .map(|t| self.import_type(source, *t, map))
                    .collect::<Result<Vec<_>, _>>()?;
                Type::Tuple(intern(&mut self.tuples, elements))
            }
            Type::Option(id) => {
                let value = self.import_type(source, source.options[id], map)?;
                Type::Option(intern(&mut self.options, value))
            }
            Type::Result(id) => {
                let (a, b) = source.results[id];
                let a = self.import_type(source, a, map)?;
                let b = self.import_type(source, b, map)?;
                Type::Result(intern(&mut self.results, (a, b)))
            }
            Type::List(id) | Type::MutList(id) => {
                let value = self.import_type(source, source.lists[id], map)?;
                let id = intern(&mut self.lists, value);
                if matches!(ty, Type::List(_)) {
                    Type::List(id)
                } else {
                    Type::MutList(id)
                }
            }
            Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                let value = &source.maps[id];
                let key = self.import_type(source, value.key, map)?;
                let value = self.import_type(source, value.value, map)?;
                let id = self
                    .maps
                    .iter()
                    .position(|m| m.key == key && m.value == value)
                    .unwrap_or_else(|| {
                        let id = self.maps.len();
                        self.maps.push(MapInfo { key, value });
                        id
                    });
                match ty {
                    Type::Map(_) => Type::Map(id),
                    Type::MutMap(_) => Type::MutMap(id),
                    _ => Type::MutSet(id),
                }
            }
            Type::Cown(id) => {
                let value = self.import_type(source, source.cowns[id], map)?;
                Type::Cown(intern(&mut self.cowns, value))
            }
            Type::CPtr(id) | Type::CMutPtr(id) => {
                let pointee = self.import_type(source, source.c_pointer_type(id), map)?;
                let local = self
                    .c_pointers
                    .iter()
                    .position(|ty| *ty == pointee)
                    .unwrap_or_else(|| {
                        let id = self.c_pointers.len();
                        self.c_pointers.push(pointee);
                        id
                    });
                if matches!(ty, Type::CPtr(_)) {
                    Type::CPtr(local)
                } else {
                    Type::CMutPtr(local)
                }
            }
            Type::CArray(id) => {
                let (element, count) = source.c_array_info(id);
                let element = self.import_type(source, element, map)?;
                let local = self
                    .c_arrays
                    .iter()
                    .position(|entry| *entry == (element, count))
                    .unwrap_or_else(|| {
                        let id = self.c_arrays.len();
                        self.c_arrays.push((element, count));
                        id
                    });
                Type::CArray(local)
            }
            Type::Function(id) => {
                let info = &source.function_types[id];
                let parameters = info
                    .parameters
                    .iter()
                    .map(|t| self.import_type(source, *t, map))
                    .collect::<Result<Vec<_>, _>>()?;
                let return_type = self.import_type(source, info.return_type, map)?;
                let mut effects = crate::sema::EffectSet::new();
                for op in info.effects.iter() {
                    effects.insert(map.operation(op));
                }
                let id = self
                    .function_types
                    .iter()
                    .position(|f| {
                        f.parameter_names == info.parameter_names
                            && f.parameters == parameters
                            && f.return_type == return_type
                            && f.effects == effects
                            && f.suspends == info.suspends
                    })
                    .unwrap_or_else(|| {
                        let id = self.function_types.len();
                        self.function_types
                            .push(crate::sema::symbol_table::FunctionTypeInfo {
                                parameter_names: info.parameter_names.clone(),
                                parameters,
                                return_type,
                                effects,
                                suspends: info.suspends,
                            });
                        id
                    });
                Type::Function(id)
            }
            Type::Struct(_) | Type::Class(_) | Type::Enum(_) => {
                return Err("unregistered nominal ABI type".into())
            }
            Type::Associated(id) => {
                // Checked template metadata can retain symbolic associated
                // types; executable MIR refers to concrete substitutions.
                let info = &source.associated_types[id];
                let id = self
                    .associated_types
                    .iter()
                    .position(|value| {
                        value.parameter == info.parameter
                            && value.trait_name == info.trait_name
                            && value.name == info.name
                    })
                    .unwrap_or_else(|| {
                        let id = self.associated_types.len();
                        self.associated_types.push(info.clone());
                        id
                    });
                Type::Associated(id)
            }
            primitive => primitive,
        };
        map.types.insert(ty, mapped);
        Ok(mapped)
    }
}

fn effect_link_name(effect: &crate::sema::effects::EffectInfo, module: StableId) -> String {
    if let Some(name) = effect.identity.strip_prefix("@runtime/") {
        name.to_owned()
    } else if effect.identity.starts_with('@') {
        effect.identity.clone()
    } else {
        format!("@{:016x}/{}", module.0, effect.name)
    }
}

fn nominal_name(name: &str, module: StableId) -> String {
    if name.starts_with('@') {
        name.into()
    } else {
        format!("@{:016x}/{name}", module.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table(source: &str) -> TypeTable {
        let mut table =
            crate::sema::check_module(&crate::syntax::parse_program(source).unwrap()).unwrap();
        table.qualify_nominal_types(StableId(1));
        table.module.types
    }

    #[test]
    fn changing_an_empty_class_layout_is_an_abi_error() {
        let mut linked = TypeTable::default();
        linked
            .merge_module(&table("class Box {}"), StableId(1))
            .unwrap();
        assert!(linked
            .merge_module(&table("class Box { let value: String }"), StableId(1))
            .unwrap_err()
            .contains("class layout ABI mismatch"));
    }

    #[test]
    fn effect_operation_order_is_relocated_by_name() {
        let first = table("eff E { fn first() -> Int32; fn second() -> String }");
        let second = table("eff E { fn second() -> String; fn first() -> Int32 }");
        let mut linked = TypeTable::default();
        let a = linked.merge_module(&first, StableId(1)).unwrap();
        let b = linked.merge_module(&second, StableId(1)).unwrap();
        let a_op = first
            .effects
            .operation_by_name(first.effects.by_name("E").unwrap(), "first")
            .unwrap();
        let b_op = second
            .effects
            .operation_by_name(second.effects.by_name("E").unwrap(), "first")
            .unwrap();
        assert_eq!(a.operation(a_op), b.operation(b_op));
    }
}
