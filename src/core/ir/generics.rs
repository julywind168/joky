//! Rigid template checking and bounded, module-local function specialization.
use super::*;
use crate::core::types::substitute;
use crate::{parser::ast::TypeParameter, FunctionSignature};
use std::collections::HashMap;

pub(super) struct ConstructorContext<'a> {
    pub expected: Option<&'a Type>,
    pub default: bool,
}

const MAX_INSTANCES: usize = 256;
const MAX_INSTANCE_DEPTH: usize = 32;

#[derive(Clone)]
pub(super) struct Instance {
    pub name: String,
    pub template: String,
    arguments: Vec<Type>,
    pub signature: FunctionSignature,
    pub depth: usize,
}

pub(super) struct GenericContext {
    pub scope: Arc<crate::core::imports::ImportScope>,
    pub templates: HashMap<String, FnDef>,
    pub instances: Vec<Instance>,
    pub checking: bool,
    pub members: std::rc::Rc<super::members::Members>,
}

impl GenericContext {
    pub fn new(module: &AstModule) -> Self {
        Self {
            scope: module.scope.clone().unwrap_or_default(),
            templates: module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::FnDef(f) if !f.type_params.is_empty() => {
                        Some((f.name.clone(), f.clone()))
                    }
                    _ => None,
                })
                .collect(),
            instances: Vec::new(),
            checking: true,
            members: Default::default(),
        }
    }

    fn instantiate(
        &mut self,
        template: &str,
        arguments: Vec<Type>,
        signature: FunctionSignature,
        depth: usize,
        span: Span,
    ) -> Result<String, Error> {
        if self.checking {
            return Ok(template.to_owned());
        }
        // Cache before planning bodies so ordinary and mutual recursion terminate.
        if let Some(instance) = self
            .instances
            .iter()
            .find(|i| i.template == template && i.arguments == arguments)
        {
            return Ok(instance.name.clone());
        }
        if self.instances.len() >= MAX_INSTANCES || depth > MAX_INSTANCE_DEPTH {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Generic instantiation exceeds 256 instances or 32 dependency levels",
            ));
        }
        let name = format!("__joky_generic_{}_{}", self.instances.len(), template);
        self.instances.push(Instance {
            name: name.clone(),
            template: template.into(),
            arguments,
            signature,
            depth,
        });
        Ok(name)
    }

    pub fn definition(&self, instance: &Instance, types: &TypeRegistry) -> Result<FnDef, Error> {
        let mut definition = self.templates[&instance.template].clone();
        let substitutions = definition
            .type_params
            .iter()
            .zip(&instance.arguments)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect();
        definition.name.clone_from(&instance.name);
        definition.type_params.clear();
        for param in &mut definition.params {
            param.ty = substitute(&param.ty, &substitutions, types)
                .map_err(|e| e.with_span(param.span))?;
        }
        definition.ret_ty = instance.signature.ret.clone();
        super::super::types::map_annotations(&mut definition.body, &mut |ty, span| {
            *ty = substitute(ty, &substitutions, types).map_err(|e| e.with_span(span))?;
            Ok(())
        })?;
        Ok(definition)
    }
}

pub(super) fn unify(
    formal: &Type,
    actual: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &mut HashMap<u64, Type>,
    span: Span,
) -> Result<(), Error> {
    // Iterative tuple matching avoids expanding shared zero-sized type DAGs.
    let mut pending = vec![(formal, actual)];
    let mut seen = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    let mut records = std::collections::HashSet::new();
    while let Some((formal, actual)) = pending.pop() {
        match (formal, actual) {
            (Type::Reference(a), Type::Reference(b)) if a.mutable == b.mutable || !a.mutable => {
                pending.push((&a.target, &b.target))
            }
            (Type::Parameter(p), _) if params.iter().any(|param| param.id == p.id) => {
                if let Some(previous) = substitutions.get(&p.id) {
                    if previous != actual {
                        return Err(type_error(&span, ErrorCode::TypeMismatch,
                            format!("Conflicting types for generic parameter `{}`: {previous} and {actual}", p.name)));
                    }
                } else {
                    substitutions.insert(p.id, actual.clone());
                }
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.elements.len() == b.elements.len() => {
                if seen.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.elements.iter().zip(&b.elements));
                }
            }
            (Type::Enum(a), Type::Enum(b))
                if a.id == b.id && a.arguments.len() == b.arguments.len() =>
            {
                if enums.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
            (Type::Record(a), Type::Record(b))
                if a.id == b.id && a.arguments.len() == b.arguments.len() =>
            {
                if records.insert((Arc::as_ptr(a), Arc::as_ptr(b))) {
                    pending.extend(a.arguments.iter().zip(&b.arguments));
                }
            }
            _ if formal == actual => {}
            _ => {
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    format!("Generic argument type mismatch: expected {formal}, got {actual}"),
                ))
            }
        }
    }
    Ok(())
}

pub(super) fn unresolved(
    ty: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &HashMap<u64, Type>,
) -> bool {
    let mut pending = vec![ty];
    let mut seen = std::collections::HashSet::new();
    let mut enums = std::collections::HashSet::new();
    let mut records = std::collections::HashSet::new();
    while let Some(ty) = pending.pop() {
        match ty {
            Type::Reference(r) => pending.push(&r.target),
            Type::Parameter(p)
                if params.iter().any(|q| p.id == q.id) && !substitutions.contains_key(&p.id) =>
            {
                return true
            }
            Type::Tuple(t) if seen.insert(Arc::as_ptr(t)) => pending.extend(&t.elements),
            Type::Enum(e) if enums.insert(Arc::as_ptr(e)) => pending.extend(&e.arguments),
            Type::Record(r) if records.insert(Arc::as_ptr(r)) => pending.extend(&r.arguments),
            _ => {}
        }
    }
    false
}

pub(super) struct InferenceState<'a> {
    pub params: &'a [Arc<TypeParameter>],
    pub substitutions: HashMap<u64, Type>,
    pub default: bool,
}

/// Seed inference with explicitly provided type arguments, if any.
pub(super) fn seed_inference<'a>(
    parameters: &'a [Arc<TypeParameter>],
    explicit: &[Type],
) -> InferenceState<'a> {
    InferenceState {
        params: parameters,
        substitutions: parameters
            .iter()
            .zip(explicit)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect(),
        default: false,
    }
}

/// The shared two-pass protocol: infer from concrete argument types first,
/// then run defaults only for parameters still unresolved. `defaults_allowed`
/// gates the second pass; callers keep their own missing-argument diagnostics.
pub(super) fn inference_passes(
    inference: &mut InferenceState<'_>,
    defaults_allowed: bool,
    mut pass: impl FnMut(&mut InferenceState<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    for default in [false, true] {
        if default && !defaults_allowed {
            break;
        }
        inference.default = default;
        pass(inference)?;
    }
    Ok(())
}

impl FunctionPlan {
    pub(super) fn materialize_inference(
        &self,
        inference: &mut InferenceState<'_>,
        types: &TypeRegistry,
        span: Span,
    ) -> Result<(), Error> {
        // Unification can extract shallow nominal identities from references.
        // Type parameters also occur by value, where their full layout is needed.
        for ty in inference.substitutions.values_mut() {
            *ty = self.pointee(ty, types).map_err(|e| e.with_span(span))?;
        }
        Ok(())
    }

    pub(super) fn generic_signature(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<(Vec<Type>, FunctionSignature), Error> {
        let Expr::Call {
            callee,
            type_args,
            args,
            span,
        } = expr
        else {
            unreachable!()
        };
        let context = self.generics.borrow();
        let template_name = self.function_name(callee).unwrap_or_else(|| callee.clone());
        let template = &context.templates[&template_name];
        if args.len() != template.params.len() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Argument count mismatch for `{callee}`"),
            ));
        }
        // Source member arities are checked by member_call. Its protected call
        // may bind only the receiver's impl prefix and infer method parameters.
        // Ordinary function calls still require all explicit arguments.
        let impl_prefix = callee.starts_with("joky/member/")
            && context.members.methods.iter().any(|method| {
                method.function == template_name && method.impl_parameters == type_args.len()
            });
        if !type_args.is_empty() && type_args.len() != template.type_params.len() && !impl_prefix {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!(
                    "Type argument count mismatch for `{callee}`: expected {}",
                    template.type_params.len()
                ),
            ));
        }
        let mut inference = seed_inference(&template.type_params, type_args);
        // First use already known argument types, then default only unresolved
        // parameters. Hints never evaluate arguments or move source effects.
        inference_passes(&mut inference, true, |inference| {
            let mut local = vars.clone();
            for (param, arg) in template.params.iter().zip(args) {
                self.infer_generic_argument(&param.ty, arg, &local, functions, types, inference)?;
                self.hint_bindings(arg, &mut local, functions, types);
            }
            Ok(())
        })?;
        self.materialize_inference(&mut inference, types, *span)?;
        let substitutions = inference.substitutions;
        let arguments: Vec<_> = template.type_params.iter().map(|p| {
            let ty = substitutions.get(&p.id).ok_or_else(|| type_error(span, ErrorCode::TypeMismatch,
                format!("Cannot infer type parameter `{}` for `{callee}`; use explicit type arguments", p.name)))?;
            types.validate(ty).map_err(|e| e.with_span(*span))?;
            validate_nominal_type(ty, &self.nominals, span)?;
            if !ty.is_copy_value() {
                return Err(type_error(span, ErrorCode::TypeMismatch,
                    format!("Generic type argument must be a Copy type, got {ty}")));
            }
            Ok(ty.clone())
        }).collect::<Result<_, Error>>()?;
        let params = template
            .params
            .iter()
            .map(|p| substitute(&p.ty, &substitutions, types))
            .collect::<Result<_, _>>()?;
        let ret = substitute(&template.ret_ty, &substitutions, types)?;
        Ok((arguments, FunctionSignature::new(params, ret)))
    }

    pub(super) fn infer_generic_argument(
        &self,
        formal: &Type,
        arg: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        inference: &mut InferenceState,
    ) -> Result<(), Error> {
        if inference.default && !unresolved(formal, inference.params, &inference.substitutions) {
            return Ok(());
        }
        if let (Type::Tuple(tuple), Expr::Tuple { elements, .. }) = (formal, arg) {
            if tuple.elements.len() != elements.len() {
                return Err(type_error(
                    &arg.span(),
                    ErrorCode::TypeMismatch,
                    "Generic tuple argument has the wrong arity",
                ));
            }
            let mut local = vars.clone();
            for (ty, expr) in tuple.elements.iter().zip(elements) {
                self.infer_generic_argument(ty, expr, &local, functions, types, inference)?;
                self.hint_bindings(expr, &mut local, functions, types);
            }
            return Ok(());
        }
        if let (
            Type::Record(record),
            Expr::Record {
                name,
                type_args,
                fields,
                ..
            },
        ) = (formal, arg)
        {
            if type_args.is_empty()
                && matches!(self.named_type(name), Some(Type::Record(actual)) if actual.id == record.id)
            {
                let mut local = vars.clone();
                for field in fields {
                    if let Some(formal) = record.fields.iter().find(|f| f.name == field.name) {
                        self.infer_generic_argument(
                            &formal.ty,
                            &field.value,
                            &local,
                            functions,
                            types,
                            inference,
                        )?;
                    }
                    self.hint_bindings(&field.value, &mut local, functions, types);
                }
                return Ok(());
            }
        }
        if let Type::Enum(formal_enum) = formal {
            if let Some(constructor) = self.enum_constructor(arg, vars) {
                if constructor.enumeration.id == formal_enum.id && constructor.type_args.is_empty()
                {
                    if let Some(variant) = formal_enum
                        .variants
                        .iter()
                        .find(|v| v.name == constructor.name)
                    {
                        if let (Some(payload), Some([arg])) = (&variant.payload, constructor.args) {
                            self.infer_generic_argument(
                                payload, arg, vars, functions, types, inference,
                            )?;
                        }
                        return Ok(());
                    }
                }
            }
        }
        let hint = if inference.default {
            self.bound_hint(arg, vars, functions, types)
        } else {
            self.type_hint(arg, vars, functions, types)
        };
        if let Some(actual) = hint {
            unify(
                formal,
                &actual,
                inference.params,
                &mut inference.substitutions,
                arg.span(),
            )?;
        }
        Ok(())
    }

    pub(super) fn function_name(&self, name: &str) -> Option<String> {
        // Only member_call can emit this unspellable path after visibility checks.
        if let Some(member) = name.strip_prefix("joky/member/") {
            return self
                .generics
                .borrow()
                .members
                .functions
                .contains(member)
                .then(|| member.to_owned());
        }
        self.scope.functions.get(name).cloned().or_else(|| {
            (self.definition_scope.is_none()
                && !self.scope.imported_functions.contains(name)
                && !self.generics.borrow().members.functions.contains(name))
            .then(|| name.to_owned())
        })
    }

    pub(super) fn resolve_callee(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<(String, FunctionInfo), Error> {
        let Expr::Call {
            callee,
            type_args,
            span,
            ..
        } = expr
        else {
            unreachable!()
        };
        let name = self.function_name(callee);
        let info = name
            .as_ref()
            .and_then(|name| functions.get(name))
            .ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::UnknownName,
                    format!("Undeclared function `{callee}`"),
                )
            })?;
        if self
            .generics
            .borrow()
            .templates
            .contains_key(name.as_ref().unwrap())
        {
            let (arguments, signature) = self.generic_signature(expr, vars, functions, types)?;
            let name = self.generics.borrow_mut().instantiate(
                name.as_ref().unwrap(),
                arguments,
                signature.clone(),
                self.instantiation_depth + 1,
                *span,
            )?;
            let mut info = info.clone();
            info.signature = signature;
            info.frame_slots = info.parameter_slots();
            info.internal = true;
            Ok((name, info))
        } else {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    format!("Non-generic function `{callee}` does not accept type arguments"),
                ));
            }
            Ok((name.unwrap(), info.clone()))
        }
    }
}
