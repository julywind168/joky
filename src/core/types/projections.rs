//! Declaration-scoped associated-type binding and bounded static trait solving.
use super::*;
use crate::core::imports::ImportScope;
use crate::parser::ast::{ImplDef, TraitDef};
use std::{cell::RefCell, rc::Rc, sync::Arc};

mod dyn_traits;
mod json_derive;

type ParameterScopes = BTreeMap<u64, (Arc<TypeParameter>, Arc<ImportScope>)>;

#[derive(Clone, Debug, Default)]
pub(super) struct Environment {
    // Scope changes share immutable declarations instead of cloning method bodies.
    declarations: Rc<[TraitDef]>,
    implementations: Rc<[ImplDef]>,
    parameters: Rc<ParameterScopes>,
    root: Arc<ImportScope>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Obligation {
    DynQualification(Type),
    Normalize(TypeProjection),
    Binding(TypeProjection),
    Implements(Type, u64),
    Alias(String, Vec<GenericArgument>),
}
#[derive(Clone, Debug)]
enum Answer {
    Type(Type),
    Bool(bool),
}
#[derive(Debug, Default)]
pub(super) struct Work {
    active: Vec<Obligation>,
    visited: Vec<Obligation>,
    heights: Vec<usize>,
    last_height: usize,
    answers: Vec<(Obligation, Answer, usize)>,
}

impl TypeRegistry {
    pub(crate) fn standard_success_variant(&self, id: u64) -> Option<&'static str> {
        self.trait_environment.root.standard_enums.get(&id).copied()
    }

    pub(super) fn solving_traits(&self) -> bool {
        !self.trait_work.borrow().active.is_empty()
    }

    pub(super) fn has_visible_forward_type(&self, name: &str) -> bool {
        (!name.contains('.') || self.trait_environment.root.types.contains_key(name))
            && self.forwards.borrow().contains_key(name)
    }

    fn cached(&self, key: &Obligation, span: Span, entered: bool) -> Result<Option<Answer>, Error> {
        let mut work = self.trait_work.borrow_mut();
        if work.active.is_empty() {
            return Ok(None);
        }
        let Some((_, answer, height)) = work
            .answers
            .iter()
            .find(|(saved, _, _)| saved == key)
            .cloned()
        else {
            return Ok(None);
        };
        if work.active.len() - usize::from(entered) + height > 32 {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Trait and projection solving exceeds 32 dependency levels",
            )
            .with_span(span));
        }
        if let Some(parent) = work.heights.last_mut() {
            *parent = (*parent).max(height + usize::from(!entered));
        }
        Ok(Some(answer))
    }
    fn save(&self, key: Obligation, answer: Answer) {
        let mut work = self.trait_work.borrow_mut();
        let height = work.last_height;
        work.answers.push((key, answer, height));
    }

    pub(super) fn with_alias_obligation<T>(
        &self,
        name: &str,
        arguments: &[GenericArgument],
        f: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.trait_work.borrow().active.is_empty() {
            return f();
        }
        self.obligation(
            Obligation::Alias(name.to_owned(), arguments.to_vec()),
            Span::empty(0),
            f,
        )
    }

    pub(crate) fn validate_associated_declarations(&self) -> Result<(), Error> {
        for declaration in self.trait_environment.declarations.iter() {
            let mut names = BTreeSet::new();
            let scope = declaration
                .scope
                .as_ref()
                .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
            for associated in &declaration.associated_types {
                if !names.insert(&associated.name) {
                    return Err(
                        error(ErrorCode::DuplicateDefinition, "Duplicate associated type")
                            .with_span(associated.span),
                    );
                }
                if matches!(
                    associated.name.as_str(),
                    "Self" | "Copy" | "Fn" | "FnMut" | "FnOnce" | "_"
                ) {
                    return Err(
                        error(ErrorCode::ReservedName, "Reserved associated type name")
                            .with_span(associated.span),
                    );
                }
                self.validate_bounds(&associated.bounds, scope)?;
            }
        }
        for (parameter, scope) in self.trait_environment.parameters.values() {
            self.validate_bounds(&parameter.bounds, scope)?;
        }
        for implementation in self.trait_environment.implementations.iter() {
            let Some(name) = &implementation.trait_name else {
                if !implementation.associated_types.is_empty() {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        "Inherent associated types are not supported",
                    )
                    .with_span(implementation.span));
                }
                continue;
            };
            let scope = implementation
                .scope
                .as_ref()
                .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
            let id = self.trait_id(name, scope, implementation.span)?;
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .ok_or_else(|| {
                    error(ErrorCode::UnknownType, "Unknown trait identity")
                        .with_span(implementation.span)
                })?;
            let mut names = BTreeSet::new();
            for associated in &implementation.associated_types {
                if !names.insert(&associated.name) {
                    return Err(error(
                        ErrorCode::DuplicateDefinition,
                        "Duplicate associated type definition",
                    )
                    .with_span(associated.span));
                }
                if !declaration
                    .associated_types
                    .iter()
                    .any(|a| a.name == associated.name)
                {
                    return Err(error(
                        ErrorCode::UnknownName,
                        format!(
                            "Trait '{name}' has no associated type '{}'",
                            associated.name
                        ),
                    )
                    .with_span(associated.span));
                }
            }
            for associated in &declaration.associated_types {
                if !names.contains(&associated.name) {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        format!("Missing associated type '{}'", associated.name),
                    )
                    .with_span(implementation.span));
                }
            }
        }
        Ok(())
    }

    fn validate_bounds(&self, bounds: &[TraitBound], scope: &ImportScope) -> Result<(), Error> {
        let mut ids = BTreeSet::new();
        for bound in bounds {
            let id = self.trait_id(&bound.name, scope, bound.span)?;
            if !ids.insert(id) {
                return Err(
                    error(ErrorCode::DuplicateDefinition, "Duplicate trait bound")
                        .with_span(bound.span),
                );
            }
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .ok_or_else(|| {
                    error(ErrorCode::UnknownType, "Unknown trait identity").with_span(bound.span)
                })?;
            let mut names = BTreeSet::new();
            for equality in &bound.equalities {
                if !names.insert(&equality.name) {
                    return Err(error(
                        ErrorCode::DuplicateDefinition,
                        "Duplicate associated equality",
                    )
                    .with_span(equality.span));
                }
                if !declaration
                    .associated_types
                    .iter()
                    .any(|a| a.name == equality.name)
                {
                    return Err(error(
                        ErrorCode::UnknownName,
                        format!(
                            "Trait '{}' has no associated type '{}'",
                            declaration.name, equality.name
                        ),
                    )
                    .with_span(equality.span));
                }
                if equality.ty == Type::Named("_".into()) {
                    return Err(error(
                        ErrorCode::UnknownType,
                        "Associated equalities require a determined type",
                    )
                    .with_span(equality.span));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_associated_implementations(&self) -> Result<(), Error> {
        for (parameter, scope) in self.trait_environment.parameters.values() {
            for bound in &parameter.bounds {
                for equality in &bound.equalities {
                    let ty = self
                        .resolve_scoped(&equality.ty, scope, std::slice::from_ref(parameter))
                        .map_err(|e| e.with_span(equality.span))?;
                    let id = self.trait_id(&bound.name, scope, bound.span)?;
                    let declaration = self
                        .trait_environment
                        .declarations
                        .iter()
                        .find(|d| d.self_parameter.id == id)
                        .unwrap();
                    let associated = declaration
                        .associated_types
                        .iter()
                        .find(|item| item.name == equality.name)
                        .unwrap();
                    let declaration_scope = declaration
                        .scope
                        .as_ref()
                        .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
                    if !self.bounds_hold(
                        &associated.bounds,
                        declaration_scope,
                        &ty,
                        &Substitutions::default(),
                    )? {
                        return Err(error(
                            ErrorCode::TypeMismatch,
                            "Associated equality does not satisfy the item's declared capabilities",
                        )
                        .with_span(equality.span));
                    }
                }
            }
        }
        for implementation in self.trait_environment.implementations.iter() {
            let Some(name) = &implementation.trait_name else {
                continue;
            };
            let scope = implementation
                .scope
                .as_ref()
                .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
            let id = self.trait_id(name, scope, implementation.span)?;
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .unwrap();
            for (index, associated) in declaration.associated_types.iter().enumerate() {
                let target = self.resolve_scoped(
                    &implementation.target,
                    scope,
                    &implementation.type_params,
                )?;
                let projection = TypeProjection {
                    base: target,
                    trait_name: Some(name.clone()),
                    name: associated.name.clone(),
                    identity: Some((id, index)),
                    span: implementation.span,
                };
                let local = self.with_parameters(&implementation.type_params)?;
                let ty = local.normalize_projection(&projection, &HashMap::new())?;
                let declaration_scope = declaration
                    .scope
                    .as_ref()
                    .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
                if !self.bounds_hold(
                    &associated.bounds,
                    declaration_scope,
                    &ty,
                    &Substitutions::default(),
                )? {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "Associated type '{}' does not satisfy its declared capabilities",
                            associated.name
                        ),
                    )
                    .with_span(implementation.span));
                }
            }
        }
        Ok(())
    }

    pub(super) fn trait_parameter(&self, id: u64) -> Option<Arc<TypeParameter>> {
        self.trait_environment
            .parameters
            .get(&id)
            .map(|(p, _)| p.clone())
    }

    pub(crate) fn with_trait_environment(&self, module: &Module) -> Self {
        let root = module.scope.clone().unwrap_or_default();
        let mut parameters = BTreeMap::new();
        for declaration in &module.traits {
            let scope = declaration
                .scope
                .as_ref()
                .map_or_else(|| root.clone(), |s| s.imports.clone());
            parameters.insert(
                declaration.self_parameter.id,
                (declaration.self_parameter.clone(), scope),
            );
        }
        for implementation in &module.implementations {
            let scope = implementation
                .scope
                .as_ref()
                .map_or_else(|| root.clone(), |s| s.imports.clone());
            for p in &implementation.type_params {
                parameters.insert(p.id, (p.clone(), scope.clone()));
            }
            for method in &implementation.methods {
                for p in &method.type_params {
                    parameters.insert(p.id, (p.clone(), scope.clone()));
                }
            }
        }
        for item in &module.items {
            if let Item::FnDef(f) = item {
                let scope = f
                    .scope
                    .as_ref()
                    .map_or_else(|| root.clone(), |s| s.imports.clone());
                for p in &f.type_params {
                    parameters.insert(p.id, (p.clone(), scope.clone()));
                }
            }
        }
        let mut local = self.clone();
        local.trait_environment = Rc::new(Environment {
            declarations: module.traits.clone().into(),
            implementations: module.implementations.clone().into(),
            parameters: Rc::new(parameters),
            root,
        });
        local.trait_work = Rc::new(RefCell::new(Work::default()));
        local
    }

    fn obligation<T>(
        &self,
        key: Obligation,
        span: Span,
        f: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        {
            let mut work = self.trait_work.borrow_mut();
            if work.active.is_empty() {
                work.visited.clear();
                work.answers.clear();
            }
            let binding = matches!(key, Obligation::Binding(_));
            if work.active.len() > 32
                || (!binding && !work.visited.contains(&key) && work.visited.len() >= 256)
            {
                return Err(error(
                    ErrorCode::ResourceLimit,
                    "Trait and projection solving exceeds 32 dependency levels or 256 obligations",
                )
                .with_span(span));
            }
            if work.active.contains(&key) {
                let chain = work
                    .active
                    .iter()
                    .chain(std::iter::once(&key))
                    .map(|o| match o {
                        Obligation::Normalize(p) | Obligation::Binding(p) => {
                            Type::Projection(Arc::new(p.clone())).to_string()
                        }
                        Obligation::Implements(t, id) => format!("{t}: trait#{id}"),
                        Obligation::Alias(name, _) => format!("alias {name}"),
                        Obligation::DynQualification(ty) => format!("qualify {ty}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(error(
                    ErrorCode::RecursiveType,
                    format!("Cyclic trait or projection dependency: {chain}"),
                )
                .with_span(span));
            }
            if !binding && !work.visited.contains(&key) {
                work.visited.push(key.clone());
            }
            work.active.push(key);
            work.heights.push(0);
        }
        let result = f();
        let mut work = self.trait_work.borrow_mut();
        work.active.pop();
        let height = work.heights.pop().unwrap();
        if let Some(parent) = work.heights.last_mut() {
            *parent = (*parent).max(height + 1);
        }
        work.last_height = height;
        result
    }

    fn trait_id(&self, name: &str, scope: &ImportScope, span: Span) -> Result<u64, Error> {
        scope.traits.get(name).map(|b| b.id).ok_or_else(|| {
            error(
                ErrorCode::UnknownType,
                format!("Unknown trait bound '{name}'"),
            )
            .with_span(span)
        })
    }

    fn scope_types(&self, scope: &ImportScope) -> HashMap<String, TypeDefinition> {
        let definitions = self.forwards.borrow();
        let mut canonical: HashMap<_, _> = definitions
            .iter()
            .filter(|(name, _)| !name.contains('.') || scope.types.contains_key(*name))
            .map(|(name, definition)| (name.clone(), definition.clone()))
            .collect();
        for (name, binding) in &scope.types {
            if let Some(definition) = &binding.resolved {
                canonical.insert(name.clone(), definition.clone());
            } else if let Some(definition) = definitions.get(&binding.name).cloned() {
                canonical.insert(name.clone(), definition);
            }
        }
        canonical
    }

    fn resolve_scoped(
        &self,
        ty: &Type,
        scope: &ImportScope,
        parameters: &[Arc<TypeParameter>],
    ) -> Result<Type, Error> {
        let mut local = self.clone();
        for p in parameters {
            local.parameters.insert(p.id, p.name.clone());
        }
        Rc::make_mut(&mut local.trait_environment).root = Arc::new(scope.clone());
        let definitions = local.scope_types(scope);
        let canonical = definitions
            .iter()
            .map(|(n, d)| (n.as_str(), d.clone()))
            .collect();
        let mut ty = ty.clone();
        for node in TypeWalk::new(&ty, TypeEdges::Arguments) {
            if let Type::Parameter(p) = node {
                if self.trait_environment.parameters.contains_key(&p.id) {
                    local.parameters.insert(p.id, p.name.clone());
                }
            }
        }
        resolve_type(&mut ty, &canonical, &local)?;
        local.validate(&ty)?;
        Ok(ty)
    }

    pub(crate) fn normalize_projection(
        &self,
        projection: &TypeProjection,
        aliases: &HashMap<&str, TypeDefinition>,
    ) -> Result<Type, Error> {
        self.obligation(
            Obligation::Binding(projection.clone()),
            projection.span,
            || self.normalize_projection_inner(projection, aliases),
        )
    }

    fn normalize_projection_inner(
        &self,
        projection: &TypeProjection,
        aliases: &HashMap<&str, TypeDefinition>,
    ) -> Result<Type, Error> {
        let mut p = projection.clone();
        let definitions = self.scope_types(&self.trait_environment.root);
        let mut base_aliases: HashMap<_, _> = definitions
            .iter()
            .filter(|(name, _)| {
                !name.contains('.') || self.trait_environment.root.types.contains_key(*name)
            })
            .map(|(name, definition)| (name.as_str(), definition.clone()))
            .collect();
        base_aliases.extend(
            aliases
                .iter()
                .map(|(name, definition)| (*name, definition.clone())),
        );
        resolve_type(&mut p.base, &base_aliases, self)?;
        self.validate(&p.base).map_err(|e| e.with_span(p.span))?;
        let scope = if let Type::Parameter(parameter) = &p.base {
            self.trait_environment
                .parameters
                .get(&parameter.id)
                .map(|(_, s)| s.as_ref())
                .unwrap_or(&self.trait_environment.root)
        } else {
            &self.trait_environment.root
        };
        if p.identity.is_none() {
            let candidates: Vec<_> = if let Some(name) = &p.trait_name {
                vec![self.trait_id(name, scope, p.span)?]
            } else {
                self.available_traits(&p.base)?
                    .into_iter()
                    .filter(|id| {
                        self.trait_environment.declarations.iter().any(|d| {
                            d.self_parameter.id == *id
                                && d.associated_types.iter().any(|a| a.name == p.name)
                        })
                    })
                    .collect()
            };
            if candidates.len() != 1 {
                return Err(error(ErrorCode::TypeMismatch, format!("Associated type '{}.{}' requires one unambiguous trait ({} candidates); use <Type as Trait>::{}", p.base, p.name, candidates.len(), p.name)).with_span(p.span));
            }
            let id = candidates[0];
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .ok_or_else(|| error(ErrorCode::UnknownType, "Unknown trait identity"))?;
            let index = declaration
                .associated_types
                .iter()
                .position(|a| a.name == p.name)
                .ok_or_else(|| {
                    error(
                        ErrorCode::UnknownName,
                        format!(
                            "Trait '{}' has no associated type '{}'",
                            declaration.name, p.name
                        ),
                    )
                    .with_span(p.span)
                })?;
            p.identity = Some((id, index));
            p.trait_name = Some(declaration.name.clone());
        }
        let (id, index) = p.identity.unwrap();
        let declaration = self
            .trait_environment
            .declarations
            .iter()
            .find(|d| d.self_parameter.id == id)
            .ok_or_else(|| {
                error(
                    ErrorCode::UnknownType,
                    "Unknown trait identity in projection",
                )
                .with_span(p.span)
            })?;
        if !declaration
            .associated_types
            .get(index)
            .is_some_and(|item| item.name == p.name)
        {
            return Err(error(
                ErrorCode::UnknownName,
                "Unknown associated type identity in projection",
            )
            .with_span(p.span));
        }
        let key = Obligation::Normalize(p.clone());
        {
            let mut work = self.trait_work.borrow_mut();
            let last = work.active.len() - 1;
            if work.active[..last].contains(&key) {
                let path = work.active[..last]
                    .iter()
                    .chain(std::iter::once(&key))
                    .map(|key| match key {
                        Obligation::Normalize(p) | Obligation::Binding(p) => {
                            Type::Projection(Arc::new(p.clone())).to_string()
                        }
                        Obligation::Implements(ty, id) => format!("{ty}: trait#{id}"),
                        Obligation::Alias(name, _) => format!("alias {name}"),
                        Obligation::DynQualification(ty) => format!("qualify {ty}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(error(
                    ErrorCode::RecursiveType,
                    format!("Cyclic projection dependency: {path}"),
                )
                .with_span(p.span));
            }
            if !work.visited.contains(&key) {
                if work.visited.len() >= 256 {
                    return Err(error(
                        ErrorCode::ResourceLimit,
                        "Trait and projection solving exceeds 256 obligations",
                    )
                    .with_span(p.span));
                }
                work.visited.push(key.clone());
            }
            work.active[last] = key.clone();
        }
        if let Some(Answer::Type(ty)) = self.cached(&key, p.span, true)? {
            return Ok(ty);
        }
        let result = (|| {
            let (id, index) = p.identity.unwrap();
            if let Some(parameter) = match &p.base {
                Type::Parameter(parameter) if self.direct_traits(&p.base)?.contains(&id) => {
                    Some(parameter)
                }
                _ => None,
            } {
                if !self.implements(&p.base, id)? {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        "Projection requires a declared trait capability",
                    )
                    .with_span(p.span));
                }
                if let Some((parameter, scope)) =
                    self.trait_environment.parameters.get(&parameter.id)
                {
                    for bound in &parameter.bounds {
                        if self.trait_id(&bound.name, scope, bound.span)? != id {
                            continue;
                        }
                        for equality in &bound.equalities {
                            if equality.name != p.name {
                                continue;
                            }
                            if let Type::Projection(rhs) = &equality.ty {
                                if rhs.base == p.base
                                    && rhs.name == p.name
                                    && (rhs.identity == p.identity
                                        || rhs.identity.is_none()
                                            && rhs.trait_name.as_ref().is_none_or(|name| {
                                                self.trait_id(name, scope, rhs.span).ok()
                                                    == Some(id)
                                            }))
                                {
                                    return Ok(Type::Projection(Arc::new(p.clone())));
                                }
                            }
                            let ty = self.resolve_scoped(
                                &equality.ty,
                                scope,
                                std::slice::from_ref(parameter),
                            )?;
                            if matches!(&ty, Type::Projection(other) if other.as_ref() == &p) {
                                return Ok(ty);
                            }
                            return Ok(ty);
                        }
                    }
                }
                return Ok(Type::Projection(Arc::new(p.clone())));
            }
            if matches!(p.base, Type::Projection(_)) && self.direct_traits(&p.base)?.contains(&id) {
                if !self.implements(&p.base, id)? {
                    return Err(error(
                        ErrorCode::TypeMismatch,
                        "Projection requires a declared trait capability",
                    )
                    .with_span(p.span));
                }
                return Ok(Type::Projection(Arc::new(p.clone())));
            }
            let mut selected = None;
            for implementation in self.trait_environment.implementations.iter() {
                let scope = implementation
                    .scope
                    .as_ref()
                    .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
                let Some(name) = &implementation.trait_name else {
                    continue;
                };
                if self.trait_id(name, scope, implementation.span)? != id {
                    continue;
                }
                if let Some(substitutions) = self.match_impl(implementation, &p.base)? {
                    if selected.is_some() {
                        return Err(error(
                            ErrorCode::DuplicateDefinition,
                            "Overlapping trait implementations",
                        )
                        .with_span(p.span));
                    }
                    selected = Some((implementation, substitutions));
                }
            }
            let (implementation, substitutions) = selected.ok_or_else(|| {
                error(
                    ErrorCode::TypeMismatch,
                    format!("Type '{}' does not implement projection trait", p.base),
                )
                .with_span(p.span)
            })?;
            let scope = implementation
                .scope
                .as_ref()
                .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .unwrap();
            let name = &declaration.associated_types[index].name;
            let binding = implementation
                .associated_types
                .iter()
                .find(|a| &a.name == name)
                .ok_or_else(|| {
                    error(
                        ErrorCode::TypeMismatch,
                        format!("Missing associated type '{name}'"),
                    )
                    .with_span(implementation.span)
                })?;
            let ty = self
                .resolve_scoped(&binding.ty, scope, &implementation.type_params)
                .map_err(|e| {
                    if e.span().is_none_or(|span| span.start() == span.end()) {
                        e.with_span(binding.target_span)
                    } else {
                        e
                    }
                })?;
            substitute(&ty, &substitutions, self)
        })()?;
        {
            let mut work = self.trait_work.borrow_mut();
            let height = *work.heights.last().unwrap();
            work.answers
                .push((key, Answer::Type(result.clone()), height));
        }
        Ok(result)
    }

    fn direct_traits(&self, ty: &Type) -> Result<Vec<u64>, Error> {
        if let Type::Parameter(p) = ty {
            if self
                .trait_environment
                .declarations
                .iter()
                .any(|d| d.self_parameter.id == p.id)
            {
                return Ok(vec![p.id]);
            }
            let Some((p, scope)) = self.trait_environment.parameters.get(&p.id) else {
                return Ok(Vec::new());
            };
            return p
                .bounds
                .iter()
                .map(|b| self.trait_id(&b.name, scope, b.span))
                .collect();
        }
        if let Type::Projection(p) = ty {
            let Some((id, index)) = p.identity else {
                return Ok(Vec::new());
            };
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .ok_or_else(|| {
                    error(
                        ErrorCode::UnknownType,
                        "Unknown trait identity in projection",
                    )
                    .with_span(p.span)
                })?;
            let scope = declaration
                .scope
                .as_ref()
                .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
            return declaration
                .associated_types
                .get(index)
                .ok_or_else(|| {
                    error(ErrorCode::UnknownName, "Unknown associated type identity")
                        .with_span(p.span)
                })?
                .bounds
                .iter()
                .map(|b| self.trait_id(&b.name, scope, b.span))
                .collect();
        }
        Ok(Vec::new())
    }

    pub(crate) fn available_traits(&self, ty: &Type) -> Result<Vec<u64>, Error> {
        let mut result = self.direct_traits(ty)?;
        for binding in self.trait_environment.root.traits.values() {
            if !result.contains(&binding.id) && self.implements(ty, binding.id)? {
                result.push(binding.id);
            }
        }
        Ok(result)
    }

    pub(crate) fn implements(&self, ty: &Type, id: u64) -> Result<bool, Error> {
        if ty.contains_dyn() {
            return Ok(false);
        }
        let key = Obligation::Implements(ty.clone(), id);
        if let Some(Answer::Bool(value)) = self.cached(&key, Span::empty(0), false)? {
            return Ok(value);
        }
        let result = self.obligation(key.clone(), Span::empty(0), || {
            if matches!(ty, Type::Parameter(_) | Type::Projection(_))
                && self.direct_traits(ty)?.contains(&id)
            {
                return Ok(true);
            }
            let mut matches = 0;
            for implementation in self.trait_environment.implementations.iter() {
                let scope = implementation
                    .scope
                    .as_ref()
                    .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
                let Some(name) = &implementation.trait_name else {
                    continue;
                };
                if self.trait_id(name, scope, implementation.span)? == id
                    && self.match_impl(implementation, ty)?.is_some()
                {
                    matches += 1;
                }
            }
            if matches > 1 {
                return Err(error(
                    ErrorCode::DuplicateDefinition,
                    format!("Overlapping trait implementations for '{ty}'"),
                ));
            }
            Ok(matches == 1)
        })?;
        self.save(key, Answer::Bool(result));
        Ok(result)
    }

    fn match_impl(
        &self,
        implementation: &ImplDef,
        actual: &Type,
    ) -> Result<Option<Substitutions>, Error> {
        let scope = implementation
            .scope
            .as_ref()
            .map_or(self.trait_environment.root.as_ref(), |s| s.imports.as_ref());
        let target =
            self.resolve_scoped(&implementation.target, scope, &implementation.type_params)?;
        let same = match (&target, actual) {
            (Type::Parameter(_), _) => true,
            (Type::Record(a), Type::Record(b)) => a.id == b.id,
            (Type::Enum(a), Type::Enum(b)) => a.id == b.id,
            _ => target == *actual,
        };
        if !same {
            return Ok(None);
        }
        let mut substitutions = Substitutions::default();
        super::unify(
            &target,
            actual,
            &implementation.type_params,
            &mut substitutions,
            implementation.span,
        )?;
        for p in &implementation.type_params {
            let Some(actual) = substitutions.get(&p.id) else {
                return Ok(None);
            };
            if (p.copy && !actual.is_copy_storage_value())
                || !self.bounds_hold(&p.bounds, scope, actual, &substitutions)?
            {
                return Ok(None);
            }
        }
        Ok(Some(substitutions))
    }

    fn bounds_hold(
        &self,
        bounds: &[TraitBound],
        scope: &ImportScope,
        actual: &Type,
        substitutions: &Substitutions,
    ) -> Result<bool, Error> {
        for bound in bounds {
            let id = self.trait_id(&bound.name, scope, bound.span)?;
            if !self.implements(actual, id)? {
                return Ok(false);
            }
            let declaration = self
                .trait_environment
                .declarations
                .iter()
                .find(|d| d.self_parameter.id == id)
                .unwrap();
            for equality in &bound.equalities {
                let index = declaration
                    .associated_types
                    .iter()
                    .position(|a| a.name == equality.name)
                    .ok_or_else(|| {
                        error(
                            ErrorCode::UnknownName,
                            format!("Unknown associated type '{}'", equality.name),
                        )
                        .with_span(equality.span)
                    })?;
                let projection = TypeProjection {
                    base: actual.clone(),
                    trait_name: Some(declaration.name.clone()),
                    name: equality.name.clone(),
                    identity: Some((id, index)),
                    span: equality.span,
                };
                // Trait checks also run from the module-wide method registry.
                // Preserve registered caller parameters while checking a conditional
                // implementation's associated equality on a rigid argument.
                let mut local = self.clone();
                for ty in TypeWalk::new(actual, TypeEdges::Arguments) {
                    if let Type::Parameter(p) = ty {
                        if self.trait_environment.parameters.contains_key(&p.id) {
                            local.parameters.insert(p.id, p.name.clone());
                        }
                    }
                }
                let actual = local.normalize_projection(&projection, &HashMap::new())?;
                let expected = self.resolve_scoped(&equality.ty, scope, &[])?;
                let expected = substitute(&expected, substitutions, self)?;
                if actual != expected {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub(crate) fn parameter_bounds_hold(
        &self,
        parameter: &TypeParameter,
        actual: &Type,
        substitutions: &Substitutions,
    ) -> Result<bool, Error> {
        let scope = self
            .trait_environment
            .parameters
            .get(&parameter.id)
            .map_or(self.trait_environment.root.as_ref(), |(_, scope)| {
                scope.as_ref()
            });
        Ok((!parameter.copy || actual.is_copy_storage_value())
            && self.bounds_hold(&parameter.bounds, scope, actual, substitutions)?)
    }

    pub(crate) fn check_associated_bounds(
        &self,
        parameter: &TypeParameter,
        actual: &Type,
        substitutions: &Substitutions,
        span: Span,
    ) -> Result<(), Error> {
        if parameter.bounds.iter().all(|b| b.equalities.is_empty()) {
            return Ok(());
        }
        let scope = self
            .trait_environment
            .parameters
            .get(&parameter.id)
            .map_or(self.trait_environment.root.as_ref(), |(_, s)| s.as_ref());
        if !self.bounds_hold(&parameter.bounds, scope, actual, substitutions)? {
            return Err(error(ErrorCode::TypeMismatch,format!("Type '{actual}' does not satisfy trait bounds or associated equalities for '{}'",parameter.name)).with_span(span));
        }
        Ok(())
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::TypeCheck, code, message)
}
