use std::collections::HashSet;

use crate::diagnostic::SemanticError;
use crate::syntax::{Enum, Struct};

use crate::sema::checker::Checker;
use crate::sema::types::Type;
use crate::sema::{ClassLayout, EnumLayout, EnumVariantLayout, StructLayout};

impl Checker {
    pub(in crate::sema) fn validate_enum_layouts(
        &self,
        definitions: &[Enum],
    ) -> Result<(), SemanticError> {
        let mut dependencies = vec![HashSet::new(); self.enums.len()];
        for enumeration in self.enums.values() {
            for variant in &enumeration.variants {
                for (_, field_type) in &variant.fields {
                    self.collect_enum_dependencies(
                        *field_type,
                        &mut dependencies[enumeration.id],
                        &mut HashSet::new(),
                        &mut HashSet::new(),
                    );
                }
            }
        }

        fn find_cycle(
            enum_id: usize,
            dependencies: &[HashSet<usize>],
            states: &mut [u8],
        ) -> Option<usize> {
            if states[enum_id] == 1 {
                return Some(enum_id);
            }
            if states[enum_id] == 2 {
                return None;
            }
            states[enum_id] = 1;
            for dependency in &dependencies[enum_id] {
                if let Some(cycle) = find_cycle(*dependency, dependencies, states) {
                    return Some(cycle);
                }
            }
            states[enum_id] = 2;
            None
        }

        let mut states = vec![0; self.enums.len()];
        for enum_id in 0..self.enums.len() {
            if let Some(cycle) = find_cycle(enum_id, &dependencies, &mut states) {
                let definition = definitions
                    .iter()
                    .find(|definition| self.enums[&definition.name].id == cycle)
                    .expect("each enum layout has a definition");
                return Err(SemanticError::RecursiveEnumDefinition {
                    name: definition.name.clone(),
                    span: definition.span,
                });
            }
        }
        Ok(())
    }

    fn collect_enum_dependencies(
        &self,
        value_type: Type,
        dependencies: &mut HashSet<usize>,
        visited_tuples: &mut HashSet<usize>,
        visited_structs: &mut HashSet<usize>,
    ) {
        match value_type {
            Type::Enum(id) => {
                dependencies.insert(id);
            }
            Type::Tuple(id) if visited_tuples.insert(id) => {
                for element in &self.tuple_types[id] {
                    self.collect_enum_dependencies(
                        *element,
                        dependencies,
                        visited_tuples,
                        visited_structs,
                    );
                }
            }
            Type::Struct(id) if visited_structs.insert(id) => {
                let structure = self
                    .structs
                    .values()
                    .find(|structure| structure.id == id)
                    .expect("resolved struct type");
                for (_, field_type) in &structure.fields {
                    self.collect_enum_dependencies(
                        *field_type,
                        dependencies,
                        visited_tuples,
                        visited_structs,
                    );
                }
            }
            _ => {}
        }
    }

    pub(in crate::sema) fn validate_struct_layouts(
        &self,
        definitions: &[Struct],
    ) -> Result<(), SemanticError> {
        let mut dependencies = vec![HashSet::new(); self.structs.len()];
        for structure in self.structs.values() {
            for (_, field_type) in &structure.fields {
                self.collect_struct_dependencies(
                    *field_type,
                    &mut dependencies[structure.id],
                    &mut HashSet::new(),
                    &mut HashSet::new(),
                );
            }
        }

        fn find_cycle(
            struct_id: usize,
            dependencies: &[HashSet<usize>],
            states: &mut [u8],
        ) -> Option<usize> {
            if states[struct_id] == 1 {
                return Some(struct_id);
            }
            if states[struct_id] == 2 {
                return None;
            }
            states[struct_id] = 1;
            for dependency in &dependencies[struct_id] {
                if let Some(cycle) = find_cycle(*dependency, dependencies, states) {
                    return Some(cycle);
                }
            }
            states[struct_id] = 2;
            None
        }

        let mut states = vec![0; self.structs.len()];
        for struct_id in 0..self.structs.len() {
            if let Some(cycle) = find_cycle(struct_id, &dependencies, &mut states) {
                let definition = definitions
                    .iter()
                    .find(|definition| self.structs[&definition.name].id == cycle)
                    .expect("each struct layout has a definition");
                return Err(SemanticError::RecursiveStructDefinition {
                    name: definition.name.clone(),
                    span: definition.span,
                });
            }
        }
        Ok(())
    }

    fn collect_struct_dependencies(
        &self,
        value_type: Type,
        dependencies: &mut HashSet<usize>,
        visited_tuples: &mut HashSet<usize>,
        visited_enums: &mut HashSet<usize>,
    ) {
        match value_type {
            Type::Struct(id) => {
                dependencies.insert(id);
            }
            Type::Tuple(id) if visited_tuples.insert(id) => {
                for element in &self.tuple_types[id] {
                    self.collect_struct_dependencies(
                        *element,
                        dependencies,
                        visited_tuples,
                        visited_enums,
                    );
                }
            }
            Type::Enum(id) if visited_enums.insert(id) => {
                let enumeration = self
                    .enums
                    .values()
                    .find(|enumeration| enumeration.id == id)
                    .expect("resolved enum type");
                for variant in &enumeration.variants {
                    for (_, field_type) in &variant.fields {
                        self.collect_struct_dependencies(
                            *field_type,
                            dependencies,
                            visited_tuples,
                            visited_enums,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub(in crate::sema) fn enum_layouts(&self) -> Vec<EnumLayout> {
        let mut layouts = self
            .enums
            .iter()
            .map(|(name, info)| {
                (
                    info.id,
                    EnumLayout {
                        name: if name == "Ordering" {
                            crate::sema::ORDERING_NAME.into()
                        } else {
                            name.clone()
                        },
                        variants: info
                            .variants
                            .iter()
                            .map(|variant| EnumVariantLayout {
                                name: variant.name.clone(),
                                fields: variant.fields.clone(),
                            })
                            .collect(),
                    },
                )
            })
            .collect::<Vec<_>>();
        layouts.sort_by_key(|(id, _)| *id);
        layouts.into_iter().map(|(_, layout)| layout).collect()
    }

    pub(in crate::sema) fn struct_layouts(&self) -> Vec<StructLayout> {
        let mut layouts = self
            .structs
            .iter()
            .map(|(name, info)| (info.id, name.clone(), info.repr_c, info.fields.clone()))
            .collect::<Vec<_>>();
        layouts.sort_by_key(|(id, _, _, _)| *id);
        layouts
            .into_iter()
            .map(|(_, name, repr_c, fields)| StructLayout {
                name,
                repr_c,
                // Filled only after all local and imported types are resolved.
                c_layout: None,
                fields,
            })
            .collect()
    }

    pub(in crate::sema) fn class_layouts(&self) -> Vec<ClassLayout> {
        let mut layouts = self
            .classes
            .iter()
            .map(|(name, info)| {
                (
                    info.id,
                    name.clone(),
                    info.fields
                        .iter()
                        .map(|(field_name, field, _)| (field_name.clone(), field.ty))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        layouts.sort_by_key(|(id, _, _)| *id);
        layouts
            .into_iter()
            .map(|(id, name, fields)| {
                let drop_effects = self
                    .trait_impls
                    .get("Drop")
                    .filter(|types| types.contains(&Type::Class(id)))
                    .map(|_| {
                        let mut effects = super::super::effects::EffectSet::new();
                        if let Some(method) =
                            self.classes[&name].methods.get(crate::sema::DROP_METHOD)
                        {
                            for group in method.declared_effects.iter() {
                                for op in self.effects.all_operations(group) {
                                    effects.insert(op);
                                }
                            }
                        }
                        effects
                    });
                ClassLayout {
                    name,
                    fields,
                    drop_effects,
                }
            })
            .collect()
    }
}
