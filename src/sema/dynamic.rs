use std::collections::{BTreeMap, BTreeSet};

use crate::diagnostic::SemanticError;
use crate::syntax::{
    BinaryOp, Expr, ExprKind, FieldAccess, ReceiverMode, TypeAnnotation, TypeExpr,
};
use crate::Span;

use super::checker::Checker;
use super::symbol_table::FunctionTypeInfo;
use super::{EffectSet, Type};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DynamicMethod {
    pub(crate) effect_names: Vec<String>,
    pub(crate) name: String,
    pub(crate) implementation_name: String,
    pub(crate) receiver: ReceiverMode,
    pub(crate) signature: usize,
    pub(crate) parameter_borrows: Vec<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DynamicType {
    pub(crate) trait_names: Vec<String>,
    pub(crate) bindings: BTreeMap<String, Type>,
    pub(crate) methods: Vec<DynamicMethod>,
}

impl DynamicType {
    pub(crate) fn upcast_slots<'a>(
        &self,
        target: &Self,
        signatures: impl Fn(usize) -> &'a FunctionTypeInfo,
    ) -> Result<Vec<usize>, String> {
        for name in &target.trait_names {
            if !self.includes(name) {
                return Err(format!(
                    "dynamic upcast requires source interface to include trait '{name}'"
                ));
            }
        }
        for (name, ty) in &target.bindings {
            if self.bindings.get(name) != Some(ty) {
                return Err(format!(
                    "dynamic upcast associated type '{name}' does not match"
                ));
            }
        }
        target
            .methods
            .iter()
            .map(|method| {
                let (slot, source) = self
                    .methods
                    .iter()
                    .enumerate()
                    .find(|(_, source)| source.name == method.name)
                    .ok_or_else(|| format!("dynamic upcast is missing method '{}'", method.name))?;
                let source_signature = signatures(source.signature);
                let target_signature = signatures(method.signature);
                if source.receiver != method.receiver
                    || source.parameter_borrows != method.parameter_borrows
                    || source_signature.parameters != target_signature.parameters
                    || source_signature.parameter_names != target_signature.parameter_names
                    || source_signature.return_type != target_signature.return_type
                    || source_signature.effects != target_signature.effects
                    || source_signature.suspends != target_signature.suspends
                {
                    return Err(format!(
                        "dynamic upcast method '{}' has incompatible signatures",
                        method.name
                    ));
                }
                Ok(slot)
            })
            .collect()
    }

    pub(crate) fn includes(&self, name: &str) -> bool {
        self.trait_names.iter().any(|item| item == name)
    }

    pub(crate) fn qualify(&mut self, module: crate::module::StableId) {
        for name in &mut self.trait_names {
            *name = super::import_methods::trait_key(module, name);
        }
        self.trait_names.sort();
    }
}

pub(super) fn trait_names_from_expression(expr: &Expr) -> Result<Vec<String>, SemanticError> {
    match &expr.kind {
        ExprKind::Name(name) => Ok(vec![name.clone()]),
        ExprKind::Field {
            value,
            access: FieldAccess::Name(name),
        } => {
            let ExprKind::Name(alias) = &value.kind else {
                return Err(invalid("Dyn requires trait names joined by '+'", expr.span));
            };
            Ok(vec![format!("{alias}.{name}")])
        }
        ExprKind::Binary {
            op: BinaryOp::Add,
            left,
            right,
        } => {
            let mut names = trait_names_from_expression(left)?;
            names.extend(trait_names_from_expression(right)?);
            Ok(names)
        }
        _ => Err(invalid("Dyn requires trait names joined by '+'", expr.span)),
    }
}

pub(super) fn trait_names_from_annotation(
    annotation: &TypeAnnotation,
) -> Result<Vec<String>, SemanticError> {
    match &annotation.kind {
        TypeExpr::Name(name) => Ok(vec![name.clone()]),
        TypeExpr::Member { base, name } if base.as_name().is_some() => {
            Ok(vec![format!("{}.{name}", base.as_name().unwrap())])
        }
        TypeExpr::TraitComposition(traits) => {
            let mut names = Vec::new();
            for item in traits {
                names.extend(trait_names_from_annotation(item)?);
            }
            Ok(names)
        }
        _ => Err(invalid(
            "Dyn requires trait names joined by '+'",
            annotation.span,
        )),
    }
}

pub(super) fn invalid(message: impl Into<String>, span: Span) -> SemanticError {
    SemanticError::FunctionNotSupported {
        name: message.into(),
        span,
    }
}

impl Checker {
    pub(super) fn resolve_dynamic_type(
        &mut self,
        names: &[String],
        bindings: Vec<(Option<String>, Type)>,
        span: Span,
    ) -> Result<Type, SemanticError> {
        let mut definitions = BTreeMap::new();
        let mut methods = BTreeMap::new();
        let mut associated_names = BTreeSet::new();
        for name in names {
            let name = self.import_dynamic_trait(name, span)?;
            if name == "Drop" {
                return Err(invalid("Drop cannot be used as a dynamic interface", span));
            }
            let definition =
                self.traits
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| SemanticError::UnknownTrait {
                        name: name.clone(),
                        span,
                    })?;
            if definitions.contains_key(&name) {
                return Err(invalid(format!("duplicate Dyn trait '{name}'"), span));
            }
            for (method_name, method) in &definition.methods {
                if method.receiver_mode == ReceiverMode::Static {
                    return Err(invalid(
                        format!("static method '{method_name}' is not object safe"),
                        span,
                    ));
                }
                if methods
                    .insert(method_name.clone(), (name.clone(), method.clone()))
                    .is_some()
                {
                    return Err(invalid(
                        format!("conflicting Dyn method '{method_name}'"),
                        span,
                    ));
                }
            }
            for associated in &definition.associated_types {
                if !associated_names.insert(associated.clone()) {
                    return Err(invalid(
                        format!("conflicting Dyn associated type '{associated}'"),
                        span,
                    ));
                }
            }
            definitions.insert(name, definition);
        }
        let names = definitions.keys().cloned().collect::<Vec<_>>();
        if methods.is_empty() {
            return Err(invalid(
                "Dyn requires a trait with at least one method",
                span,
            ));
        }
        let mut associated = BTreeMap::new();
        for (label, ty) in bindings {
            let label =
                label.ok_or_else(|| invalid("Dyn associated types require labels", span))?;
            if !associated_names.contains(&label) || associated.insert(label.clone(), ty).is_some()
            {
                return Err(invalid(
                    format!("unknown or duplicate Dyn associated type '{label}'"),
                    span,
                ));
            }
        }
        if associated.len() != associated_names.len() {
            return Err(invalid(
                format!(
                    "Dyn({}) requires all associated types to be bound",
                    names.join(" + ")
                ),
                span,
            ));
        }
        if let Some(id) = self
            .dynamic_types
            .iter()
            .position(|info| info.trait_names == names && info.bindings == associated)
        {
            return Ok(Type::Dyn(id));
        }
        let id = self.dynamic_types.len();
        let receiver = Type::Dyn(id);
        for (name, definition) in definitions {
            self.trait_associated_impls.insert(
                (name, receiver),
                definition
                    .associated_types
                    .iter()
                    .map(|item| (item.clone(), associated[item]))
                    .collect(),
            );
        }
        let mut resolved = Vec::new();
        for (method_name, (trait_name, method)) in methods {
            // Keep Self unresolved: its ABI cannot be selected at a dynamic call site.
            let parameters = method
                .parameters
                .iter()
                .map(|(_, ty)| *ty)
                .collect::<Vec<_>>();
            let parameters = parameters
                .into_iter()
                .map(|ty| self.resolve_dynamic_member(ty, receiver))
                .collect::<Vec<_>>();
            let result = self.resolve_dynamic_member(method.return_type, receiver);
            if parameters
                .iter()
                .chain(std::iter::once(&result))
                .any(|ty| self.dynamic_unresolved(*ty))
            {
                return Err(invalid(format!("method '{method_name}' is not object safe: Self is only allowed as the receiver"), span));
            }
            let signature = self.function_types.len();
            let effects = self.dynamic_effects(&method.effect_names, span)?;
            self.function_types.push(FunctionTypeInfo {
                parameter_names: method
                    .parameters
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect(),
                parameters,
                return_type: result,
                effects,
                suspends: true,
            });
            resolved.push(DynamicMethod {
                effect_names: method.effect_names,
                implementation_name: super::trait_method_name(
                    self.definition_module,
                    &trait_name,
                    &method_name,
                ),
                name: method_name,
                receiver: method.receiver_mode,
                signature,
                parameter_borrows: method.parameter_borrows,
            });
        }
        self.dynamic_types.push(DynamicType {
            trait_names: names,
            bindings: associated,
            methods: resolved,
        });
        Ok(receiver)
    }

    pub(super) fn import_dynamic_trait(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<String, SemanticError> {
        let Some((alias, member)) = name.split_once('.') else {
            return Ok(name.into());
        };
        let module = self
            .import_aliases
            .get(alias)
            .copied()
            .ok_or_else(|| invalid(format!("unknown module '{alias}'"), span))?;
        self.import_trait_definition(module, member, span)
    }

    pub(super) fn import_trait_definition(
        &mut self,
        module: crate::module::StableId,
        member: &str,
        span: Span,
    ) -> Result<String, SemanticError> {
        let key = super::import_methods::trait_key(module, member);
        if self.traits.contains_key(&key) {
            return Ok(key);
        }
        let table = self
            .dependency_types
            .get(&module)
            .cloned()
            .ok_or_else(|| invalid("missing trait module", span))?;
        let mut info = table
            .interface
            .trait_definitions
            .get(member)
            .or_else(|| table.interface.trait_definitions.get(&key))
            .cloned()
            .ok_or_else(|| SemanticError::UnknownTrait {
                name: member.into(),
                span,
            })?;
        for method in info.methods.values_mut() {
            for (_, ty) in &mut method.parameters {
                *ty = self.import_abi_type(module, &table, *ty, span)?;
            }
            method.return_type = self.import_abi_type(module, &table, method.return_type, span)?;
            for effect in &mut method.effect_names {
                let group = self.import_abi_effect(module, effect, span)?;
                *effect = self.effects.effect(group).unwrap().name.clone();
            }
        }
        self.traits.insert(key.clone(), info);
        Ok(key)
    }

    pub(super) fn dynamic_effects(
        &mut self,
        names: &[String],
        span: Span,
    ) -> Result<EffectSet, SemanticError> {
        let mut effects = EffectSet::new();
        for name in names {
            let id = if let Some(id) = self.effects.by_name(name) {
                id
            } else {
                let (alias, effect) = name.split_once('.').unwrap_or((name, name));
                let module = self.import_aliases.get(alias).copied().ok_or_else(|| {
                    invalid(format!("unknown dynamic method effect '{name}'"), span)
                })?;
                self.import_abi_effect(module, effect, span)?
            };
            for operation in self.effects.all_operations(id) {
                effects.insert(operation);
            }
        }
        Ok(effects)
    }

    pub(super) fn refresh_dynamic_effects(&mut self) -> Result<(), SemanticError> {
        for info in self.dynamic_types.clone() {
            for method in info.methods {
                let effects = self.dynamic_effects(&method.effect_names, Span::new(0, 0))?;
                self.function_types[method.signature].effects = effects;
            }
        }
        Ok(())
    }

    fn resolve_dynamic_member(&mut self, ty: Type, receiver: Type) -> Type {
        if ty == Type::SelfType {
            return ty;
        }
        // Associated type lookup uses the bindings registered for this dynamic type.
        match ty {
            Type::Associated(_) => self.replace_self_type(ty, receiver),
            Type::Option(id) => {
                let item = self.resolve_dynamic_member(self.option_types[id], receiver);
                Type::Option(super::checker::intern_option(&mut self.option_types, item))
            }
            Type::Result(id) => {
                let (a, b) = self.result_types[id];
                let a = self.resolve_dynamic_member(a, receiver);
                let b = self.resolve_dynamic_member(b, receiver);
                Type::Result(super::checker::intern_result(&mut self.result_types, a, b))
            }
            Type::Tuple(id) => {
                let items = self.tuple_types[id]
                    .clone()
                    .into_iter()
                    .map(|ty| self.resolve_dynamic_member(ty, receiver))
                    .collect();
                Type::Tuple(super::checker::intern_tuple(&mut self.tuple_types, items))
            }
            Type::List(id) | Type::MutList(id) => {
                let item = self.resolve_dynamic_member(self.list_types[id], receiver);
                let id = super::checker::intern_list(&mut self.list_types, item);
                if matches!(ty, Type::List(_)) {
                    Type::List(id)
                } else {
                    Type::MutList(id)
                }
            }
            Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                let info = self.maps[id];
                let key = self.resolve_dynamic_member(info.key, receiver);
                let value = self.resolve_dynamic_member(info.value, receiver);
                let id = super::checker::intern_map(&mut self.maps, key, value);
                match ty {
                    Type::Map(_) => Type::Map(id),
                    Type::MutMap(_) => Type::MutMap(id),
                    _ => Type::MutSet(id),
                }
            }
            Type::Cown(id) => {
                let item = self.resolve_dynamic_member(self.cowns[id], receiver);
                Type::Cown(super::checker::intern_cown(&mut self.cowns, item))
            }
            Type::Function(id) => {
                let mut info = self.function_types[id].clone();
                info.parameters = info
                    .parameters
                    .into_iter()
                    .map(|ty| self.resolve_dynamic_member(ty, receiver))
                    .collect();
                info.return_type = self.resolve_dynamic_member(info.return_type, receiver);
                let id = self
                    .function_types
                    .iter()
                    .position(|existing| {
                        existing.parameter_names == info.parameter_names
                            && existing.parameters == info.parameters
                            && existing.return_type == info.return_type
                            && existing.effects == info.effects
                            && existing.suspends == info.suspends
                    })
                    .unwrap_or_else(|| {
                        let id = self.function_types.len();
                        self.function_types.push(info);
                        id
                    });
                Type::Function(id)
            }
            _ => ty,
        }
    }

    fn dynamic_unresolved(&self, ty: Type) -> bool {
        match ty {
            Type::SelfType | Type::Associated(_) | Type::Param(_) => true,
            Type::Option(id) => self.dynamic_unresolved(self.option_types[id]),
            Type::List(id) | Type::MutList(id) => self.dynamic_unresolved(self.list_types[id]),
            Type::Result(id) => {
                let (a, b) = self.result_types[id];
                self.dynamic_unresolved(a) || self.dynamic_unresolved(b)
            }
            Type::Tuple(id) => self.tuple_types[id]
                .iter()
                .any(|ty| self.dynamic_unresolved(*ty)),
            Type::Function(id) => {
                let info = &self.function_types[id];
                info.parameters
                    .iter()
                    .chain(std::iter::once(&info.return_type))
                    .any(|ty| self.dynamic_unresolved(*ty))
            }
            Type::Map(id) | Type::MutMap(id) | Type::MutSet(id) => {
                self.dynamic_unresolved(self.maps[id].key)
                    || self.dynamic_unresolved(self.maps[id].value)
            }
            Type::Cown(id) => self.dynamic_unresolved(self.cowns[id]),
            Type::CPtr(id) | Type::CMutPtr(id) => self.dynamic_unresolved(self.c_pointers[id]),
            Type::CArray(id) => self.dynamic_unresolved(self.c_arrays[id].0),
            _ => false,
        }
    }
}
