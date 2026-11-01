//! Canonical borrowed trait interfaces and declaration-scoped qualification.
use super::*;
use crate::core::types::{DynInterface, DynMethodSlot, DynTraitType};

impl TypeRegistry {
    pub(crate) fn normalize_dyn_trait(&self, view: &DynTraitType) -> Result<DynTraitType, Error> {
        self.obligation(
            Obligation::DynQualification(Type::DynTrait(Arc::new(view.clone()))),
            view.span,
            || self.check_dyn_trait_inner(view),
        )
    }

    fn check_dyn_trait_inner(&self, view: &DynTraitType) -> Result<DynTraitType, Error> {
        let root = &self.trait_environment.root;
        let id = match view.identity {
            Some(id) => id,
            None => self.trait_id(&view.name, root, view.span)?,
        };
        let declaration = self
            .trait_environment
            .declarations
            .iter()
            .find(|d| d.self_parameter.id == id)
            .ok_or_else(|| {
                error(ErrorCode::UnknownType, "Unknown dyn trait identity").with_span(view.span)
            })?;
        let bad = |message: String| error(ErrorCode::TypeMismatch, message).with_span(view.span);
        if declaration.is_drop {
            return Err(bad(
                "The built-in Drop protocol cannot be used as dyn".into()
            ));
        }
        let scope = declaration
            .scope
            .as_ref()
            .map_or(root.as_ref(), |s| s.imports.as_ref());
        let mut names = BTreeSet::new();
        let mut equalities = Vec::new();
        for equality in &view.equalities {
            if !names.insert(&equality.name) {
                return Err(error(
                    ErrorCode::DuplicateDefinition,
                    "Duplicate dyn associated equality",
                )
                .with_span(equality.span));
            }
            let associated = declaration
                .associated_types
                .iter()
                .find(|a| a.name == equality.name)
                .ok_or_else(|| {
                    error(
                        ErrorCode::UnknownType,
                        format!("Unknown dyn associated type '{}'", equality.name),
                    )
                    .with_span(equality.span)
                })?;
            let ty = self
                .resolve_scoped(&equality.ty, root, &[])
                .map_err(|e| e.with_span(equality.span))?;
            if !self.bounds_hold(&associated.bounds, scope, &ty, &Substitutions::default())? {
                return Err(error(
                    ErrorCode::TypeMismatch,
                    "Dyn associated equality does not satisfy the item's declared capabilities",
                )
                .with_span(equality.span));
            }
            equalities.push(AssociatedEquality {
                ty,
                ..equality.clone()
            });
        }
        for associated in &declaration.associated_types {
            if !names.contains(&associated.name) {
                return Err(bad(format!(
                    "Dyn trait requires an explicit equality for '{}'",
                    associated.name
                )));
            }
        }
        equalities.sort_by_key(|e| {
            declaration
                .associated_types
                .iter()
                .position(|a| a.name == e.name)
                .unwrap()
        });
        let mut methods = Vec::new();
        // Resolve Self.Item through the existing projection solver with these
        // explicit equalities. Never infer bindings from a candidate impl.
        let mut local = self.clone();
        // A fresh rigid parameter also isolates solver cache keys between views
        // with different equalities, while retaining the shared work budget.
        let mut parameter = TypeParameter::new("Self".into(), view.span);
        parameter.bounds = declaration.self_parameter.bounds.clone();
        for bound in &mut parameter.bounds {
            if self.trait_id(&bound.name, scope, bound.span)? == id {
                bound.equalities = equalities.clone();
            }
        }
        let parameter = Arc::new(parameter);
        let self_id = parameter.id;
        local.parameters.insert(self_id, parameter.name.clone());
        Rc::make_mut(&mut Rc::make_mut(&mut local.trait_environment).parameters)
            .insert(self_id, (parameter.clone(), Arc::new(scope.clone())));
        let substitutions = [(id, Type::Parameter(parameter.clone()))]
            .into_iter()
            .collect::<Substitutions>();
        for method in &declaration.methods {
            let incompatible = |reason: &str| {
                bad(format!(
                    "Trait '{}' cannot be used as dyn: method '{}' {reason}",
                    view.name, method.name
                ))
            };
            let Some(receiver) = method.params.first().filter(|p| p.name == "self") else {
                return Err(incompatible("requires a borrowed receiver"));
            };
            if !receiver
                .ty
                .reference_type()
                .is_some_and(|r| matches!(&r.target, Type::Parameter(p) if p.id == id))
            {
                return Err(incompatible("requires &self or &mut self"));
            }
            let mut params = Vec::new();
            let mut noescape_params = Vec::new();
            for (index, param) in method.params.iter().enumerate() {
                // Resolve declaration-scoped names before substitution rebuilds
                // aggregate layouts (which require fully resolved elements).
                let ty = local.resolve_scoped(
                    &param.ty,
                    scope,
                    std::slice::from_ref(&declaration.self_parameter),
                )?;
                let ty = substitute(&ty, &substitutions, &local)?;
                let ty = local.resolve_scoped(&ty, scope, std::slice::from_ref(&parameter))?;
                if index > 0 && contains_self(&ty, self_id) {
                    return Err(incompatible("uses Self outside its receiver"));
                }
                if method.suspends && index > 0 && ty.contains_references() {
                    return Err(incompatible(
                        "requires reference-free explicit parameters when suspending",
                    ));
                }
                if ty.contains_references()
                    && !method
                        .noescape
                        .as_ref()
                        .is_some_and(|n| n.all || n.params.contains(&param.name))
                {
                    return Err(incompatible(
                        "requires a declared @noescape contract for every borrowed parameter",
                    ));
                }
                if ty.contains_references() {
                    noescape_params.push(index);
                }
                params.push(if index == 0 {
                    Type::reference(Type::Unit, receiver.ty.reference_type().unwrap().mutable)
                } else {
                    ty
                });
            }
            let result = local.resolve_scoped(
                &method.ret_ty,
                scope,
                std::slice::from_ref(&declaration.self_parameter),
            )?;
            let result = substitute(&result, &substitutions, &local)?;
            let result = local.resolve_scoped(&result, scope, std::slice::from_ref(&parameter))?;
            if contains_self(&result, self_id) {
                return Err(incompatible("returns Self outside its receiver"));
            }
            if result.contains_references() {
                return Err(incompatible(
                    "cannot return references or results containing references",
                ));
            }
            if TypeWalk::new(&result, TypeEdges::Storage).any(|t| {
                matches!(t, Type::Projection(_))
                    && !TypeWalk::new(t, TypeEdges::Arguments)
                        .any(|t| matches!(t, Type::Parameter(_)))
            }) {
                return Err(incompatible("has an unresolved result projection"));
            }
            methods.push(DynMethodSlot {
                suspends: method.suspends,
                name: method.name.clone(),
                mutable: receiver.ty.reference_type().unwrap().mutable,
                signature: crate::FunctionSignature::new(params, result),
                noescape_params,
                span: method.signature_span,
            });
        }
        Ok(DynTraitType {
            identity: Some(id),
            interface: Some(Arc::new(DynInterface {
                trait_id: id,
                bindings: equalities
                    .iter()
                    .map(|e| (e.name.clone(), e.ty.clone()))
                    .collect(),
                methods,
            })),
            equalities,
            ..view.clone()
        })
    }

    pub(crate) fn check_dyn_erasure(
        &self,
        actual: &Type,
        interface: &DynInterface,
        span: Span,
    ) -> Result<(), Error> {
        if actual.contains_dyn_view()
            || matches!(actual, Type::DynTrait(_))
            || !self.implements(actual, interface.trait_id)?
        {
            return Err(error(
                ErrorCode::TypeMismatch,
                format!("Type '{actual}' has no applicable impl for the dyn trait"),
            )
            .with_span(span));
        }
        for (index, (name, expected)) in interface.bindings.iter().enumerate() {
            let projection = TypeProjection {
                base: actual.clone(),
                name: name.clone(),
                trait_name: None,
                identity: Some((interface.trait_id, index)),
                span,
            };
            let found = self.normalize_projection(&projection, &HashMap::new())?;
            if found != *expected {
                return Err(error(ErrorCode::TypeMismatch, format!("Dyn associated equality '{name}' mismatch: expected {expected}, got {found}")).with_span(span));
            }
        }
        Ok(())
    }
}

fn contains_self(ty: &Type, id: u64) -> bool {
    TypeWalk::new(ty, TypeEdges::Fields).any(|t| matches!(t, Type::Parameter(p) if p.id == id))
}
