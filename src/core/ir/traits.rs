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
        if implementation.parameters.is_empty() && implementation.target != *ty
            || !matches!(implementation.target, Type::Parameter(_))
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
                impl_type_params: vec![declaration.self_parameter.clone()],
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
    for (implementation_index, implementation) in module.implementations.iter().enumerate() {
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
                    && (!i.parameters.is_empty()
                        || !implementation.type_params.is_empty()
                        || i.target == implementation.target)
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
            let mismatch = if definition.public {
                Some("visibility is inherited from the trait; remove `pub`".to_owned())
            } else if !definition.type_params.is_empty() {
                Some("the implementation method cannot introduce type parameters".into())
            } else if definition.suspends != requirement.suspends {
                Some(
                    if requirement.suspends {
                        "expected a suspending method"
                    } else {
                        "expected a synchronous method"
                    }
                    .into(),
                )
            } else if receiver(definition).is_some()
                != receiver(&requirement.definition()).is_some()
            {
                Some("the self receiver differs from the declaration".into())
            } else if params.len() != definition.params.len() {
                Some(format!(
                    "expected {} parameters, got {}",
                    params.len(),
                    definition.params.len()
                ))
            } else if let Some((i, (expected, actual))) = params
                .iter()
                .zip(&definition.params)
                .enumerate()
                .find(|(_, (expected, p))| **expected != p.ty)
            {
                Some(format!(
                    "parameter {} expects {expected}, got {}",
                    i + 1,
                    actual.ty
                ))
            } else if ret != definition.ret_ty {
                Some(format!(
                    "expected return type {ret}, got {}",
                    definition.ret_ty
                ))
            } else {
                None
            };
            if let Some(detail) = mismatch {
                return Err(type_error(
                    &definition.span,
                    ErrorCode::TypeMismatch,
                    format!(
                        "Signature does not match trait method '{name}.{}': {detail}",
                        definition.name
                    ),
                ));
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
                "joky/impl/{}/{}/{}.{}",
                binding.id,
                implementation_index,
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
                impl_type_params: implementation.type_params.clone(),
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
                "joky/impl/{}/{}/{}.{}",
                binding.id,
                implementation_index,
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
                impl_type_params: implementation.type_params.clone(),
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
        let mut selected: Option<&Method> = None;
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
                        .available_traits(ty, |candidate| candidate == id)?
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
            if let Some(previous) = selected {
                if previous.trait_id == Some(id) {
                    return Err(type_error(&span, ErrorCode::TypeMismatch, format!("Overlapping trait implementations for `{ty}` provide `{name}`; make their implementation bounds disjoint")));
                }
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

impl FunctionPlan {
    /// Equality borrows both operands through the trusted Eq requirement. The
    /// ordinary call planner retains temporaries and evaluates each operand once.
    pub(super) fn equality_call(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Option<Expr>, Error> {
        let Expr::BinOp {
            lhs,
            op: op @ (BinOp::Eq | BinOp::Ne),
            rhs,
            span,
        } = expr
        else {
            return Ok(None);
        };
        let Some(hint) = self
            .binary_hint(lhs, rhs, vars, functions, types)
            .or_else(|| self.bound_hint(lhs, vars, functions, types))
            .or_else(|| self.bound_hint(rhs, vars, functions, types))
        else {
            return Ok(None);
        };
        // Primitive implementations use the intrinsic comparison themselves.
        // String retains its content comparison, including reference operands.
        let target = hint.reference_type().map_or(&hint, |r| &r.target);
        let scalar = hint.is_numeric() || matches!(hint, Type::Bool | Type::Rune);
        let borrowed_rhs = scalar
            && self
                .type_hint(rhs, vars, functions, types)
                .is_some_and(|ty| ty.reference_type().is_some());
        if scalar && !borrowed_rhs || *target == Type::String {
            return Ok(None);
        }
        let context = self.generics.borrow();
        let id = context
            .members
            .traits
            .definitions
            .iter()
            .find(|d| d.is_eq)
            .map(|d| d.self_parameter.id);
        let method = match id {
            Some(id) => context
                .members
                .trait_method(target, "eq", Some(id), &self.scope, *span)?,
            None => None,
        };
        // Payload-free enums keep their built-in tag comparison without Eq.
        if method.is_none()
            && matches!(&hint, Type::Enum(e) if e.variants.iter().all(|v| v.payload.is_none()))
        {
            return Ok(None);
        }
        let method = method.ok_or_else(|| {
            type_error(
                span,
                ErrorCode::TypeMismatch,
                format!("Type '{target}' does not implement joky/eq.Eq for equality comparison"),
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
        let call = Expr::Call {
            callee: format!("joky/member/{}", method.function),
            type_args,
            args: [lhs.as_ref(), rhs.as_ref()]
                .into_iter()
                .map(|operand| {
                    let place = indexing::is_index_place(operand)
                        || locals::local_path(operand)
                            .is_some_and(|(name, _)| vars.contains_key(name));
                    let needs_borrow =
                        self.type_hint(operand, vars, functions, types)
                            .is_none_or(|ty| {
                                ty.reference_type().is_none()
                                    && !ty.is_numeric()
                                    && !matches!(ty, Type::Bool | Type::Rune)
                            });
                    if place && needs_borrow {
                        Expr::Unary {
                            op: UnaryOp::Borrow,
                            operand: Box::new(operand.clone()),
                            span: operand.span(),
                        }
                    } else {
                        operand.clone()
                    }
                })
                .collect(),
            span: *span,
        };
        Ok(Some(if *op == BinOp::Ne {
            Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(call),
                span: *span,
            }
        } else {
            call
        }))
    }

    /// Select the trusted Add identity before evaluating either operand. Ordinary
    /// call planning preserves left-to-right evaluation, moves and failure cleanup.
    pub(super) fn addition_call(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<Option<Expr>, Error> {
        let Expr::BinOp {
            lhs,
            op: BinOp::Add,
            rhs,
            span,
        } = expr
        else {
            return Ok(None);
        };
        let Some(target) = self.binary_hint(lhs, rhs, vars, functions, types) else {
            return Ok(None);
        };
        if target.is_numeric() {
            return Ok(None);
        }
        let context = self.generics.borrow();
        let id = context
            .members
            .traits
            .definitions
            .iter()
            .find(|d| d.is_add)
            .map(|d| d.self_parameter.id);
        let method = match id {
            Some(id) => {
                context
                    .members
                    .trait_method(&target, "add", Some(id), &self.scope, *span)?
            }
            None => None,
        }
        .ok_or_else(|| {
            type_error(
                span,
                ErrorCode::TypeMismatch,
                format!("Type '{target}' does not implement joky/ops.Add for '+'"),
            )
        })?;
        let type_args = if method.impl_parameters == 0 {
            Vec::new()
        } else {
            match (&method.target, &target) {
                (Type::Parameter(_), _) => vec![target.clone().into()],
                (_, Type::Record(r)) => r.arguments.clone(),
                (_, Type::Enum(e)) => e.arguments.clone(),
                _ => unreachable!("generic trait targets are nominal or rigid"),
            }
        };
        Ok(Some(Expr::Call {
            callee: format!("joky/member/{}", method.function),
            type_args,
            args: vec![(**lhs).clone(), (**rhs).clone()],
            span: *span,
        }))
    }
}
