use crate::diagnostic::SemanticError;
use crate::sema::checker::Checker;
use crate::sema::expectation::TypeExpectation;
use crate::sema::methods::{method_source_name, method_trait_name, ResolvedMethodCall};
use crate::sema::symbol_table::FunctionSignature;
use crate::sema::{trait_method_name, Type};
use crate::syntax::{CallArgument, Expr, ExprKind, FieldAccess, NodeId};
use crate::Span;

impl Checker {
    pub(super) fn qualified_trait_call(
        &mut self,
        callee: &Expr,
        arguments: &[CallArgument],
        span: Span,
        call_id: NodeId,
    ) -> Result<Option<Type>, SemanticError> {
        let ExprKind::Field {
            value,
            access: FieldAccess::Name(method),
        } = &callee.kind
        else {
            return Ok(None);
        };
        let Some(path) = super::effect_path(value) else {
            return Ok(None);
        };
        let root = path.split('.').next().unwrap();
        if self.lookup(root).is_some() {
            return Ok(None);
        }
        if !self.traits.contains_key(&path) {
            let imported_trait = path.split_once('.').is_some_and(|(alias, member)| {
                self.import_aliases.get(alias).is_some_and(|module| {
                    self.dependency_types.get(module).is_some_and(|table| {
                        table.interface.trait_definitions.contains_key(member)
                            || table
                                .interface
                                .trait_definitions
                                .contains_key(&crate::sema::trait_key(*module, member))
                    })
                })
            });
            if !imported_trait {
                return Ok(None);
            }
        }
        let trait_name = self.import_dynamic_trait(&path, value.span)?;
        let definition = self
            .traits
            .get(&trait_name)
            .and_then(|info| info.methods.get(method))
            .cloned()
            .ok_or_else(|| SemanticError::UnknownFunction {
                name: format!("{path}.{method}"),
                span: callee.span,
            })?;
        if arguments.len() != definition.parameters.len() + 1 {
            return Err(SemanticError::WrongArgumentCount {
                function: format!("{path}.{method}"),
                expected: definition.parameters.len() + 1,
                span,
            });
        }
        let receiver = &arguments[0];
        if definition.receiver_mode == crate::syntax::ReceiverMode::Static {
            let target = self
                .resolve_type_value_arguments(call_id, &arguments[..1], 1)?
                .ok_or_else(|| SemanticError::FunctionNotSupported {
                    name: format!("{path}.{method} requires a positional compile-time target type"),
                    span,
                })?[0];
            if !self.implements_trait(target, &trait_name) {
                return Err(SemanticError::TraitBoundNotSatisfied {
                    trait_name,
                    actual: crate::sema::type_name(target),
                    span,
                });
            }
            let result =
                self.check_bound_method(target, &trait_name, method, &arguments[1..], span)?;
            self.resolved_method_calls.insert(
                call_id,
                ResolvedMethodCall {
                    name: trait_method_name(self.definition_module, &trait_name, method),
                    qualified: true,
                    static_target: Some(target),
                },
            );
            return Ok(Some(result));
        }
        if receiver
            .label
            .as_deref()
            .is_some_and(|label| label != "self")
        {
            return Err(SemanticError::UnknownFunction {
                name: receiver.label.clone().unwrap(),
                span: receiver.value.span,
            });
        }
        let ty = self.check_expression(&receiver.value, TypeExpectation::none())?;
        let implements = match ty {
            Type::Dyn(id) => self.dynamic_types[id].includes(&trait_name),
            _ => self.implements_trait(ty, &trait_name),
        };
        if !implements {
            return Err(SemanticError::TraitBoundNotSatisfied {
                trait_name,
                actual: crate::sema::type_name(ty),
                span: receiver.value.span,
            });
        }
        if trait_name == "Drop" {
            return Err(SemanticError::FunctionNotSupported {
                name: "Drop.drop is invoked automatically and cannot be called directly".into(),
                span,
            });
        }
        let name = trait_method_name(self.definition_module, &trait_name, method);
        self.resolved_method_calls.insert(
            call_id,
            ResolvedMethodCall {
                name: name.clone(),
                qualified: true,
                static_target: None,
            },
        );
        if matches!(ty, Type::Param(_)) {
            return self
                .check_bound_method(ty, &trait_name, method, &arguments[1..], span)
                .map(Some);
        }
        // Reuse ordinary checking for concrete, built-in and dynamic receivers.
        let selected = Expr {
            id: callee.id,
            span: callee.span,
            kind: ExprKind::Field {
                value: Box::new(receiver.value.clone()),
                access: FieldAccess::Name(name),
            },
        };
        self.check_method_call(
            &selected,
            &arguments[1..],
            span,
            call_id,
            TypeExpectation::none(),
        )
        .map(Some)
    }

    pub(super) fn bound_method_candidate(
        &self,
        id: usize,
        method: &str,
        span: Span,
    ) -> Result<String, SemanticError> {
        let mut candidates = self.current_type_parameter_bounds[id]
            .iter()
            .flat_map(|name| crate::sema::methods::builtin_bounds(name))
            .filter(|name| {
                self.traits
                    .get(*name)
                    .and_then(|info| info.methods.get(method))
                    .is_some_and(|method| {
                        method.receiver_mode != crate::syntax::ReceiverMode::Static
                    })
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        match candidates.as_slice() {
            [name] => Ok(name.clone()),
            [] => Err(SemanticError::UnknownFunction {
                name: method.into(),
                span,
            }),
            _ => Err(SemanticError::AmbiguousMethod {
                name: method.into(),
                candidates,
                span,
            }),
        }
    }

    pub(in crate::sema) fn check_bound_method(
        &mut self,
        receiver: Type,
        trait_name: &str,
        method: &str,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Type, SemanticError> {
        let signature = self.traits[trait_name].methods[method].clone();
        let parameters = signature
            .parameters
            .iter()
            .map(|(name, ty)| (name.clone(), self.replace_self_type(*ty, receiver)))
            .collect::<Vec<_>>();
        for (argument, (_, ty)) in self
            .order_arguments(arguments, &parameters, method, span)?
            .into_iter()
            .zip(&parameters)
        {
            self.check_expression(argument, TypeExpectation::require(*ty))?;
        }
        let effects = self.dynamic_effects(&signature.effect_names, span)?;
        self.current_effects.extend(&effects);
        Ok(self.replace_self_type(signature.return_type, receiver))
    }

    pub(in crate::sema) fn nominal_methods(
        &self,
        ty: Type,
    ) -> Option<&std::collections::HashMap<String, FunctionSignature>> {
        match ty {
            Type::Struct(id) => self
                .structs
                .values()
                .find(|info| info.id == id)
                .map(|info| &info.methods),
            Type::Class(id) => self
                .classes
                .values()
                .find(|info| info.id == id)
                .map(|info| &info.methods),
            _ => None,
        }
    }

    pub(super) fn nominal_method_candidate(
        &self,
        ty: Type,
        method: &str,
        span: Span,
    ) -> Result<Option<String>, SemanticError> {
        let Some(methods) = self.nominal_methods(ty) else {
            return Ok(None);
        };
        let mut candidates = methods
            .iter()
            .filter(|(name, signature)| {
                method_source_name(name) == method
                    && signature.receiver_mode != crate::syntax::ReceiverMode::Static
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        match candidates.as_slice() {
            [] => Ok(None),
            [name] => Ok(Some(name.clone())),
            _ => Err(SemanticError::AmbiguousMethod {
                name: method.into(),
                candidates: candidates
                    .iter()
                    .map(|name| {
                        method_trait_name(name).unwrap_or_else(|| format!("type method {name}"))
                    })
                    .collect(),
                span,
            }),
        }
    }
}
