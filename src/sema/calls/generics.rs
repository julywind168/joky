//! Generic type substitution and trait checks used by call analysis.

use super::*;

impl Checker {
    pub(in crate::sema) fn check_generic_bound_effects(
        &mut self,
        ty: Type,
        trait_name: &str,
        span: Span,
    ) -> Result<(), SemanticError> {
        if !self.implements_trait(ty, trait_name) {
            return Err(SemanticError::TraitBoundNotSatisfied {
                trait_name: trait_name.into(),
                actual: super::super::type_name(ty),
                span,
            });
        }
        // Structural keys have no effect contract; the builtin List, byte, and
        // map cursors are pure.
        if trait_name == "__MapKey"
            || (trait_name == "Cursor"
                && matches!(
                    ty,
                    Type::List(_)
                        | Type::BytesCursor
                        | Type::MapCursor(_)
                        | Type::MapKeyCursor(_)
                        | Type::MapValueCursor(_)
                        | Type::MutListCursor(_)
                        | Type::MutMapCursor(_)
                        | Type::MutSetCursor(_)
                ))
        {
            return Ok(());
        }
        if !self.traits.contains_key(trait_name) {
            let (module, name) = self
                .dependency_types
                .iter()
                .find_map(|(module, table)| {
                    table.interface.trait_definitions.keys().find_map(|name| {
                        (super::super::trait_key(*module, name) == trait_name)
                            .then(|| (*module, name.clone()))
                    })
                })
                .ok_or_else(|| SemanticError::UnknownTrait {
                    name: trait_name.into(),
                    span,
                })?;
            self.import_trait_definition(module, &name, span)?;
        }
        let mut methods = self.traits[trait_name]
            .methods
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        methods.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (method, contract) in methods {
            if contract.receiver_mode == crate::syntax::ReceiverMode::Static {
                continue;
            }
            let implementation_name =
                super::super::trait_method_name(self.definition_module, trait_name, &method);
            let Some(implementation) = self
                .nominal_methods(ty)
                .and_then(|methods| methods.get(&implementation_name))
                .cloned()
            else {
                if matches!(
                    trait_name,
                    "Show"
                        | "Debug"
                        | "PartialEq"
                        | "Eq"
                        | "PartialOrd"
                        | "Ord"
                        | "Hash"
                        | "FromString"
                ) {
                    // Implicit built-in implementations have no user method effects.
                    continue;
                }
                return Err(SemanticError::InvalidTraitImpl {
                    trait_name: trait_name.into(),
                    type_name: super::super::type_name(ty),
                    span,
                });
            };
            let allowed = self.dynamic_effects(&contract.effect_names, span)?;
            // An imported implementation can refer to a local effect through
            // another registry ID before local identities are qualified for export.
            let operation_identity = |operation: super::super::EffectOperationId| {
                (
                    self.effects
                        .effect(operation.effect)
                        .unwrap()
                        .identity_in_module(self.definition_module),
                    self.effects.operation_info(operation).unwrap().name.clone(),
                )
            };
            let allowed = allowed
                .iter()
                .map(&operation_identity)
                .collect::<std::collections::HashSet<_>>();
            let mut extra = implementation
                .declared_effects
                .iter()
                .flat_map(|group| self.effects.all_operations(group))
                .filter(|operation| !allowed.contains(&operation_identity(*operation)))
                .map(|operation| {
                    format!(
                        "{}.{}",
                        self.effects.effect(operation.effect).unwrap().name,
                        self.effects.operation_info(operation).unwrap().name
                    )
                })
                .collect::<Vec<_>>();
            if !extra.is_empty() {
                extra.sort();
                let actual = match ty {
                    Type::Struct(id) => self
                        .structs
                        .iter()
                        .find(|(_, info)| info.id == id)
                        .map(|(name, _)| name.clone()),
                    Type::Class(id) => self
                        .classes
                        .iter()
                        .find(|(_, info)| info.id == id)
                        .map(|(name, _)| name.clone()),
                    _ => None,
                }
                .unwrap_or_else(|| super::super::type_name(ty));
                return Err(SemanticError::TraitEffectBoundNotSatisfied {
                    trait_name: trait_name.into(),
                    method,
                    actual,
                    effects: extra,
                    span,
                });
            }
        }
        Ok(())
    }

    pub(in crate::sema) fn implements_trait(&self, ty: Type, trait_name: &str) -> bool {
        if trait_name == "Cursor"
            && matches!(
                ty,
                Type::List(_)
                    | Type::BytesCursor
                    | Type::MapCursor(_)
                    | Type::MapKeyCursor(_)
                    | Type::MapValueCursor(_)
                    | Type::MutListCursor(_)
                    | Type::MutMapCursor(_)
                    | Type::MutSetCursor(_)
            )
        {
            return true;
        }
        if trait_name == "__MapKey" {
            return match ty {
                Type::Param(id) => self
                    .current_type_parameter_bounds
                    .get(id)
                    .is_some_and(|bounds| bounds.iter().any(|bound| bound == "__MapKey")),
                _ => self.is_supported_map_key(ty, &mut Vec::new()),
            };
        }
        if trait_name == "FromString" && ty.has_builtin_from_string() {
            return true;
        }
        if trait_name == "Hash" {
            match ty {
                Type::Tuple(id) => {
                    return self.tuple_types[id]
                        .iter()
                        .all(|member| self.implements_trait(*member, "Hash"))
                }
                Type::Enum(id)
                    if self
                        .enums
                        .values()
                        .find(|e| e.id == id)
                        .is_some_and(|e| e.variants.iter().all(|v| v.fields.is_empty())) =>
                {
                    return true
                }
                _ => {}
            }
        }
        // Generic bounds entail built-in supertraits; concrete impls are validated separately.
        if matches!(ty, Type::Param(id) if self.current_type_parameter_bounds.get(id).is_some_and(|bounds| bounds.iter().any(|name| super::super::methods::builtin_bounds(name).contains(&trait_name))))
        {
            return true;
        }
        if matches!(trait_name, "PartialEq" | "Eq" | "PartialOrd" | "Ord") {
            let members = match ty {
                Type::Tuple(id) => Some(self.tuple_types[id].clone()),
                Type::Option(id) => Some(vec![self.option_types[id]]),
                Type::Result(id) => {
                    let (ok, err) = self.result_types[id];
                    Some(vec![ok, err])
                }
                Type::List(id) => Some(vec![self.list_types[id]]),
                _ => None,
            };
            if let Some(members) = members {
                return members
                    .into_iter()
                    .all(|member| self.implements_trait(member, trait_name));
            }
        }
        if matches!(trait_name, "PartialOrd" | "Ord") {
            let builtin = ty.is_numeric()
                || matches!(ty, Type::String | Type::Bytes | Type::Bool | Type::Duration)
                || ty == Type::Enum(self.enums["Ordering"].id);
            if builtin && (trait_name == "PartialOrd" || !ty.is_float()) {
                return true;
            }
        }
        if matches!(trait_name, "PartialEq" | "Eq") {
            let builtin = ty.is_numeric()
                || matches!(
                    ty,
                    Type::String | Type::Bool | Type::Bytes | Type::Duration | Type::Unit
                )
                || matches!(ty, Type::Enum(id) if self.enums.values().find(|e| e.id == id).is_some_and(|e| e.variants.iter().all(|v| v.fields.is_empty())));
            if builtin && (trait_name == "PartialEq" || !ty.is_float()) {
                return true;
            }
            if trait_name == "PartialEq"
                && (matches!(ty, Type::Param(id) if self.current_type_parameter_bounds.get(id).is_some_and(|bounds| bounds.iter().any(|bound| bound == "Eq")))
                    || (self
                        .trait_impls
                        .get("Eq")
                        .is_some_and(|types| types.contains(&ty))
                        && self.is_structural_map_key(ty, &mut Vec::new())))
            {
                return true;
            }
        }
        if trait_name == "Show" && (matches!(ty, Type::String | Type::Bool) || ty.is_numeric()) {
            return true;
        }
        if trait_name == "Debug" {
            return self.supports_debug(ty);
        }
        if matches!(trait_name, "Eq" | "Hash") && super::collections::is_builtin_map_key(ty) {
            return true;
        }
        let identity = |name: &str| {
            self.definition_module.map_or_else(
                || name.to_owned(),
                |module| crate::sema::trait_key(module, name),
            )
        };
        let expected = identity(trait_name);
        if let Type::Associated(id) = ty {
            if self
                .current_associated_bounds
                .iter()
                .any(|(subject, bounds)| {
                    *subject == Type::Associated(id)
                        && bounds.iter().any(|bound| {
                            super::super::methods::builtin_bounds(bound).contains(&trait_name)
                                || identity(bound) == expected
                        })
                })
            {
                return true;
            }
        }
        self.trait_impls
            .iter()
            .any(|(name, types)| identity(name) == expected && types.contains(&ty))
            || matches!(ty, Type::Param(id) if self.current_type_parameter_bounds.get(id).is_some_and(|bounds| bounds.iter().any(|bound| identity(bound) == expected)))
    }

    pub(in crate::sema) fn check_associated_bounds(
        &mut self,
        bounds: &[(Type, Vec<String>)],
        substitutions: &[Option<Type>],
        span: Span,
    ) -> Result<(), SemanticError> {
        for (subject, traits) in bounds {
            let actual = self
                .substitute_type(*subject, substitutions)
                .ok_or_else(|| SemanticError::UnknownFunction {
                    name: "cannot infer associated type constraint".to_owned(),
                    span,
                })?;
            for trait_name in traits {
                self.check_generic_bound_effects(actual, trait_name, span)?;
            }
        }
        Ok(())
    }

    pub(crate) fn implements_show(&self, ty: Type) -> bool {
        if matches!(ty, Type::Dyn(id) if self.dynamic_types[id].includes("Show")) {
            return true;
        }
        self.implements_trait(ty, "Show") || self.show_impls.contains(&ty)
    }

    pub(super) fn implements_debug(&self, ty: Type) -> bool {
        matches!(ty, Type::Dyn(id) if self.dynamic_types[id].includes("Debug"))
            || self.implements_trait(ty, "Debug")
    }

    pub(in crate::sema) fn substitute_type(
        &mut self,
        ty: Type,
        substitutions: &[Option<Type>],
    ) -> Option<Type> {
        match ty {
            Type::Param(id) => substitutions.get(id).copied().flatten(),
            Type::List(id) => {
                let element = self.substitute_type(self.list_types[id], substitutions)?;
                Some(Type::List(intern_list(&mut self.list_types, element)))
            }
            Type::MutList(id) => {
                let element = self.substitute_type(self.list_types[id], substitutions)?;
                Some(Type::MutList(intern_list(&mut self.list_types, element)))
            }
            Type::Map(id) => {
                let info = self.maps[id];
                let key = self.substitute_type(info.key, substitutions)?;
                let value = self.substitute_type(info.value, substitutions)?;
                Some(Type::Map(intern_map(&mut self.maps, key, value)))
            }
            Type::MutMap(id) => {
                let info = self.maps[id];
                let key = self.substitute_type(info.key, substitutions)?;
                let value = self.substitute_type(info.value, substitutions)?;
                Some(Type::MutMap(intern_map(&mut self.maps, key, value)))
            }
            Type::MutSet(id) => {
                let key = self.substitute_type(self.maps[id].key, substitutions)?;
                Some(Type::MutSet(intern_map(&mut self.maps, key, Type::Bool)))
            }
            Type::Option(id) => {
                let value = self.substitute_type(self.option_types[id], substitutions)?;
                Some(Type::Option(super::super::checker::intern_option(
                    &mut self.option_types,
                    value,
                )))
            }
            Type::Result(id) => {
                let (ok, err) = self.result_types[id];
                let ok = self.substitute_type(ok, substitutions)?;
                let err = self.substitute_type(err, substitutions)?;
                Some(Type::Result(super::super::checker::intern_result(
                    &mut self.result_types,
                    ok,
                    err,
                )))
            }
            Type::Tuple(id) => {
                let elements = self.tuple_types[id].clone();
                let elements = elements
                    .into_iter()
                    .map(|element| self.substitute_type(element, substitutions))
                    .collect::<Option<Vec<_>>>()?;
                Some(Type::Tuple(super::super::checker::intern_tuple(
                    &mut self.tuple_types,
                    elements,
                )))
            }
            Type::Associated(id) => {
                let info = self.associated_types[id].clone();
                let receiver = info
                    .parameter
                    .and_then(|parameter| substitutions.get(parameter).copied().flatten())?;
                if let Type::Param(parameter) = receiver {
                    return Some(super::super::checker::intern_associated_type(
                        &mut self.associated_types,
                        Some(parameter),
                        &info.trait_name,
                        &info.name,
                    ));
                }
                self.associated_type_value(receiver, &info.trait_name, &info.name)
            }
            Type::Cown(id) => {
                let inner = self.substitute_type(self.cowns[id], substitutions)?;
                Some(Type::Cown(super::super::checker::intern_cown(
                    &mut self.cowns,
                    inner,
                )))
            }
            Type::Function(id) => {
                let mut info = self.function_types[id].clone();
                info.parameters = info
                    .parameters
                    .iter()
                    .map(|ty| self.substitute_type(*ty, substitutions))
                    .collect::<Option<Vec<_>>>()?;
                info.return_type = self.substitute_type(info.return_type, substitutions)?;
                let id = self
                    .function_types
                    .iter()
                    .position(|candidate| {
                        candidate.parameter_names == info.parameter_names
                            && candidate.parameters == info.parameters
                            && candidate.return_type == info.return_type
                            && candidate.effects == info.effects
                    })
                    .unwrap_or_else(|| {
                        let id = self.function_types.len();
                        self.function_types.push(info);
                        id
                    });
                Some(Type::Function(id))
            }
            concrete => Some(concrete),
        }
    }

    pub(super) fn unify_generic_type(
        &self,
        template: Type,
        actual: Type,
        substitutions: &mut [Option<Type>],
        span: Span,
    ) -> Result<(), SemanticError> {
        let matches = match template {
            Type::Param(id) => match substitutions[id] {
                Some(expected) => expected == actual,
                None => {
                    substitutions[id] = Some(actual);
                    true
                }
            },
            Type::List(template_id) => match actual {
                Type::List(actual_id) => self
                    .unify_generic_type(
                        self.list_types[template_id],
                        self.list_types[actual_id],
                        substitutions,
                        span,
                    )
                    .is_ok(),
                _ => false,
            },
            Type::MutList(template_id) => match actual {
                Type::MutList(actual_id) => self
                    .unify_generic_type(
                        self.list_types[template_id],
                        self.list_types[actual_id],
                        substitutions,
                        span,
                    )
                    .is_ok(),
                _ => false,
            },
            Type::Option(template_id) => match actual {
                Type::Option(actual_id) => self
                    .unify_generic_type(
                        self.option_types[template_id],
                        self.option_types[actual_id],
                        substitutions,
                        span,
                    )
                    .is_ok(),
                _ => false,
            },
            Type::Result(template_id) => match actual {
                Type::Result(actual_id) => {
                    let (template_ok, template_err) = self.result_types[template_id];
                    let (actual_ok, actual_err) = self.result_types[actual_id];
                    self.unify_generic_type(template_ok, actual_ok, substitutions, span)
                        .is_ok()
                        && self
                            .unify_generic_type(template_err, actual_err, substitutions, span)
                            .is_ok()
                }
                _ => false,
            },
            Type::Tuple(template_id) => match actual {
                Type::Tuple(actual_id) => {
                    let template = &self.tuple_types[template_id];
                    let actual = &self.tuple_types[actual_id];
                    template.len() == actual.len()
                        && template.iter().zip(actual).all(|(template, actual)| {
                            self.unify_generic_type(*template, *actual, substitutions, span)
                                .is_ok()
                        })
                }
                _ => false,
            },
            Type::Function(template_id) => match actual {
                Type::Function(actual_id) => {
                    let template = &self.function_types[template_id];
                    let actual = &self.function_types[actual_id];
                    template.parameter_names == actual.parameter_names
                        && template.effects == actual.effects
                        && template.suspends == actual.suspends
                        && template.parameters.len() == actual.parameters.len()
                        && template.parameters.iter().zip(&actual.parameters).all(
                            |(template, actual)| {
                                self.unify_generic_type(*template, *actual, substitutions, span)
                                    .is_ok()
                            },
                        )
                        && self
                            .unify_generic_type(
                                template.return_type,
                                actual.return_type,
                                substitutions,
                                span,
                            )
                            .is_ok()
                }
                _ => false,
            },
            Type::Map(template_id) => match actual {
                Type::Map(actual_id) => {
                    let template = self.maps[template_id];
                    let actual = self.maps[actual_id];
                    self.unify_generic_type(template.key, actual.key, substitutions, span)
                        .is_ok()
                        && self
                            .unify_generic_type(template.value, actual.value, substitutions, span)
                            .is_ok()
                }
                _ => false,
            },
            Type::MutMap(template_id) => match actual {
                Type::MutMap(actual_id) => {
                    let template = self.maps[template_id];
                    let actual = self.maps[actual_id];
                    self.unify_generic_type(template.key, actual.key, substitutions, span)
                        .is_ok()
                        && self
                            .unify_generic_type(template.value, actual.value, substitutions, span)
                            .is_ok()
                }
                _ => false,
            },
            Type::MutSet(template_id) => match actual {
                Type::MutSet(actual_id) => self
                    .unify_generic_type(
                        self.maps[template_id].key,
                        self.maps[actual_id].key,
                        substitutions,
                        span,
                    )
                    .is_ok(),
                _ => false,
            },
            Type::Associated(id) => {
                let info = &self.associated_types[id];
                // A result annotation cannot infer a receiver from its
                // associated type. Defer until arguments determine it; the
                // concrete arguments/result are checked again afterwards.
                info.parameter
                    .and_then(|id| substitutions.get(id).copied().flatten())
                    .and_then(|receiver| {
                        self.associated_type_value(receiver, &info.trait_name, &info.name)
                    })
                    .is_none_or(|expected| expected == actual)
            }
            concrete => concrete == actual,
        };
        if matches {
            Ok(())
        } else {
            Err(SemanticError::TypeMismatch {
                expected: super::super::type_name(template),
                actual: super::super::type_name(actual),
                span,
            })
        }
    }
}
