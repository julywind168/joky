//! Rigid template checking and bounded, module-local function specialization.
use super::*;
use crate::{parser::ast::TypeParameter, FunctionSignature};
use std::collections::HashMap;

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
    pub templates: HashMap<String, FnDef>,
    pub instances: Vec<Instance>,
    pub checking: bool,
}

impl GenericContext {
    pub fn new(module: &AstModule) -> Self {
        Self {
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

fn substitute(
    ty: &Type,
    substitutions: &HashMap<u64, Type>,
    types: &TypeRegistry,
) -> Result<Type, Error> {
    fn visit(
        ty: &Type,
        substitutions: &HashMap<u64, Type>,
        types: &TypeRegistry,
        cache: &mut HashMap<*const crate::parser::ast::TupleType, Type>,
    ) -> Result<Type, Error> {
        Ok(match ty {
            Type::Parameter(p) => substitutions
                .get(&p.id)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Tuple(tuple) => {
                let key = Arc::as_ptr(tuple);
                if let Some(ty) = cache.get(&key) {
                    return Ok(ty.clone());
                }
                let elements = tuple
                    .elements
                    .iter()
                    .map(|ty| visit(ty, substitutions, types, cache))
                    .collect::<Result<_, _>>()?;
                let ty = super::super::types::tuple_type(elements, types)?;
                cache.insert(key, ty.clone());
                ty
            }
            _ => ty.clone(),
        })
    }
    visit(ty, substitutions, types, &mut HashMap::new())
}

fn unify(
    formal: &Type,
    actual: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &mut HashMap<u64, Type>,
    span: Span,
) -> Result<(), Error> {
    // Iterative tuple matching avoids expanding shared zero-sized type DAGs.
    let mut pending = vec![(formal, actual)];
    let mut seen = std::collections::HashSet::new();
    while let Some((formal, actual)) = pending.pop() {
        match (formal, actual) {
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

fn unresolved(
    ty: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &HashMap<u64, Type>,
) -> bool {
    let mut pending = vec![ty];
    let mut seen = std::collections::HashSet::new();
    while let Some(ty) = pending.pop() {
        match ty {
            Type::Parameter(p)
                if params.iter().any(|q| p.id == q.id) && !substitutions.contains_key(&p.id) =>
            {
                return true
            }
            Type::Tuple(t) if seen.insert(Arc::as_ptr(t)) => pending.extend(&t.elements),
            _ => {}
        }
    }
    false
}

struct InferenceState<'a> {
    params: &'a [Arc<TypeParameter>],
    substitutions: HashMap<u64, Type>,
    default: bool,
}

impl FunctionPlan {
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
        let template = &context.templates[callee];
        if args.len() != template.params.len() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("Argument count mismatch for `{callee}`"),
            ));
        }
        if !type_args.is_empty() && type_args.len() != template.type_params.len() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!(
                    "Type argument count mismatch for `{callee}`: expected {}",
                    template.type_params.len()
                ),
            ));
        }
        let mut inference = InferenceState {
            params: &template.type_params,
            substitutions: template
                .type_params
                .iter()
                .zip(type_args)
                .map(|(p, ty)| (p.id, ty.clone()))
                .collect(),
            default: false,
        };
        // First use already known argument types, then default only unresolved
        // parameters. Hints never evaluate arguments or move source effects.
        for default in [false, true] {
            inference.default = default;
            let mut local = vars.clone();
            for (param, arg) in template.params.iter().zip(args) {
                self.infer_generic_argument(
                    &param.ty,
                    arg,
                    &local,
                    functions,
                    types,
                    &mut inference,
                )?;
                self.hint_bindings(arg, &mut local, functions, types);
            }
        }
        let substitutions = inference.substitutions;
        let arguments: Vec<_> = template.type_params.iter().map(|p| {
            let ty = substitutions.get(&p.id).ok_or_else(|| type_error(span, ErrorCode::TypeMismatch,
                format!("Cannot infer type parameter `{}` for `{callee}`; use explicit type arguments", p.name)))?;
            types.validate(ty).map_err(|e| e.with_span(*span))?;
            validate_nominal_type(ty, &self.nominals, span)?;
            if !ty.is_copy_scalar() && !ty.is_aggregate() {
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

    fn infer_generic_argument(
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
        let info = functions.get(callee).ok_or_else(|| {
            type_error(
                span,
                ErrorCode::UnknownName,
                format!("Undeclared function `{callee}`"),
            )
        })?;
        if self.generics.borrow().templates.contains_key(callee) {
            let (arguments, signature) = self.generic_signature(expr, vars, functions, types)?;
            let name = self.generics.borrow_mut().instantiate(
                callee,
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
            Ok((callee.clone(), info.clone()))
        }
    }
}
