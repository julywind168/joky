use std::collections::{HashMap, HashSet};

use crate::diagnostic::SemanticError;
use crate::syntax::{
    Class, Constant, Effect, EffectMode as SyntaxEffectMode, Enum, Expr, ExprKind, Function, Impl,
    Struct, Trait,
};

use super::checker::{Checker, ConstantState};
use super::effects::EffectMode;
use super::expectation::TypeExpectation;
use super::symbol_table::{
    ClassFieldInfo, ClassInfo, EnumInfo, EnumVariantInfo, FunctionSignature, StructInfo, TraitInfo,
    TraitMethodInfo,
};
use super::types::Type;
use super::validation::type_mismatch;

mod layouts;
mod signatures;

impl Checker {
    pub(super) fn register_constant(&mut self, constant: &Constant) -> Result<(), SemanticError> {
        if self.constants.contains_key(&constant.name)
            || self.type_functions.contains_key(&constant.name)
        {
            return Err(SemanticError::DuplicateConstantDefinition {
                name: constant.name.clone(),
                span: constant.span,
            });
        }
        let ty = constant
            .annotation
            .as_ref()
            .map(|annotation| self.resolve_type(annotation))
            .transpose()?;
        self.constants.insert(
            constant.name.clone(),
            super::checker::ConstantInfo {
                ty,
                span: constant.span,
            },
        );
        self.constant_definitions
            .insert(constant.name.clone(), constant.clone());
        self.constant_states
            .insert(constant.name.clone(), ConstantState::Unresolved);
        Ok(())
    }

    pub(super) fn check_constant(&mut self, name: &str) -> Result<Type, SemanticError> {
        match self.constant_states[name] {
            ConstantState::Resolved => {
                return Ok(self.constants[name]
                    .ty
                    .expect("resolved constant has a type"))
            }
            ConstantState::Resolving => {
                return Err(SemanticError::ConstantDependencyCycle {
                    name: name.to_owned(),
                    span: self.constants[name].span,
                })
            }
            ConstantState::Unresolved => {}
        }
        let constant = self.constant_definitions[name].clone();
        self.validate_constant_expression(&constant.value)?;
        self.constant_states
            .insert(constant.name.clone(), ConstantState::Resolving);
        let expected = self.constants[&constant.name].ty;
        let previous = self.current_constant.replace(constant.name.clone());
        let previous_prefix = std::mem::replace(
            &mut self.current_module_prefix,
            module_prefix(&constant.name),
        );
        let result = self.check_expression(&constant.value, TypeExpectation::from_option(expected));
        self.current_constant = previous;
        self.current_module_prefix = previous_prefix;
        let ty = result?;
        self.constants
            .get_mut(&constant.name)
            .expect("registered constant")
            .ty = Some(ty);
        self.constant_states
            .insert(constant.name.clone(), ConstantState::Resolved);
        Ok(ty)
    }

    pub(super) fn check_constants(&mut self) -> Result<(), SemanticError> {
        let names = self
            .constant_definitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for name in names {
            self.check_constant(&name)?;
        }
        Ok(())
    }

    fn validate_constant_expression(&self, expression: &Expr) -> Result<(), SemanticError> {
        match &expression.kind {
            ExprKind::Integer(_)
            | ExprKind::TypedInteger(_, _)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::String(_)
            | ExprKind::IncludeBytes { .. }
            | ExprKind::Boolean(_)
            | ExprKind::Name(_) => Ok(()),
            ExprKind::Field {
                value,
                access: crate::syntax::FieldAccess::Name(_),
            } if matches!(&value.kind, ExprKind::Name(alias) if self.import_aliases.contains_key(alias)) => {
                Ok(())
            }
            ExprKind::Unary { expression, .. } => self.validate_constant_expression(expression),
            ExprKind::Binary { left, right, .. } => {
                self.validate_constant_expression(left)?;
                self.validate_constant_expression(right)
            }
            ExprKind::Tuple(elements) => {
                for element in elements {
                    self.validate_constant_expression(element)?;
                }
                Ok(())
            }
            _ => Err(SemanticError::InvalidConstantExpression {
                span: expression.span,
            }),
        }
    }

    pub(super) fn declare_effect(&mut self, definition: &Effect) -> Result<(), SemanticError> {
        if self.effects.by_name(&definition.name).is_some() {
            return Err(SemanticError::DuplicateEffectDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        self.effects.define(definition.name.clone());
        Ok(())
    }

    pub(super) fn define_effect(&mut self, definition: &Effect) -> Result<(), SemanticError> {
        let effect_id = self.effects.by_name(&definition.name).ok_or_else(|| {
            SemanticError::InvalidEffectDefinition {
                name: definition.name.clone(),
                span: definition.span,
            }
        })?;
        let mut names = HashSet::new();
        for operation in &definition.operations {
            if !names.insert(operation.name.as_str()) {
                return Err(SemanticError::InvalidEffectDefinition {
                    name: operation.name.clone(),
                    span: operation.span,
                });
            }
            let parameters = operation
                .parameters
                .iter()
                .map(|parameter| self.resolve_effect_parameter_type(parameter))
                .collect::<Result<Vec<_>, _>>()?;
            let return_type = self.resolve_type(&operation.return_type)?;
            let operation_id = self.effects.operation(
                effect_id,
                operation.name.clone(),
                parameters,
                return_type,
                match operation.mode {
                    SyntaxEffectMode::Normal => EffectMode::Normal,
                    SyntaxEffectMode::Resumable => EffectMode::Resumable,
                    SyntaxEffectMode::Aborts => EffectMode::Aborts,
                },
                operation.suspends,
            );
            self.effects.set_parameter_borrows(
                operation_id,
                operation
                    .parameters
                    .iter()
                    .map(|parameter| parameter.borrowed)
                    .collect(),
            );
        }
        Ok(())
    }

    pub(super) fn declare_trait(&mut self, definition: &Trait) -> Result<(), SemanticError> {
        let builtin_declaration =
            matches!(definition.name.as_str(), "Show" | "Debug" | "FromString")
                && self.traits.contains_key(&definition.name);
        if self.traits.contains_key(&definition.name) && !builtin_declaration {
            return Err(SemanticError::InvalidTraitDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        if definition
            .methods
            .iter()
            .any(|method| method.name.is_empty())
            || definition
                .methods
                .iter()
                .map(|method| method.name.as_str())
                .collect::<HashSet<_>>()
                .len()
                != definition.methods.len()
        {
            return Err(SemanticError::InvalidTraitDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        let associated_types = definition
            .associated_types
            .iter()
            .map(|associated_type| associated_type.name.clone())
            .collect::<HashSet<_>>();
        if (associated_types.is_empty() && definition.methods.is_empty())
            || associated_types.len() != definition.associated_types.len()
            || associated_types
                .iter()
                .any(|name| definition.methods.iter().any(|method| method.name == *name))
        {
            return Err(SemanticError::InvalidTraitDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        if !builtin_declaration {
            self.traits.insert(
                definition.name.clone(),
                TraitInfo {
                    associated_types: associated_types.clone(),
                    methods: HashMap::new(),
                },
            );
        }
        let previous_context = self.current_trait_context.replace(definition.name.clone());
        let methods = definition
            .methods
            .iter()
            .map(|method| {
                let parameters = method
                    .parameters
                    .iter()
                    .map(|parameter| {
                        Ok((
                            parameter.name.clone(),
                            self.resolve_parameter_type(parameter)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SemanticError>>()?;
                let return_type = self.resolve_type(&method.return_type)?;
                Ok((
                    method.name.clone(),
                    TraitMethodInfo {
                        effect_names: method
                            .effect_names
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                        receiver_mode: method.receiver_mode,
                        parameter_borrows: method
                            .parameters
                            .iter()
                            .map(|parameter| parameter.borrowed)
                            .collect(),
                        parameters,
                        return_type,
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, SemanticError>>()?;
        self.current_trait_context = previous_context;
        if matches!(definition.name.as_str(), "Show" | "Debug")
            && (!associated_types.is_empty()
                || methods.len() != 1
                || methods
                    .get(if definition.name == "Show" {
                        "show"
                    } else {
                        "debug"
                    })
                    .is_none_or(|method| {
                        !method.parameters.is_empty()
                            || (definition.name == "Debug" && !method.effect_names.is_empty())
                            || method.return_type != Type::String
                            || method.receiver_mode != crate::syntax::ReceiverMode::Borrowed
                    }))
        {
            return Err(SemanticError::InvalidTraitDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        if definition.name == "FromString"
            && (!associated_types.is_empty()
                || methods.len() != 1
                || methods.get("from_string").is_none_or(|method| {
                    method.receiver_mode != crate::syntax::ReceiverMode::Static
                        || method.parameters != vec![("value".into(), Type::String)]
                        || method.parameter_borrows != vec![true]
                        || method.return_type
                            != self.traits["FromString"].methods["from_string"].return_type
                        || !method.effect_names.is_empty()
                }))
        {
            return Err(SemanticError::InvalidTraitDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        if !builtin_declaration {
            self.traits.insert(
                definition.name.clone(),
                TraitInfo {
                    associated_types,
                    methods,
                },
            );
        }
        Ok(())
    }

    pub(super) fn declare_enum(&mut self, definition: &Enum) -> Result<(), SemanticError> {
        if self.enums.contains_key(&definition.name) {
            return Err(SemanticError::DuplicateEnumDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        let id = self.enums.len();
        self.enums.insert(
            definition.name.clone(),
            EnumInfo {
                id,
                variants: Vec::new(),
            },
        );
        Ok(())
    }

    pub(super) fn define_enum(&mut self, definition: &Enum) -> Result<(), SemanticError> {
        if definition.variants.is_empty() {
            return Err(SemanticError::EmptyEnumDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        let mut variants: Vec<EnumVariantInfo> = Vec::new();
        for variant in &definition.variants {
            if variants
                .iter()
                .any(|candidate| candidate.name == variant.name)
            {
                return Err(SemanticError::DuplicateEnumVariant {
                    name: variant.name.clone(),
                    span: variant.span,
                });
            }
            let mut fields = Vec::new();
            for field in &variant.fields {
                if field.borrowed {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "borrowed values cannot be stored in enum fields".to_owned(),
                        span: field.span,
                    });
                }
                if fields.iter().any(|(name, _)| name == &field.name) {
                    return Err(SemanticError::DuplicateEnumField {
                        name: field.name.clone(),
                        span: field.span,
                    });
                }
                fields.push((field.name.clone(), self.resolve_type(&field.ty)?));
            }
            variants.push(EnumVariantInfo {
                name: variant.name.clone(),
                fields,
            });
        }
        self.enums
            .get_mut(&definition.name)
            .expect("declared enum")
            .variants = variants;
        Ok(())
    }

    pub(super) fn declare_struct(&mut self, definition: &Struct) -> Result<(), SemanticError> {
        if self.structs.contains_key(&definition.name)
            || self.classes.contains_key(&definition.name)
            || self.enums.contains_key(&definition.name)
        {
            return Err(SemanticError::DuplicateStructDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        let id = self.structs.len();
        self.structs.insert(
            definition.name.clone(),
            StructInfo {
                id,
                repr_c: definition.repr_c,
                fields: Vec::new(),
                defaults: Vec::new(),
                methods: HashMap::new(),
            },
        );
        Ok(())
    }

    pub(super) fn define_struct(&mut self, definition: &Struct) -> Result<(), SemanticError> {
        let mut fields = Vec::new();
        let mut defaults = Vec::new();
        for field in &definition.fields {
            if fields.iter().any(|(name, _)| name == &field.name) {
                return Err(SemanticError::DuplicateStructField {
                    name: field.name.clone(),
                    span: field.span,
                });
            }
            fields.push((field.name.clone(), self.resolve_type(&field.ty)?));
            defaults.push(field.default.clone());
        }
        let mut methods = HashMap::new();
        for method in &definition.methods {
            if !method.type_parameters.is_empty() {
                return Err(SemanticError::GenericParametersNotSupported {
                    target: format!("struct method '{}.{}'", definition.name, method.name),
                    span: method.type_parameters[0].span,
                });
            }
            if methods.contains_key(&method.name) {
                return Err(SemanticError::DuplicateFunctionDefinition {
                    name: method.name.clone(),
                    span: method.span,
                });
            }
            methods.insert(method.name.clone(), self.function_signature(method)?);
        }
        let structure = self
            .structs
            .get_mut(&definition.name)
            .expect("declared struct");
        structure.fields = fields;
        structure.defaults = defaults;
        structure.methods = methods;
        Ok(())
    }

    pub(super) fn declare_class(&mut self, definition: &Class) -> Result<(), SemanticError> {
        if self.classes.contains_key(&definition.name)
            || self.structs.contains_key(&definition.name)
            || self.enums.contains_key(&definition.name)
        {
            return Err(SemanticError::DuplicateClassDefinition {
                name: definition.name.clone(),
                span: definition.span,
            });
        }
        let id = self.classes.len();
        self.classes.insert(
            definition.name.clone(),
            ClassInfo {
                id,
                fields: Vec::new(),
                methods: HashMap::new(),
            },
        );
        Ok(())
    }

    pub(super) fn define_class(&mut self, definition: &Class) -> Result<(), SemanticError> {
        let mut fields: Vec<(String, ClassFieldInfo, Option<Expr>)> = Vec::new();
        for field in &definition.fields {
            if fields.iter().any(|(name, _, _)| name == &field.name) {
                return Err(SemanticError::DuplicateClassField {
                    name: field.name.clone(),
                    span: field.span,
                });
            }
            fields.push((
                field.name.clone(),
                ClassFieldInfo {
                    ty: self.resolve_type(&field.ty)?,
                    mutable: field.mutable,
                },
                field.default.clone(),
            ));
        }
        let mut methods = HashMap::new();
        for method in &definition.methods {
            if !method.type_parameters.is_empty() {
                return Err(SemanticError::GenericParametersNotSupported {
                    target: format!("class method '{}.{}'", definition.name, method.name),
                    span: method.type_parameters[0].span,
                });
            }
            if methods.contains_key(&method.name) {
                return Err(SemanticError::DuplicateFunctionDefinition {
                    name: method.name.clone(),
                    span: method.span,
                });
            }
            methods.insert(method.name.clone(), self.function_signature(method)?);
        }
        let class = self
            .classes
            .get_mut(&definition.name)
            .expect("declared class");
        class.fields = fields;
        class.methods = methods;
        Ok(())
    }

    pub(super) fn define_impl(&mut self, implementation: &Impl) -> Result<(), SemanticError> {
        if implementation.trait_name.contains('.') && !implementation.trait_name.starts_with('@') {
            let mut implementation = implementation.clone();
            implementation.trait_name =
                self.import_dynamic_trait(&implementation.trait_name, implementation.span)?;
            return self.define_impl(&implementation);
        }
        let trait_info = self
            .traits
            .get(&implementation.trait_name)
            .ok_or_else(|| SemanticError::UnknownTrait {
                name: implementation.trait_name.clone(),
                span: implementation.span,
            })?
            .clone();
        if implementation.associated_types.len() != trait_info.associated_types.len()
            || implementation
                .associated_types
                .iter()
                .any(|associated_type| !trait_info.associated_types.contains(&associated_type.name))
        {
            return Err(SemanticError::InvalidTraitImpl {
                trait_name: implementation.trait_name.clone(),
                type_name: implementation.type_name.clone(),
                span: implementation.span,
            });
        }
        let default_hash = implementation.trait_name == "Hash" && implementation.methods.is_empty();
        if !default_hash
            && (implementation.methods.len() != trait_info.methods.len()
                || implementation.methods.iter().any(|method| {
                    !method.type_parameters.is_empty()
                        || !trait_info.methods.contains_key(&method.name)
                })
                || trait_info.methods.keys().any(|name| {
                    !implementation
                        .methods
                        .iter()
                        .any(|method| &method.name == name)
                }))
        {
            return Err(SemanticError::InvalidTraitImpl {
                trait_name: implementation.trait_name.clone(),
                type_name: implementation.type_name.clone(),
                span: implementation.span,
            });
        }
        let receiver = self
            .structs
            .get(&implementation.type_name)
            .map(|info| Type::Struct(info.id))
            .or_else(|| {
                self.classes
                    .get(&implementation.type_name)
                    .map(|info| Type::Class(info.id))
            })
            .ok_or_else(|| SemanticError::UnknownType {
                name: implementation.type_name.clone(),
                span: implementation.span,
            })?;
        if implementation.trait_name == "Drop"
            && (!matches!(receiver, Type::Class(_))
                || implementation.methods[0].receiver_mode
                    != Some(crate::syntax::ReceiverMode::Borrowed))
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "Drop requires a class and fn drop(&self) -> Unit".into(),
                span: implementation.span,
            });
        }
        let previous_context = self
            .current_trait_context
            .replace(implementation.trait_name.clone());
        let associated_values = implementation
            .associated_types
            .iter()
            .map(|associated_type| {
                Ok((
                    associated_type.name.clone(),
                    self.resolve_type(&associated_type.ty)?,
                ))
            })
            .collect::<Result<HashMap<_, _>, SemanticError>>()?;
        self.current_trait_context = previous_context;
        self.trait_associated_impls.insert(
            (implementation.trait_name.clone(), receiver),
            associated_values,
        );
        let previous_context = self
            .current_trait_context
            .replace(implementation.trait_name.clone());
        let mut signatures = Vec::with_capacity(implementation.methods.len());
        for method in &implementation.methods {
            let mut raw_signature = self.function_signature(method)?;
            if trait_info.methods[&method.name].receiver_mode == crate::syntax::ReceiverMode::Static
                && method.receiver_mode.is_none()
            {
                raw_signature.receiver_mode = crate::syntax::ReceiverMode::Static;
            }
            let signature = FunctionSignature {
                receiver_mode: raw_signature.receiver_mode,
                parameter_borrows: raw_signature.parameter_borrows,
                type_parameters: raw_signature.type_parameters,
                type_parameter_bounds: raw_signature.type_parameter_bounds,
                associated_bounds: raw_signature.associated_bounds,
                parameters: raw_signature
                    .parameters
                    .into_iter()
                    .map(|(name, ty)| (name, self.replace_self_type(ty, receiver)))
                    .collect(),
                return_type: self.replace_self_type(raw_signature.return_type, receiver),
                declared_effects: raw_signature.declared_effects,
                used_effects: raw_signature.used_effects,
            };
            let expected = &trait_info.methods[&method.name];
            let invalid_static_effects = expected.receiver_mode
                == crate::syntax::ReceiverMode::Static
                && self
                    .effects
                    .group_set(&expected.effect_names)
                    .is_none_or(|allowed| {
                        signature
                            .declared_effects
                            .iter()
                            .any(|group| !allowed.contains(group))
                    });
            let expected_parameters = expected
                .parameters
                .iter()
                .map(|(name, ty)| (name.as_str(), self.replace_self_type(*ty, receiver)))
                .collect::<Vec<_>>();
            if signature.parameters.len() != expected_parameters.len()
                || invalid_static_effects
                || (matches!(
                    implementation.trait_name.as_str(),
                    "Debug" | "PartialEq" | "PartialOrd" | "Ord" | "FromString" | "Hash"
                ) && !signature.declared_effects.is_empty())
                || signature.receiver_mode != expected.receiver_mode
                || signature.parameter_borrows != expected.parameter_borrows
                || signature.parameters.iter().zip(expected_parameters).any(
                    |((actual_name, actual), (expected_name, expected))| {
                        actual_name != expected_name || *actual != expected
                    },
                )
                || self.replace_self_type(expected.return_type, receiver) != signature.return_type
            {
                return Err(SemanticError::InvalidTraitImpl {
                    trait_name: implementation.trait_name.clone(),
                    type_name: implementation.type_name.clone(),
                    span: implementation.span,
                });
            }
            signatures.push((
                super::trait_method_name(
                    self.definition_module,
                    &implementation.trait_name,
                    &method.name,
                ),
                signature,
            ));
        }
        self.current_trait_context = previous_context;
        let duplicate = signatures.iter().any(|(method_name, _)| match receiver {
            Type::Struct(id) => self
                .structs
                .values()
                .find(|info| info.id == id)
                .is_some_and(|info| info.methods.contains_key(method_name)),
            Type::Class(id) => self
                .classes
                .values()
                .find(|info| info.id == id)
                .is_some_and(|info| info.methods.contains_key(method_name)),
            _ => false,
        });
        if duplicate
            || self
                .trait_impls
                .get(&implementation.trait_name)
                .is_some_and(|types| types.contains(&receiver))
        {
            return Err(SemanticError::DuplicateTraitImpl {
                trait_name: implementation.trait_name.clone(),
                type_name: implementation.type_name.clone(),
                span: implementation.span,
            });
        }
        match receiver {
            Type::Struct(id) => {
                let info = self
                    .structs
                    .values_mut()
                    .find(|info| info.id == id)
                    .expect("resolved struct");
                for (name, signature) in signatures {
                    info.methods.insert(name, signature);
                }
            }
            Type::Class(id) => {
                let info = self
                    .classes
                    .values_mut()
                    .find(|info| info.id == id)
                    .expect("resolved class");
                for (name, signature) in signatures {
                    info.methods.insert(name, signature);
                }
            }
            _ => unreachable!(),
        }
        self.trait_impls
            .entry(implementation.trait_name.clone())
            .or_default()
            .insert(receiver);
        if implementation.trait_name == "Show" {
            self.show_impls.insert(receiver);
        }
        Ok(())
    }

    pub(super) fn check_impl(&mut self, implementation: &Impl) -> Result<(), SemanticError> {
        if implementation.trait_name == "Hash" && implementation.methods.is_empty() {
            let receiver = self
                .structs
                .get(&implementation.type_name)
                .map(|info| Type::Struct(info.id));
            if !receiver.is_some_and(|ty| self.is_structural_map_key(ty, &mut Vec::new())) {
                return Err(SemanticError::FunctionNotSupported {
                    name: "empty Hash impl requires structural equality; custom PartialEq requires an explicit Hash.hash implementation".into(),
                    span: implementation.span,
                });
            }
        }
        for required in match implementation.trait_name.as_str() {
            "Eq" | "PartialOrd" => &["PartialEq"][..],
            "Ord" => &["Eq", "PartialOrd"][..],
            _ => &[],
        } {
            let receiver = self
                .structs
                .get(&implementation.type_name)
                .map(|info| Type::Struct(info.id))
                .or_else(|| {
                    self.classes
                        .get(&implementation.type_name)
                        .map(|info| Type::Class(info.id))
                })
                .expect("declared impl receiver");
            let satisfied = self.implements_trait(receiver, required);
            if !satisfied {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name: (*required).into(),
                    actual: implementation.type_name.clone(),
                    span: implementation.span,
                });
            }
        }
        let trait_name =
            self.import_dynamic_trait(&implementation.trait_name, implementation.span)?;
        let receiver = self
            .structs
            .get(&implementation.type_name)
            .map(|info| Type::Struct(info.id))
            .or_else(|| {
                self.classes
                    .get(&implementation.type_name)
                    .map(|info| Type::Class(info.id))
            })
            .expect("registered impl target");
        for method in &implementation.methods {
            let name = super::trait_method_name(self.definition_module, &trait_name, &method.name);
            let signature = match receiver {
                Type::Struct(id) => self
                    .structs
                    .values()
                    .find(|info| info.id == id)
                    .and_then(|info| info.methods.get(&name))
                    .expect("registered impl method")
                    .clone(),
                Type::Class(id) => self
                    .classes
                    .values()
                    .find(|info| info.id == id)
                    .and_then(|info| info.methods.get(&name))
                    .expect("registered impl method")
                    .clone(),
                _ => unreachable!(),
            };
            let mut checked = self.check_method_body(method, receiver, signature)?;
            if trait_name == "Drop" {
                for group in checked.declared_effects.iter() {
                    for operation in self.effects.all_operations(group) {
                        checked.used_effects.insert(operation);
                    }
                }
            }
            match receiver {
                Type::Struct(id) => self
                    .structs
                    .values_mut()
                    .find(|info| info.id == id)
                    .expect("registered impl target")
                    .methods
                    .insert(name.clone(), checked),
                Type::Class(id) => self
                    .classes
                    .values_mut()
                    .find(|info| info.id == id)
                    .expect("registered impl target")
                    .methods
                    .insert(name.clone(), checked),
                _ => unreachable!(),
            };
        }
        Ok(())
    }

    pub(super) fn replace_self_type(&mut self, ty: Type, receiver: Type) -> Type {
        match ty {
            Type::SelfType => receiver,
            Type::Tuple(id) => {
                let elements = self.tuple_types[id]
                    .clone()
                    .into_iter()
                    .map(|element| self.replace_self_type(element, receiver))
                    .collect();
                Type::Tuple(super::checker::intern_tuple(
                    &mut self.tuple_types,
                    elements,
                ))
            }
            Type::Option(id) => {
                let value = self.replace_self_type(self.option_types[id], receiver);
                Type::Option(super::checker::intern_option(&mut self.option_types, value))
            }
            Type::Result(id) => {
                let (ok, err) = self.result_types[id];
                let ok = self.replace_self_type(ok, receiver);
                let err = self.replace_self_type(err, receiver);
                Type::Result(super::checker::intern_result(
                    &mut self.result_types,
                    ok,
                    err,
                ))
            }
            Type::List(id) => {
                let value = self.replace_self_type(self.list_types[id], receiver);
                Type::List(super::checker::intern_list(&mut self.list_types, value))
            }
            Type::Map(id) => {
                let info = self.maps[id];
                let key = self.replace_self_type(info.key, receiver);
                let value = self.replace_self_type(info.value, receiver);
                Type::Map(super::checker::intern_map(&mut self.maps, key, value))
            }
            Type::MutMap(id) => {
                let info = self.maps[id];
                let key = self.replace_self_type(info.key, receiver);
                let value = self.replace_self_type(info.value, receiver);
                Type::MutMap(super::checker::intern_map(&mut self.maps, key, value))
            }
            Type::MutSet(id) => {
                let key = self.replace_self_type(self.maps[id].key, receiver);
                Type::MutSet(super::checker::intern_map(&mut self.maps, key, Type::Bool))
            }
            Type::Associated(id) => {
                let info = self.associated_types[id].clone();
                if info.parameter.is_some() {
                    return Type::Associated(id);
                }
                if let Type::Param(parameter) = receiver {
                    return super::checker::intern_associated_type(
                        &mut self.associated_types,
                        Some(parameter),
                        &info.trait_name,
                        &info.name,
                    );
                }
                self.associated_type_value(receiver, &info.trait_name, &info.name)
                    .unwrap_or(Type::Associated(id))
            }
            other => other,
        }
    }

    /// Registers a function signature
    pub(super) fn register_function(&mut self, function: &Function) -> Result<(), SemanticError> {
        if function
            .return_type
            .as_ref()
            .is_some_and(|ty| ty.is_name("type"))
        {
            return self.register_type_function(function);
        }
        if function.name == "main" && !function.type_parameters.is_empty() {
            return Err(SemanticError::GenericParametersNotSupported {
                target: "main".to_owned(),
                span: function.type_parameters[0].span,
            });
        }
        // Check for duplicate function names
        if self.functions.contains_key(&function.name)
            || self.type_functions.contains_key(&function.name)
            || self.constants.contains_key(&function.name)
        {
            return Err(SemanticError::DuplicateFunctionDefinition {
                name: function.name.clone(),
                span: function.span,
            });
        }

        let signature = self.function_signature(function)?;
        if function.foreign.is_some() {
            self.foreign_functions.insert(function.name.clone());
        }
        self.functions.insert(function.name.clone(), signature);

        Ok(())
    }

    /// Checks the function body
    pub(super) fn check_function(&mut self, function: &Function) -> Result<(), SemanticError> {
        if function.foreign.is_some() {
            // Its declared signature is the contract; there is no Joky body.
            return Ok(());
        }
        if function.receiver_mode.is_some() {
            return Err(SemanticError::FunctionNotSupported {
                name: "self receivers are only allowed on methods".to_owned(),
                span: function.span,
            });
        }
        if let Some(parameter) = function
            .parameters
            .iter()
            .find(|parameter| parameter.name == "self")
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "self is reserved for the method receiver".to_owned(),
                span: parameter.span,
            });
        }
        if self.type_functions.contains_key(&function.name) {
            return self.validate_type_function(function);
        }
        // Get the function signature
        let signature = self.functions.get(&function.name).unwrap().clone();
        self.current_effects = super::effects::EffectSet::new();
        self.current_effect_groups = signature.declared_effects.iter().collect();
        self.current_type_parameters = signature.type_parameters.clone();
        self.current_type_parameter_bounds = signature.type_parameter_bounds.clone();
        self.current_associated_bounds = signature.associated_bounds.clone();
        let previous_module_prefix = std::mem::replace(
            &mut self.current_module_prefix,
            module_prefix(&function.name),
        );

        // Enter a new scope
        self.scopes.push(HashMap::new());

        // Bind parameters
        for (name, ty) in &signature.parameters {
            self.bind(name.clone(), *ty);
        }
        self.return_types.push(signature.return_type);

        for ((_, ty), borrowed) in signature
            .parameters
            .iter()
            .zip(&signature.parameter_borrows)
        {
            if !borrowed {
                self.record_drop_effects(*ty, function.span)?;
            }
        }
        // Check the function body
        // Result-returning main needs an expected type to resolve generic
        // constructors such as Ok/Err. Other main functions keep the legacy
        // permissive body-checking behavior.
        let result_main = if function.name == "main" {
            match signature.return_type {
                Type::Result(result_id) => {
                    let (ok, err) = self.result_types[result_id];
                    ok == Type::Unit && err == Type::String
                }
                _ => false,
            }
        } else {
            false
        };
        let body_type = if function.name != "main" || result_main {
            self.check_expression(
                &function.body,
                TypeExpectation::require(signature.return_type),
            )?
        } else {
            self.check_expression(&function.body, TypeExpectation::none())?
        };
        self.return_types.pop();

        if function.name == "main" && matches!(signature.return_type, Type::Result(_)) {
            let Type::Result(result_id) = signature.return_type else {
                unreachable!();
            };
            let (ok, err) = self.result_types[result_id];
            if ok != Type::Unit || err != Type::String {
                return Err(SemanticError::FunctionNotSupported {
                    name: "main may return only Result(Unit, String)".to_owned(),
                    span: function.span,
                });
            }
            if body_type != signature.return_type {
                return Err(type_mismatch(
                    signature.return_type,
                    body_type,
                    function.body.span,
                ));
            }
        }

        // Validate the return type (non-main functions)
        if function.name != "main" && body_type != signature.return_type {
            return Err(type_mismatch(
                signature.return_type,
                body_type,
                function.body.span,
            ));
        }
        for operation in self.current_effects.iter() {
            if !self.current_effect_groups.contains(&operation.effect) {
                let info = self.effects.effect(operation.effect).expect("effect id");
                let operation = self
                    .effects
                    .operation_info(operation)
                    .expect("operation id");
                return Err(SemanticError::UnhandledEffect {
                    effect: info.name.clone(),
                    operation: operation.name.clone(),
                    span: function.body.span,
                });
            }
        }

        if let Some(signature) = self.functions.get_mut(&function.name) {
            signature.used_effects = self.current_effects.clone();
        }

        // Exit the scope
        self.scopes.pop();
        self.current_type_parameters.clear();
        self.current_type_parameter_bounds.clear();
        self.current_associated_bounds.clear();
        self.current_effects = super::effects::EffectSet::new();
        self.current_effect_groups.clear();
        self.current_module_prefix = previous_module_prefix;

        Ok(())
    }

    pub(super) fn check_struct(&mut self, definition: &Struct) -> Result<(), SemanticError> {
        let structure = self
            .structs
            .get(&definition.name)
            .expect("registered struct")
            .clone();
        for ((_, field_type), default) in structure.fields.iter().zip(&structure.defaults) {
            if let Some(default) = default {
                self.check_expression(default, TypeExpectation::require(*field_type))?;
            }
        }
        for method in &definition.methods {
            let signature = structure
                .methods
                .get(&method.name)
                .expect("registered method")
                .clone();
            let checked = self.check_method_body(method, Type::Struct(structure.id), signature)?;
            self.structs
                .get_mut(&definition.name)
                .expect("registered struct")
                .methods
                .insert(method.name.clone(), checked);
        }
        Ok(())
    }

    pub(super) fn check_class(&mut self, definition: &Class) -> Result<(), SemanticError> {
        let class = self
            .classes
            .get(&definition.name)
            .expect("registered class")
            .clone();
        for (_, field, default) in &class.fields {
            if let Some(default) = default {
                self.check_expression(default, TypeExpectation::require(field.ty))?;
            }
        }
        for method in &definition.methods {
            let signature = class
                .methods
                .get(&method.name)
                .expect("registered method")
                .clone();
            let checked = self.check_method_body(method, Type::Class(class.id), signature)?;
            self.classes
                .get_mut(&definition.name)
                .expect("registered class")
                .methods
                .insert(method.name.clone(), checked);
        }
        Ok(())
    }

    fn check_method_body(
        &mut self,
        method: &Function,
        receiver: Type,
        mut signature: FunctionSignature,
    ) -> Result<FunctionSignature, SemanticError> {
        if method
            .parameters
            .iter()
            .any(|parameter| parameter.name == "self")
        {
            return Err(SemanticError::FunctionNotSupported {
                name: "self is reserved for the method receiver".to_owned(),
                span: method.span,
            });
        }
        self.current_effects = super::effects::EffectSet::new();
        self.current_effect_groups = signature.declared_effects.iter().collect();
        self.current_type_parameters = signature.type_parameters.clone();
        self.current_type_parameter_bounds = signature.type_parameter_bounds.clone();
        self.current_associated_bounds = signature.associated_bounds.clone();
        self.scopes.push(HashMap::new());
        if signature.receiver_mode != crate::syntax::ReceiverMode::Static {
            self.bind("self".to_owned(), receiver);
        }
        for (name, ty) in &signature.parameters {
            self.bind(name.clone(), *ty);
        }
        self.return_types.push(signature.return_type);
        if signature.receiver_mode == crate::syntax::ReceiverMode::Owned {
            self.record_drop_effects(receiver, method.span)?;
        }
        for ((_, ty), borrowed) in signature
            .parameters
            .iter()
            .zip(&signature.parameter_borrows)
        {
            if !borrowed {
                self.record_drop_effects(*ty, method.span)?;
            }
        }
        let body_type = self.check_expression(
            &method.body,
            TypeExpectation::require(signature.return_type),
        )?;
        self.return_types.pop();
        if body_type != signature.return_type {
            return Err(type_mismatch(
                signature.return_type,
                body_type,
                method.body.span,
            ));
        }
        for operation in self.current_effects.iter() {
            if !self.current_effect_groups.contains(&operation.effect) {
                let info = self.effects.effect(operation.effect).expect("effect id");
                let operation = self
                    .effects
                    .operation_info(operation)
                    .expect("operation id");
                return Err(SemanticError::UnhandledEffect {
                    effect: info.name.clone(),
                    operation: operation.name.clone(),
                    span: method.body.span,
                });
            }
        }
        signature.used_effects = self.current_effects.clone();
        self.scopes.pop();
        self.current_type_parameters.clear();
        self.current_type_parameter_bounds.clear();
        self.current_associated_bounds.clear();
        self.current_effects = super::effects::EffectSet::new();
        self.current_effect_groups.clear();
        Ok(signature)
    }
}

fn module_prefix(name: &str) -> Option<String> {
    if name.starts_with("@prelude/") {
        return name
            .rsplit_once('/')
            .map(|(prefix, _)| format!("{prefix}/"));
    }
    let suffix = name.strip_prefix('m')?;
    let separator = suffix.find('_')?;
    (!suffix[..separator].is_empty()
        && suffix[..separator]
            .chars()
            .all(|character| character.is_ascii_digit()))
    .then(|| format!("m{}_", &suffix[..separator]))
}
