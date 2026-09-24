//! User destruction contracts. Owning a value may require synchronous cleanup.
use std::collections::HashSet;

use super::{checker::Checker, EffectMode, EffectSet, Type, TypeTable};
use crate::{diagnostic::SemanticError, Span};

impl Checker {
    pub(super) fn drop_contract(&self, ty: Type) -> (bool, EffectSet) {
        fn visit(
            checker: &Checker,
            ty: Type,
            seen: &mut HashSet<Type>,
            found: &mut bool,
            effects: &mut EffectSet,
        ) {
            if !seen.insert(ty) {
                return;
            }
            let children: Vec<Type> = match ty {
                Type::Class(id) => {
                    let class = checker
                        .classes
                        .values()
                        .find(|c| c.id == id)
                        .expect("class type");
                    if checker
                        .trait_impls
                        .get("Drop")
                        .is_some_and(|types| types.contains(&ty))
                    {
                        *found = true;
                        if let Some(method) = class.methods.get(super::DROP_METHOD) {
                            for group in method.declared_effects.iter() {
                                for op in checker.effects.all_operations(group) {
                                    effects.insert(op);
                                }
                            }
                        }
                    }
                    class.fields.iter().map(|(_, f, _)| f.ty).collect()
                }
                Type::Struct(id) => checker
                    .structs
                    .values()
                    .find(|s| s.id == id)
                    .unwrap()
                    .fields
                    .iter()
                    .map(|(_, t)| *t)
                    .collect(),
                Type::Enum(id) => checker
                    .enums
                    .values()
                    .find(|e| e.id == id)
                    .unwrap()
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|(_, t)| *t))
                    .collect(),
                Type::Tuple(id) => checker.tuple_types[id].clone(),
                Type::Option(id) => vec![checker.option_types[id]],
                Type::Result(id) => {
                    let (a, b) = checker.result_types[id];
                    vec![a, b]
                }
                Type::List(id) | Type::MutList(id) => vec![checker.list_types[id]],
                Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                    vec![checker.maps[id].key, checker.maps[id].value]
                }
                Type::Cown(id) => vec![checker.cowns[id]],
                _ => vec![],
            };
            for child in children {
                visit(checker, child, seen, found, effects);
            }
        }
        let mut found = false;
        let mut effects = EffectSet::new();
        visit(self, ty, &mut HashSet::new(), &mut found, &mut effects);
        (found, effects)
    }

    pub(super) fn record_drop_effects(
        &mut self,
        ty: Type,
        span: Span,
    ) -> Result<(), SemanticError> {
        let (_, effects) = self.drop_contract(ty);
        for op in effects.iter() {
            let operation = self
                .effects
                .operation_info(op)
                .expect("Drop effect operation");
            if operation.suspends || operation.mode != EffectMode::Normal {
                return Err(SemanticError::FunctionNotSupported {
                    name: "Drop effects must be synchronous and cannot resume or abort".into(),
                    span,
                });
            }
        }
        self.current_effects.extend(&effects);
        Ok(())
    }
}

impl TypeTable {
    pub(crate) fn drop_contract(&self, ty: Type) -> (bool, EffectSet) {
        fn visit(
            table: &TypeTable,
            ty: Type,
            seen: &mut HashSet<Type>,
            found: &mut bool,
            effects: &mut EffectSet,
        ) {
            if !seen.insert(ty) {
                return;
            }
            let children: Vec<Type> = match ty {
                Type::Class(id) => {
                    if let Some(contract) = table.class_drop_effects(id) {
                        *found = true;
                        effects.extend(contract);
                    }
                    table.class_fields(id).iter().map(|(_, t)| *t).collect()
                }
                Type::Struct(id) => table.struct_fields(id).iter().map(|(_, t)| *t).collect(),
                Type::Enum(id) => table
                    .enum_variants(id)
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|(_, t)| *t))
                    .collect(),
                Type::Tuple(id) => table.tuple_elements(id).to_vec(),
                Type::Option(id) => vec![table.option_type(id)],
                Type::Result(id) => {
                    let (a, b) = table.result_types(id);
                    vec![a, b]
                }
                Type::List(id) | Type::MutList(id) => vec![table.list_type(id)],
                Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                    let info = table.map_info(id);
                    vec![info.key, info.value]
                }
                Type::Cown(id) => vec![table.cown_type(id)],
                _ => vec![],
            };
            for child in children {
                visit(table, child, seen, found, effects);
            }
        }
        let mut found = false;
        let mut effects = EffectSet::new();
        visit(self, ty, &mut HashSet::new(), &mut found, &mut effects);
        (found, effects)
    }
}
