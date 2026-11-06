//! Infer conditional derive bounds from field obligations and existing impl contracts.
use super::*;
use crate::core::imports::{DefinitionScope, TraitBinding};
use std::collections::HashSet;

#[derive(Clone, Default)]
struct Inference {
    parameters: HashSet<u64>,
    visited: Vec<(Type, u64)>,
    bounds: Vec<(u64, TraitBound)>,
    // Failed candidate branches share the same finite inference work budget.
    work: Rc<std::cell::Cell<usize>>,
}

impl TypeRegistry {
    /// Layouts are available, but generated methods have not been checked yet.
    pub(crate) fn infer_derives(&self, module: &Module) -> Result<Option<Module>, Error> {
        if !module
            .implementations
            .iter()
            .any(|i| i.derive_fields.is_some())
        {
            return Ok(None);
        }
        let mut output = module.clone();
        for implementation in &mut output.implementations {
            let Some(fields) = implementation.derive_fields.take() else {
                continue;
            };
            let original_scope = implementation
                .scope
                .as_ref()
                .map_or(&self.trait_environment.declaration_root, |s| &s.imports);
            let name = &fields.trait_name;
            let id = self.trait_id(name, original_scope, implementation.span)?;
            let local = self.with_parameters(&implementation.type_params)?;
            let mut inference = Inference {
                parameters: implementation.type_params.iter().map(|p| p.id).collect(),
                ..Default::default()
            };
            for field in fields.fields {
                let ty = local
                    .resolve_scoped(&field.ty, original_scope, &implementation.type_params)
                    .map_err(|e| e.with_span(field.span))?;
                local
                    .infer_derive_obligation(&ty, id, &mut inference, 0, field.span)
                    .map_err(|e| e.with_span(field.span))?;
            }
            let mut imports = original_scope.as_ref().clone();
            for (parameter, bound) in inference.bounds {
                let declaration = self
                    .trait_environment
                    .declarations
                    .iter()
                    .find(|d| d.name == bound.name)
                    .unwrap();
                imports.traits.insert(
                    bound.name.clone(),
                    TraitBinding {
                        name: bound.name.clone(),
                        id: declaration.self_parameter.id,
                    },
                );
                let parameter = implementation
                    .type_params
                    .iter_mut()
                    .find(|p| p.id == parameter)
                    .unwrap();
                Arc::make_mut(parameter).bounds.push(bound);
            }
            // These bindings are private to generated methods, so inferred traits
            // cannot change method lookup in the user's surrounding source.
            let scope = if let Some(scope) = &implementation.scope {
                let mut scope = scope.as_ref().clone();
                scope.imports = Arc::new(imports);
                scope
            } else {
                DefinitionScope {
                    module_name: String::new(),
                    lazy_functions: false,
                    imports: Arc::new(imports),
                    nominals: Arc::new(
                        self.forwards
                            .borrow()
                            .iter()
                            .filter_map(|(name, ty)| match ty {
                                TypeDefinition::Type(ty) => Some((name.clone(), ty.clone())),
                                TypeDefinition::Alias(_) => None,
                            })
                            .collect(),
                    ),
                    members: self.trait_environment.root.members.clone(),
                    records: module
                        .records
                        .iter()
                        .filter(|r| !r.imported)
                        .map(|r| r.id)
                        .collect(),
                }
            };
            let scope = Arc::new(scope);
            implementation.scope = Some(scope.clone());
            let parameters = implementation
                .type_params
                .iter()
                .map(|p| (p.id, Type::Parameter(p.clone())))
                .collect();
            implementation.target =
                super::super::indirect::substitute_syntax(&implementation.target, &parameters);
            for method in &mut implementation.methods {
                method.scope = Some(scope.clone());
                for param in &mut method.params {
                    param.ty = super::super::indirect::substitute_syntax(&param.ty, &parameters);
                }
                method.ret_ty =
                    super::super::indirect::substitute_syntax(&method.ret_ty, &parameters);
                super::super::map_annotations(&mut method.body, &mut |ty, _| {
                    *ty = super::super::indirect::substitute_syntax(ty, &parameters);
                    Ok(())
                })?;
            }
        }
        Ok(Some(output))
    }

    fn infer_derive_obligation(
        &self,
        ty: &Type,
        id: u64,
        inference: &mut Inference,
        depth: usize,
        span: Span,
    ) -> Result<(), Error> {
        if depth > 32 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Derive inference exceeds 32 dependency levels",
            )
            .with_span(span));
        }
        let key = (ty.clone(), id);
        if inference.visited.contains(&key) {
            // A same-type recursive field reuses the generated implementation.
            return Ok(());
        }
        if inference.work.get() >= 256 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Derive inference exceeds 256 obligation expansions",
            )
            .with_span(span));
        }
        inference.work.set(inference.work.get() + 1);
        inference.visited.push(key);
        let declaration = self
            .trait_environment
            .declarations
            .iter()
            .find(|d| d.self_parameter.id == id)
            .unwrap();
        if let Type::Parameter(p) = ty {
            if inference.parameters.contains(&p.id) {
                inference.bounds.push((
                    p.id,
                    TraitBound {
                        name: declaration.name.clone(),
                        equalities: vec![],
                        span,
                    },
                ));
                return Ok(());
            }
        }
        let mut candidate = None;
        let mut rejected = None;
        for implementation in self.trait_environment.implementations.iter() {
            let scope = implementation
                .scope
                .as_ref()
                .map_or(&self.trait_environment.declaration_root, |s| &s.imports);
            let Some(name) = &implementation.trait_name else {
                continue;
            };
            if self.trait_id(name, scope, implementation.span)? != id {
                continue;
            }
            let target =
                self.resolve_scoped(&implementation.target, scope, &implementation.type_params)?;
            if implementation.type_params.is_empty() && target != *ty {
                continue;
            }
            let same = match (&target, ty) {
                (Type::Parameter(_), _) => true,
                (Type::Record(a), Type::Record(b)) => a.id == b.id,
                (Type::Enum(a), Type::Enum(b)) => a.id == b.id,
                _ => target == *ty,
            };
            if !same {
                continue;
            }
            let mut substitutions = Substitutions::default();
            super::super::unify(
                &target,
                ty,
                &implementation.type_params,
                &mut substitutions,
                span,
            )?;
            let mut inferred = inference.clone();
            let inference = &mut inferred;
            let attempt = (|| -> Result<(), Error> {
                if let Some(fields) = &implementation.derive_fields {
                    // Nongeneric derives check their own fields independently.
                    // Only conditional implementations contribute outer bounds.
                    if implementation.type_params.is_empty() {
                        return Ok(());
                    }
                    let field_trait =
                        self.trait_id(&fields.trait_name, scope, implementation.span)?;
                    for field in &fields.fields {
                        let field_ty =
                            self.resolve_scoped(&field.ty, scope, &implementation.type_params)?;
                        let field_ty = super::super::substitute(&field_ty, &substitutions, self)?;
                        self.infer_derive_obligation(
                            &field_ty,
                            field_trait,
                            inference,
                            depth + 1,
                            span,
                        )?;
                    }
                } else {
                    for parameter in &implementation.type_params {
                        let Some(actual) = substitutions.get(&parameter.id) else {
                            return Err(error(
                                ErrorCode::TypeMismatch,
                                "Implementation has an undetermined type parameter",
                            )
                            .with_span(span));
                        };
                        for bound in &parameter.bounds {
                            let bound_id = self.trait_id(&bound.name, scope, bound.span)?;
                            self.infer_derive_obligation(
                                actual,
                                bound_id,
                                inference,
                                depth + 1,
                                span,
                            )?;
                            if bound.equalities.is_empty() {
                                continue;
                            }
                            if let Type::Parameter(p) = actual {
                                if inference.parameters.contains(&p.id) {
                                    let declaration = self
                                        .trait_environment
                                        .declarations
                                        .iter()
                                        .find(|d| d.self_parameter.id == bound_id)
                                        .unwrap();
                                    for equality in &bound.equalities {
                                        let ty = self.resolve_scoped(
                                            &equality.ty,
                                            scope,
                                            &implementation.type_params,
                                        )?;
                                        let ty =
                                            super::super::substitute(&ty, &substitutions, self)?;
                                        let inferred = inference
                                            .bounds
                                            .iter_mut()
                                            .find(|(parameter, inferred)| {
                                                *parameter == p.id
                                                    && inferred.name == declaration.name
                                            })
                                            .unwrap();
                                        if let Some(existing) = inferred
                                            .1
                                            .equalities
                                            .iter()
                                            .find(|e| e.name == equality.name)
                                        {
                                            if existing.ty != ty {
                                                return Err(error(ErrorCode::TypeMismatch, "Conflicting associated-type equalities in derive").with_span(span));
                                            }
                                        } else {
                                            inferred.1.equalities.push(AssociatedEquality {
                                                name: equality.name.clone(),
                                                ty,
                                                span,
                                            });
                                        }
                                    }
                                    continue;
                                }
                            }
                            if !self.bounds_hold(
                                std::slice::from_ref(bound),
                                scope,
                                actual,
                                &substitutions,
                            )? {
                                return Err(error(ErrorCode::TypeMismatch, "derive cannot infer this associated-type equality; write an explicit implementation").with_span(span));
                            }
                        }
                    }
                }
                Ok(())
            })();
            match attempt {
                Ok(()) => {
                    if candidate.replace(inferred).is_some() {
                        return Err(error(
                            ErrorCode::DuplicateDefinition,
                            format!("Overlapping trait implementations for '{ty}'"),
                        )
                        .with_span(span));
                    }
                }
                Err(error) if error.code() == ErrorCode::TypeMismatch => {
                    rejected = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        let Some(selected) = candidate else {
            return Err(rejected.unwrap_or_else(|| {
                error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "Cannot derive: field type '{ty}' does not implement '{}'",
                        declaration.name
                    ),
                )
                .with_span(span)
            }));
        };
        *inference = selected;
        Ok(())
    }
}
