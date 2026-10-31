//! Static trait identity, requirement validation and bounded proof search.
use super::*;
use crate::core::imports::ImportScope;
use crate::core::types::substitute;
use crate::core::types::TypeParameter;
use crate::parser::ast::TraitDef;
use members::{same_owner, Members, Method, Receiver};
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Traits {
    pub definitions: Vec<TraitDef>,
    projection_types: TypeRegistry,
    pub bounds: HashMap<u64, Vec<u64>>,
    pub requirements: HashSet<String>,
    implementations: Vec<Implementation>,
}

struct Implementation {
    id: u64,
    target: Type,
    parameters: Vec<Arc<TypeParameter>>,
}

impl Traits {
    fn register(
        &mut self,
        parameters: &[Arc<TypeParameter>],
        scope: &ImportScope,
    ) -> Result<(), Error> {
        for p in parameters {
            let mut ids = Vec::new();
            for bound in &p.bounds {
                let binding = scope.traits.get(&bound.name).ok_or_else(|| {
                    type_error(
                        &p.span,
                        ErrorCode::UnknownType,
                        format!("Unknown trait bound '{}'", bound.name),
                    )
                })?;
                if !self
                    .definitions
                    .iter()
                    .any(|d| d.self_parameter.id == binding.id)
                {
                    return Err(type_error(
                        &p.span,
                        ErrorCode::UnknownType,
                        "Unknown trait identity in bound",
                    ));
                }
                if ids.contains(&binding.id) {
                    return Err(type_error(
                        &p.span,
                        ErrorCode::DuplicateDefinition,
                        "Duplicate trait bound",
                    ));
                }
                ids.push(binding.id);
            }
            self.bounds.insert(p.id, ids);
        }
        Ok(())
    }

    pub fn satisfies(&self, ty: &Type, id: u64) -> Result<bool, Error> {
        self.projection_types.implements(ty, id)
    }

    fn applies(&self, implementation: &Implementation, ty: &Type) -> Result<bool, Error> {
        if !matches!(implementation.target, Type::Parameter(_))
            && !same_owner(&implementation.target, ty)
        {
            return Ok(false);
        }
        let mut substitutions = crate::core::types::Substitutions::default();
        generics::unify(
            &implementation.target,
            ty,
            &implementation.parameters,
            &mut substitutions,
            Span::empty(0),
        )?;
        for p in &implementation.parameters {
            let Some(actual) = substitutions.get(&p.id) else {
                return Ok(false);
            };
            if !self
                .projection_types
                .parameter_bounds_hold(p, actual, &substitutions)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn check_bounds(&self, p: &TypeParameter, actual: &Type, span: Span) -> Result<(), Error> {
        if p.copy && !actual.is_copy_argument() {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                format!(
                    "Type '{actual}' does not satisfy Copy bound for '{}'",
                    p.name
                ),
            ));
        }
        for &id in self.bounds.get(&p.id).into_iter().flatten() {
            if !self.satisfies(actual, id).map_err(|e| e.with_span(span))? {
                let name = &self
                    .definitions
                    .iter()
                    .find(|d| d.self_parameter.id == id)
                    .unwrap()
                    .name;
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Type '{actual}' does not satisfy trait bound '{name}' for '{}'",
                        p.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn receiver(definition: &FnDef) -> Option<Receiver> {
    definition
        .params
        .first()
        .filter(|p| p.name == "self")
        .map(|p| {
            p.ty.reference_type()
                .map_or(Receiver::Value, |r| Receiver::Borrowed {
                    mutable: r.mutable,
                })
        })
}

pub(super) fn collect(
    members: &mut Members,
    module: &mut AstModule,
    types: &TypeRegistry,
) -> Result<(), Error> {
    let root = module.scope.clone().unwrap_or_default();
    members.traits.definitions = module.traits.clone();
    members.traits.projection_types = types.clone();
    let mut identities = HashSet::new();
    let mut declaration_names = HashSet::new();
    for declaration in &module.traits {
        let id = declaration.self_parameter.id;
        if !identities.insert(id) || !declaration_names.insert(&declaration.name) {
            return Err(type_error(
                &declaration.span,
                ErrorCode::DuplicateDefinition,
                "Duplicate trait declaration",
            ));
        }
        members.traits.bounds.insert(id, vec![id]);
        if declaration.is_drop
            && (!declaration.associated_types.is_empty()
                || declaration
                    .methods
                    .iter()
                    .any(|method| method.body.is_some()))
        {
            return Err(type_error(
                &declaration.span,
                ErrorCode::TypeMismatch,
                "The built-in Drop protocol requires its original abstract drop method",
            ));
        }
        let mut names = HashSet::new();
        for requirement in &declaration.methods {
            if !names.insert(&requirement.name) {
                return Err(type_error(
                    &requirement.span,
                    ErrorCode::DuplicateDefinition,
                    "Duplicate trait method",
                ));
            }
            let mut params = HashSet::new();
            if requirement.params.iter().any(|p| !params.insert(&p.name)) {
                return Err(type_error(
                    &requirement.span,
                    ErrorCode::DuplicateDefinition,
                    "Duplicate trait method parameter",
                ));
            }
            let mut definition = requirement.definition();
            definition.name = format!("joky/trait/{id}/{}", requirement.name);
            definition.type_params = vec![declaration.self_parameter.clone()];
            definition.scope = declaration.scope.clone();
            members.traits.requirements.insert(definition.name.clone());
            members.functions.insert(definition.name.clone());
            members.methods.push(Method {
                target: Type::Parameter(declaration.self_parameter.clone()),
                trait_id: Some(id),
                name: requirement.name.clone(),
                function: definition.name.clone(),
                receiver: receiver(&requirement.definition()),
                impl_parameters: 1,
                method_parameters: 0,
            });
            if requirement.body.is_some() {
                let mut default = definition.clone();
                default.name = format!("joky/default/{id}/{}", requirement.name);
                // Abstract dispatch always uses the declaration contract, never inferred effects.
                members.functions.insert(default.name.clone());
                module.items.push(Item::FnDef(default));
            }
            module.items.push(Item::FnDef(definition));
        }
    }
    for implementation in &module.implementations {
        let scope = implementation
            .scope
            .as_ref()
            .map_or(root.as_ref(), |s| s.imports.as_ref());
        members
            .traits
            .register(&implementation.type_params, scope)?;
        for method in &implementation.methods {
            members.traits.register(&method.type_params, scope)?;
        }
    }
    for item in &module.items {
        if let Item::FnDef(f) = item {
            if !members.traits.requirements.contains(&f.name)
                && !f.name.starts_with("joky/default/")
            {
                let scope = f
                    .scope
                    .as_ref()
                    .map_or(root.as_ref(), |s| s.imports.as_ref());
                members.traits.register(&f.type_params, scope)?;
            }
        }
    }
    for implementation in &module.implementations {
        let Some(name) = &implementation.trait_name else {
            continue;
        };
        let scope = implementation
            .scope
            .as_ref()
            .map_or(root.as_ref(), |s| s.imports.as_ref());
        let binding = scope.traits.get(name).ok_or_else(|| {
            type_error(
                &implementation.span,
                ErrorCode::UnknownType,
                format!("Unknown trait '{name}'"),
            )
        })?;
        let declaration = members
            .traits
            .definitions
            .iter()
            .find(|d| d.self_parameter.id == binding.id)
            .ok_or_else(|| {
                type_error(
                    &implementation.span,
                    ErrorCode::UnknownType,
                    "Unknown trait identity",
                )
            })?;
        if declaration.is_drop
            && (!matches!(implementation.target, Type::Record(_))
                || !implementation.target.is_owner()
                || implementation
                    .type_params
                    .iter()
                    .any(|p| !p.bounds.is_empty()))
        {
            return Err(type_error(
                &implementation.span,
                ErrorCode::TypeMismatch,
                "Drop requires an owning record and cannot have conditional bounds",
            ));
        }
        let target_owner = implementation
            .target
            .nominal()
            .and_then(|(name, _)| name.rsplit_once('.').map(|(owner, _)| owner));
        let trait_owner = binding.name.rsplit_once('.').map(|(owner, _)| owner);
        let local_type = implementation.target.nominal().is_some()
            && target_owner == implementation.origin.as_deref();
        if trait_owner != implementation.origin.as_deref() && !local_type {
            return Err(type_error(
                &implementation.span,
                ErrorCode::TypeMismatch,
                "A trait implementation requires a trait or nominal type defined in this module",
            ));
        }
        if members.traits.implementations.iter().any(|i| {
            i.id == binding.id
                && (same_owner(&i.target, &implementation.target)
                    || matches!(
                        (&i.target, &implementation.target),
                        (Type::Parameter(_), Type::Parameter(_))
                    ))
        }) {
            return Err(type_error(
                &implementation.span,
                ErrorCode::DuplicateDefinition,
                "Overlapping trait implementation",
            ));
        }
        if !implementation.constants.is_empty() {
            return Err(type_error(
                &implementation.span,
                ErrorCode::TypeMismatch,
                "Trait implementations cannot define associated constants",
            ));
        }
        let substitutions = [(declaration.self_parameter.id, implementation.target.clone())]
            .into_iter()
            .collect::<crate::core::types::Substitutions>();
        let local = types.with_parameters(&implementation.type_params)?;
        let mut names = HashSet::new();
        for definition in &implementation.methods {
            if !names.insert(&definition.name) {
                return Err(type_error(
                    &definition.span,
                    ErrorCode::DuplicateDefinition,
                    "Duplicate trait implementation method",
                ));
            }
            let requirement = declaration
                .methods
                .iter()
                .find(|m| m.name == definition.name)
                .ok_or_else(|| {
                    type_error(
                        &definition.span,
                        ErrorCode::UnknownName,
                        format!("Trait '{name}' has no method '{}'", definition.name),
                    )
                })?;
            let params = requirement
                .params
                .iter()
                .map(|p| substitute(&p.ty, &substitutions, &local))
                .collect::<Result<Vec<_>, _>>()?;
            let ret = substitute(&requirement.ret_ty, &substitutions, &local)?;
            if definition.public
                || !definition.type_params.is_empty()
                || definition.suspends != requirement.suspends
                || receiver(definition).is_some() != receiver(&requirement.definition()).is_some()
                || !params.iter().eq(definition.params.iter().map(|p| &p.ty))
                || ret != definition.ret_ty
            {
                return Err(type_error(&definition.span, ErrorCode::TypeMismatch,
                    format!("Signature does not match trait method '{name}.{}' (visibility is inherited from the trait)", definition.name)));
            }
            let mut function = definition.clone();
            if let Some(required) = &requirement.noescape {
                let actual = function.noescape.get_or_insert_with(Default::default);
                actual.all |= required.all;
                for name in &required.params {
                    let index = requirement
                        .params
                        .iter()
                        .position(|p| &p.name == name)
                        .ok_or_else(|| {
                            type_error(
                                &requirement.span,
                                ErrorCode::UnknownName,
                                "Unknown @noescape parameter",
                            )
                        })?;
                    let name = &definition.params[index].name;
                    if !actual.params.contains(name) {
                        actual.params.push(name.clone());
                    }
                }
            }
            function.name = format!(
                "joky/impl/{}/{}.{}",
                binding.id,
                match &implementation.target {
                    Type::Parameter(p) => format!("blanket_{}", p.id),
                    ty => members::owner_name(ty),
                },
                definition.name
            );
            function.type_params = implementation.type_params.clone();
            members.functions.insert(function.name.clone());
            members.methods.push(Method {
                target: implementation.target.clone(),
                trait_id: Some(binding.id),
                name: definition.name.clone(),
                function: function.name.clone(),
                receiver: receiver(definition),
                impl_parameters: implementation.type_params.len(),
                method_parameters: 0,
            });
            module.items.push(Item::FnDef(function));
        }
        for requirement in &declaration.methods {
            if names.contains(&requirement.name) {
                continue;
            }
            if requirement.body.is_none() {
                return Err(type_error(
                    &implementation.span,
                    ErrorCode::TypeMismatch,
                    format!("Missing trait method '{name}.{}'", requirement.name),
                ));
            }
            let mut function = requirement.definition();
            function.scope = declaration.scope.clone();
            function.name = format!(
                "joky/impl/{}/{}.{}",
                binding.id,
                match &implementation.target {
                    Type::Parameter(p) => format!("blanket_{}", p.id),
                    ty => members::owner_name(ty),
                },
                requirement.name
            );
            function.type_params = implementation.type_params.clone();
            for param in &mut function.params {
                param.ty = substitute(&param.ty, &substitutions, &local)?;
            }
            function.ret_ty = substitute(&function.ret_ty, &substitutions, &local)?;
            crate::core::types::map_annotations(&mut function.body, &mut |ty, span| {
                *ty = substitute(ty, &substitutions, &local).map_err(|e| e.with_span(span))?;
                Ok(())
            })?;
            members.functions.insert(function.name.clone());
            members.methods.push(Method {
                target: implementation.target.clone(),
                trait_id: Some(binding.id),
                name: requirement.name.clone(),
                function: function.name.clone(),
                receiver: receiver(&requirement.definition()),
                impl_parameters: implementation.type_params.len(),
                method_parameters: 0,
            });
            module.items.push(Item::FnDef(function));
        }
        members.traits.implementations.push(Implementation {
            id: binding.id,
            target: implementation.target.clone(),
            parameters: implementation.type_params.clone(),
        });
    }
    Ok(())
}

impl Members {
    pub fn trait_method(
        &self,
        ty: &Type,
        name: &str,
        requested: Option<u64>,
        scope: &ImportScope,
        span: Span,
    ) -> Result<Option<&Method>, Error> {
        let mut selected = None;
        for method in &self.methods {
            let Some(id) = method.trait_id else {
                continue;
            };
            if requested.is_some_and(|wanted| wanted != id)
                || (requested.is_none() && !scope.traits.values().any(|b| b.id == id))
                || method.name != name
            {
                continue;
            }
            let rigid = matches!(ty, Type::Parameter(p) if self.traits.bounds.get(&p.id).is_some_and(|bounds| bounds.contains(&id)))
                || matches!(ty, Type::Projection(_))
                    && self
                        .traits
                        .projection_types
                        .available_traits(ty)?
                        .contains(&id);
            let applicable = if self.traits.requirements.contains(&method.function) {
                rigid
            } else {
                // A rigid bound uses its abstract requirement until specialization.
                if rigid {
                    continue;
                }
                let implementation = self
                    .traits
                    .implementations
                    .iter()
                    .find(|i| i.id == id && i.target == method.target)
                    .unwrap();
                self.traits
                    .applies(implementation, ty)
                    .map_err(|e| e.with_span(span))?
            };
            if !applicable {
                continue;
            }
            if selected.is_some() {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    format!("Ambiguous trait method '{name}'; use Trait.{name}(receiver, ...)"),
                ));
            }
            selected = Some(method);
        }
        Ok(selected)
    }
}

impl FunctionPlan {
    pub(super) fn explicit_trait_call(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Option<Expr>, Error> {
        let (path, name, type_args, args, span) = match expr {
            Expr::Call {
                callee,
                type_args,
                args,
                span,
            } => {
                let Some((path, name)) = callee.rsplit_once('.') else {
                    return Ok(None);
                };
                (path.to_owned(), name, type_args, args, *span)
            }
            Expr::MethodCall(call) => {
                let Some(path) = super::expression_path(&call.receiver) else {
                    return Ok(None);
                };
                (
                    path,
                    call.name.as_str(),
                    &call.type_args,
                    &call.args,
                    call.span,
                )
            }
            _ => return Ok(None),
        };
        if vars.contains_key(path.split('.').next().unwrap()) || types.is_parameter_name(&path) {
            return Ok(None);
        }
        let Some(binding) = self.scope.traits.get(&path) else {
            return Ok(None);
        };
        if let Expr::MethodCall(call) = expr {
            let mut receiver = &call.receiver;
            loop {
                if super::expression_type_arguments(receiver).is_some() {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidCall,
                        "Traits do not accept type arguments; use Trait.method::<Self>(...)",
                    ));
                }
                match receiver {
                    Expr::Field { base, .. } => receiver = base,
                    _ => break,
                }
            }
        }
        let requirement = {
            let context = self.generics.borrow();
            context
                .members
                .methods
                .iter()
                .find(|m| {
                    m.trait_id == Some(binding.id)
                        && m.name == name
                        && matches!(m.target, Type::Parameter(_))
                })
                .map(|m| m.function.clone())
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::UnknownName,
                        format!("Trait '{path}' has no method '{name}'"),
                    )
                })?
        };
        let probe = Expr::Call {
            callee: format!("joky/member/{requirement}"),
            type_args: type_args.clone(),
            args: args.clone(),
            span,
        };
        let (arguments, _) = self.generic_signature(&probe, vars, functions, types)?;
        let target = arguments[0].as_type().expect("trait Self type");
        let context = self.generics.borrow();
        let method = context
            .members
            .trait_method(target, name, Some(binding.id), &self.scope, span)?
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    format!("Type '{target}' does not implement trait '{path}'"),
                )
            })?;
        let type_args = if method.impl_parameters == 0 {
            Vec::new()
        } else {
            match (&method.target, target) {
                (Type::Parameter(_), _) => vec![target.clone().into()],
                (_, Type::Record(r)) => r.arguments.clone(),
                (_, Type::Enum(e)) => e.arguments.clone(),
                _ => unreachable!("generic trait targets are nominal or rigid"),
            }
        };
        Ok(Some(Expr::Call {
            callee: format!("joky/member/{}", method.function),
            type_args,
            args: args.clone(),
            span,
        }))
    }
}
