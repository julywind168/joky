use std::collections::HashSet;

use crate::diagnostic::SemanticError;
use crate::syntax::{Function, TypeAnnotation, TypeExpr};

use crate::sema::checker::{intern_associated_type, intern_tuple, Checker};
use crate::sema::effects::EffectSet;
use crate::sema::symbol_table::{FunctionSignature, FunctionTypeInfo};
use crate::sema::types::{primitive_type, Type};

impl Checker {
    pub(super) fn resolve_parameter_type(
        &mut self,
        parameter: &crate::syntax::Parameter,
    ) -> Result<Type, SemanticError> {
        // Copy/shared arguments preserve their normal value representation;
        // owned arguments use an actual borrow. This also makes &Self and &T
        // consistent with their concrete instantiations.
        self.resolve_type(&parameter.ty)
    }
    pub(super) fn resolve_effect_parameter_type(
        &mut self,
        parameter: &crate::syntax::Parameter,
    ) -> Result<Type, SemanticError> {
        let ty = self.resolve_type(&parameter.ty)?;
        if parameter.borrowed
            && !matches!(
                ty,
                Type::CCallback
                    | Type::Class(_)
                    | Type::Dyn(_)
                    | Type::MutBytes
                    | Type::MutList(_)
                    | Type::MutMap(_)
                    | Type::MutSet(_)
                    | Type::Native(_)
                    | Type::Param(_)
                    | Type::SelfType
            )
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "borrowed parameters require a class or native resource type".to_owned(),
                span: parameter.span,
            });
        }
        Ok(ty)
    }
    pub(super) fn function_signature(
        &mut self,
        function: &Function,
    ) -> Result<FunctionSignature, SemanticError> {
        if function.foreign.is_some()
            && (function.receiver_mode.is_some()
                || !function.type_parameters.is_empty()
                || function.return_type.is_none()
                || function.name == "main"
                || function.parameters.iter().any(|p| p.borrowed))
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "extern C requires an explicit scalar/Unit return type, owned scalar parameters, no receiver or generics, and cannot be main".into(),
                span: function.span,
            });
        }
        let type_parameters = function
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let mut type_parameter_bounds = function
            .type_parameters
            .iter()
            .map(|parameter| {
                parameter
                    .bounds
                    .iter()
                    .map(|bound| {
                        let name = bound.as_name().unwrap_or("");
                        if matches!(name, "Show" | "Debug" | "Eq") || self.traits.contains_key(name)
                        {
                            Ok(name.to_owned())
                        } else {
                            Err(SemanticError::UnknownTrait {
                                name: bound.to_string(),
                                span: bound.span,
                            })
                        }
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()
            })
            .collect::<Result<Vec<_>, SemanticError>>()?;
        let mut unique = HashSet::new();
        if let Some(duplicate) = function
            .type_parameters
            .iter()
            .find(|parameter| !unique.insert(parameter.name.as_str()))
        {
            return Err(SemanticError::DuplicateTypeParameter {
                name: duplicate.name.clone(),
                span: duplicate.span,
            });
        }
        self.apply_parameter_where_bounds(function, &type_parameters, &mut type_parameter_bounds)?;
        let previous =
            std::mem::replace(&mut self.current_type_parameters, type_parameters.clone());
        let previous_bounds = std::mem::replace(
            &mut self.current_type_parameter_bounds,
            type_parameter_bounds.clone(),
        );
        let parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                Ok((
                    parameter.name.clone(),
                    self.resolve_parameter_type(parameter)?,
                ))
            })
            .collect::<Result<Vec<_>, SemanticError>>()?;
        let return_type = function
            .return_type
            .as_ref()
            .map(|annotation| self.resolve_type(annotation))
            .transpose()?
            .unwrap_or(Type::Unit);
        // Resolving container types can infer bounds for their type parameters
        // (for example, `Map(K, V)` contributes `Hash + Eq` to `K`). Keep the
        // inferred constraints on the published function signature.
        let inferred_type_parameter_bounds = self.current_type_parameter_bounds.clone();
        let associated_bounds = self.collect_associated_where_bounds(function, &type_parameters)?;
        self.current_type_parameters = previous;
        self.current_type_parameter_bounds = previous_bounds;
        let effect_names = function
            .effect_names
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let declared_effects =
            self.effects
                .group_set(&effect_names)
                .ok_or_else(|| SemanticError::UnknownType {
                    name: effect_names
                        .iter()
                        .find(|name| self.effects.by_name(name).is_none())
                        .cloned()
                        .unwrap_or_else(|| "<unknown effect>".to_owned()),
                    span: function
                        .effect_names
                        .iter()
                        .find(|annotation| self.effects.by_name(&annotation.to_string()).is_none())
                        .map(|annotation| annotation.span)
                        .unwrap_or(function.span),
                })?;
        let mut used_effects = EffectSet::new();
        if function.foreign.is_some() {
            // Cross-module imports cannot re-resolve interned callback types
            // yet; keep callbacks single-module.
            if function.visibility == crate::syntax::Visibility::Public
                && parameters
                    .iter()
                    .any(|(_, ty)| matches!(ty, Type::Function(_)))
            {
                return Err(SemanticError::FunctionNotSupported {
                    name: "cross-module native callbacks are not supported yet; declare the extern function private in the module that calls it".into(),
                    span: function.span,
                });
            }
            for (_, ty) in &parameters {
                if let Type::Function(id) = ty {
                    self.validate_callback_signature(*id, function.span)?;
                    continue;
                }
                if !ty.is_c_abi_compatible() {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "extern C supports fixed-width integers, floats, pointers, CStr, and synchronous callbacks; Unit is allowed only as the return type".into(),
                        span: function.span,
                    });
                }
            }
            if !(return_type.is_c_abi_compatible() || return_type == Type::Unit) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "extern C supports fixed-width integers, floats, pointers, CStr, and synchronous callbacks; Unit is allowed only as the return type".into(),
                    span: function.span,
                });
            }
            for group in declared_effects.iter() {
                for operation in self.effects.all_operations(group) {
                    let info = self.effects.operation_info(operation).unwrap();
                    if info.suspends || info.mode != super::super::effects::EffectMode::Normal {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "extern C is synchronous; suspending, resumable and aborting effects require a runtime bridge".into(),
                            span: function.span,
                        });
                    }
                    used_effects.insert(operation);
                }
            }
        }
        Ok(FunctionSignature {
            receiver_mode: function
                .receiver_mode
                .unwrap_or(crate::syntax::ReceiverMode::Borrowed),
            parameter_borrows: function
                .parameters
                .iter()
                .map(|parameter| parameter.borrowed)
                .collect(),
            type_parameters,
            type_parameter_bounds: inferred_type_parameter_bounds,
            associated_bounds,
            parameters,
            return_type,
            declared_effects,
            used_effects,
        })
    }

    fn resolve_where_bounds(
        &self,
        predicate: &crate::syntax::WherePredicate,
    ) -> Result<Vec<String>, SemanticError> {
        predicate
            .bounds
            .iter()
            .map(|bound| {
                let name = bound.as_name().unwrap_or("");
                if self.traits.contains_key(name) {
                    Ok(name.to_owned())
                } else {
                    Err(SemanticError::UnknownTrait {
                        name: bound.to_string(),
                        span: bound.span,
                    })
                }
            })
            .collect()
    }

    fn apply_parameter_where_bounds(
        &self,
        function: &Function,
        type_parameters: &[String],
        type_parameter_bounds: &mut [Vec<String>],
    ) -> Result<(), SemanticError> {
        for predicate in &function.where_predicates {
            if predicate.associated.is_some() {
                continue;
            }
            let index = type_parameters
                .iter()
                .position(|name| name == &predicate.parameter)
                .ok_or_else(|| SemanticError::UnknownType {
                    name: predicate.parameter.clone(),
                    span: predicate.span,
                })?;
            type_parameter_bounds[index].extend(self.resolve_where_bounds(predicate)?);
        }
        Ok(())
    }

    fn collect_associated_where_bounds(
        &mut self,
        function: &Function,
        type_parameters: &[String],
    ) -> Result<Vec<(Type, Vec<String>)>, SemanticError> {
        let mut associated_bounds = Vec::new();
        for predicate in &function.where_predicates {
            let Some(associated) = &predicate.associated else {
                continue;
            };
            let index = type_parameters
                .iter()
                .position(|name| name == &predicate.parameter)
                .ok_or_else(|| SemanticError::UnknownType {
                    name: predicate.parameter.clone(),
                    span: predicate.span,
                })?;
            let subject =
                self.resolve_type_member(Type::Param(index), associated, predicate.span)?;
            associated_bounds.push((subject, self.resolve_where_bounds(predicate)?));
        }
        Ok(associated_bounds)
    }

    /// A callback type in an extern signature must itself be C-flat and
    /// synchronous: C invokes it with the host ABI, so Joky parameters are
    /// restricted to scalars and pointers and the body must complete without
    /// suspending or touching effects.
    fn validate_callback_signature(
        &self,
        id: usize,
        span: crate::Span,
    ) -> Result<(), SemanticError> {
        let info = self
            .function_types
            .get(id)
            .ok_or(SemanticError::NotCallable { span })?;
        if info
            .parameters
            .iter()
            .any(|parameter| !parameter.is_c_abi_compatible())
            || !(info.return_type.is_c_abi_compatible() || info.return_type == Type::Unit)
            || !info.effects.is_empty()
            || info.suspends
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "extern C callbacks must be synchronous with fixed-width integer, float, or pointer parameters and no effects".into(),
                span,
            });
        }
        Ok(())
    }

    pub(in crate::sema) fn resolve_type(
        &mut self,
        ty_ann: &TypeAnnotation,
    ) -> Result<Type, SemanticError> {
        let ty = match &ty_ann.kind {
            TypeExpr::TraitComposition(_) => {
                return Err(super::super::dynamic::invalid(
                    "trait composition is only allowed in Dyn",
                    ty_ann.span,
                ));
            }
            TypeExpr::Name(name) => self.resolve_type_atom(name, ty_ann.span)?,
            TypeExpr::Const(value) => {
                return Err(SemanticError::UnknownType {
                    name: value.to_string(),
                    span: ty_ann.span,
                })
            }
            TypeExpr::Member { base, name } => {
                if let Some(module) = base
                    .as_name()
                    .and_then(|alias| self.import_aliases.get(alias))
                    .copied()
                {
                    let ty =
                        self.resolve_abi_type(module, &TypeAnnotation::named(name, ty_ann.span))?;
                    self.annotation_types.insert(ty_ann.span, ty);
                    return Ok(ty);
                }
                let base = self.resolve_type(base)?;
                self.resolve_type_member(base, name, ty_ann.span)?
            }
            TypeExpr::Apply { callee, arguments } => {
                if callee.is_name("Dyn") {
                    let first = arguments
                        .first()
                        .filter(|arg| arg.label.is_none())
                        .ok_or_else(|| {
                            super::super::dynamic::invalid("Dyn requires a trait name", ty_ann.span)
                        })?;
                    let names = super::super::dynamic::trait_names_from_annotation(&first.value)?;
                    let mut bindings = Vec::new();
                    for argument in &arguments[1..] {
                        bindings
                            .push((argument.label.clone(), self.resolve_type(&argument.value)?));
                    }
                    let ty = self.resolve_dynamic_type(&names, bindings, ty_ann.span)?;
                    self.annotation_types.insert(ty_ann.span, ty);
                    return Ok(ty);
                }
                if let Some(name) = callee.as_name() {
                    if matches!(name, "CPtr" | "CMutPtr" | "CArray") {
                        let arity = if name == "CArray" { 2 } else { 1 };
                        if arguments.len() != arity
                            || arguments.iter().any(|arg| arg.label.is_some())
                        {
                            return Err(SemanticError::FunctionNotSupported {
                                name: format!("{name} requires {arity} positional type arguments"),
                                span: ty_ann.span,
                            });
                        }
                    }
                    if matches!(name, "CPtr" | "CMutPtr") && arguments.len() == 1 {
                        let pointee = self.resolve_type(&arguments[0].value)?;
                        let ty = self.apply_type_value(
                            name,
                            &[(None, pointee, ty_ann.span)],
                            ty_ann.span,
                            &mut Vec::new(),
                        )?;
                        self.annotation_types.insert(ty_ann.span, ty);
                        return Ok(ty);
                    }
                    if name == "CArray" && arguments.len() == 2 {
                        let element = self.resolve_type(&arguments[0].value)?;
                        let TypeExpr::Const(count) = arguments[1].value.kind else {
                            return Err(SemanticError::FunctionNotSupported {
                                name: "CArray length must be an integer constant".into(),
                                span: arguments[1].value.span,
                            });
                        };
                        if count == 0 {
                            return Err(SemanticError::FunctionNotSupported {
                                name: "CArray length must be greater than zero".into(),
                                span: arguments[1].value.span,
                            });
                        }
                        let id = self
                            .c_arrays
                            .iter()
                            .position(|entry| *entry == (element, count))
                            .unwrap_or_else(|| {
                                let id = self.c_arrays.len();
                                self.c_arrays.push((element, count));
                                id
                            });
                        let ty = Type::CArray(id);
                        self.annotation_types.insert(ty_ann.span, ty);
                        return Ok(ty);
                    }
                }
                if let TypeExpr::Member { base, name } = &callee.kind {
                    if let Some(module) = base
                        .as_name()
                        .and_then(|alias| self.import_aliases.get(alias))
                        .copied()
                    {
                        let arguments = arguments
                            .iter()
                            .map(|argument| {
                                Ok((
                                    argument.label.clone(),
                                    self.resolve_type(&argument.value)?,
                                    argument.value.span,
                                ))
                            })
                            .collect::<Result<Vec<_>, SemanticError>>()?;
                        let ty =
                            self.import_type_function(module, name, &arguments, ty_ann.span)?;
                        self.annotation_types.insert(ty_ann.span, ty);
                        return Ok(ty);
                    }
                }
                let name = callee
                    .as_name()
                    .filter(|name| self.lookup(name).is_none())
                    .ok_or_else(|| SemanticError::UnknownType {
                        name: callee.to_string(),
                        span: callee.span,
                    })?;
                let values = arguments
                    .iter()
                    .map(|argument| {
                        Ok((
                            argument.label.clone(),
                            self.resolve_type(&argument.value)?,
                            argument.value.span,
                        ))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                self.apply_type_value(name, &values, ty_ann.span, &mut Vec::new())?
            }
            TypeExpr::Tuple(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| self.resolve_type(element))
                    .collect::<Result<Vec<_>, _>>()?;
                if elements.is_empty() {
                    Type::Unit
                } else {
                    Type::Tuple(intern_tuple(&mut self.tuple_types, elements))
                }
            }
            TypeExpr::Function { parameters, result } => {
                let parameters = parameters
                    .iter()
                    .map(|parameter| {
                        Ok((
                            parameter.label.clone().unwrap_or_default(),
                            self.resolve_type(&parameter.value)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                let (parameter_names, parameters): (Vec<_>, Vec<_>) =
                    parameters.into_iter().unzip();
                let return_type = self.resolve_type(result)?;
                let id = self
                    .function_types
                    .iter()
                    .position(|info| {
                        info.parameter_names == parameter_names
                            && info.parameters == parameters
                            && info.return_type == return_type
                    })
                    .unwrap_or_else(|| {
                        let id = self.function_types.len();
                        self.function_types.push(FunctionTypeInfo {
                            parameter_names,
                            parameters,
                            return_type,
                            effects: EffectSet::new(),
                            suspends: false,
                        });
                        id
                    });
                Type::Function(id)
            }
        };
        self.check_list_types(ty, ty_ann.span)?;
        self.annotation_types.insert(ty_ann.span, ty);
        Ok(ty)
    }

    pub(in crate::sema) fn resolve_type_atom(
        &mut self,
        name: &str,
        span: crate::Span,
    ) -> Result<Type, SemanticError> {
        if let Some(ty) = self.type_bindings.get(name) {
            return Ok(*ty);
        }
        if let Some(binding) = self.lookup(name) {
            return binding
                .type_value
                .ok_or_else(|| SemanticError::UnknownType {
                    name: name.to_owned(),
                    span,
                });
        }
        if let Some(id) = self.current_type_parameters.iter().position(|p| p == name) {
            return Ok(Type::Param(id));
        }
        if name == "Self" {
            return Ok(Type::SelfType);
        }
        if let Some(trait_name) = &self.current_trait_context {
            if self
                .traits
                .get(trait_name)
                .is_some_and(|info| info.associated_types.contains(name))
            {
                return Ok(intern_associated_type(
                    &mut self.associated_types,
                    None,
                    trait_name,
                    name,
                ));
            }
        }
        self.structs
            .get(name)
            .map(|info| Type::Struct(info.id))
            .or_else(|| self.classes.get(name).map(|info| Type::Class(info.id)))
            .or_else(|| self.enums.get(name).map(|info| Type::Enum(info.id)))
            .or_else(|| primitive_type(name))
            .ok_or_else(|| SemanticError::UnknownType {
                name: name.to_owned(),
                span,
            })
    }

    pub(in crate::sema) fn resolve_type_member(
        &mut self,
        base: Type,
        name: &str,
        span: crate::Span,
    ) -> Result<Type, SemanticError> {
        let (parameter, trait_name) = match base {
            Type::SelfType => (None, self.current_trait_context.clone()),
            Type::Param(id) => (
                Some(id),
                self.current_type_parameter_bounds
                    .get(id)
                    .into_iter()
                    .flatten()
                    .find(|trait_name| {
                        self.traits
                            .get(*trait_name)
                            .is_some_and(|info| info.associated_types.contains(name))
                    })
                    .cloned(),
            ),
            _ => (None, None),
        };
        if let Some(trait_name) = trait_name.filter(|trait_name| {
            self.traits
                .get(trait_name)
                .is_some_and(|info| info.associated_types.contains(name))
        }) {
            return Ok(intern_associated_type(
                &mut self.associated_types,
                parameter,
                &trait_name,
                name,
            ));
        }
        Err(SemanticError::UnknownType {
            name: name.to_owned(),
            span,
        })
    }
}
