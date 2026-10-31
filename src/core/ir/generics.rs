//! Rigid template checking and bounded, module-local function specialization.
use super::*;
use crate::core::types::substitute;
use crate::core::types::{GenericArgument, ParameterKind, TypeParameter};
use crate::FunctionSignature;
use std::collections::HashMap;

pub(super) struct ConstructorContext<'a> {
    pub expected: Option<&'a Type>,
    pub default: bool,
}

const MAX_GENERATED_FUNCTIONS: usize = 4096;
const MAX_INSTANCE_DEPTH: usize = 32;

#[derive(Clone)]
pub(super) struct Instance {
    pub name: String,
    pub template: String,
    pub call_span: Span,
    arguments: Vec<crate::core::types::GenericArgument>,
    pub signature: FunctionSignature,
    pub depth: usize,
}

#[derive(Clone)]
pub(super) struct ClosureInstance {
    pub parent: String,
    pub site: Span,
    pub environment: Arc<crate::core::types::RecordType>,
}

pub(super) struct GenericContext {
    /// Attribute embedded qualification failures to the user's library call.
    pub library_call_span: Option<Span>,
    /// Declaring modules keep handle-method identity independent of the caller.
    pub when_modules: HashMap<String, String>,
    pub when_parents: HashMap<String, String>,
    pub module_name: String,
    pub when_sites: Vec<(String, Span, String)>,
    pub scope: Arc<crate::core::imports::ImportScope>,
    pub templates: HashMap<String, FnDef>,
    pub instances: Vec<Instance>,
    pub checking: bool,
    pub closure_definitions: HashMap<String, super::closures::ClosureDefinition>,
    pub generated_bodies: Vec<NestedTask>,
    pub generated_functions: HashMap<String, FunctionInfo>,
    pub closures: Vec<ClosureInstance>,
    pub callables: Vec<crate::core::types::FunctionType>,
    pub abstract_callables: Vec<(FunctionSignature, bool)>,
    pub noescape_closures: std::collections::HashSet<String>,
    pub dyn_tables: Vec<Arc<DynTable>>,
    pub dyn_contracts: Vec<crate::core::types::DynMethodSlot>,
    pub definitions: std::collections::HashSet<String>,
    pub members: std::rc::Rc<super::members::Members>,
}

impl GenericContext {
    pub fn new(module: &AstModule) -> Self {
        Self {
            library_call_span: None,
            when_modules: module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::FnDef(f) => Some((
                        f.name.clone(),
                        f.scope.as_ref().map_or_else(
                            || module.logical_name.clone(),
                            |scope| scope.module_name.clone(),
                        ),
                    )),
                    _ => None,
                })
                .collect(),
            when_parents: HashMap::new(),
            module_name: module.logical_name.clone(),
            when_sites: Vec::new(),
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
            closure_definitions: HashMap::new(),
            generated_bodies: Vec::new(),
            generated_functions: HashMap::new(),
            closures: Vec::new(),
            callables: Vec::new(),
            abstract_callables: Vec::new(),
            noescape_closures: Default::default(),
            dyn_tables: Vec::new(),
            dyn_contracts: Vec::new(),
            definitions: module
                .items
                .iter()
                .filter_map(|i| match i {
                    Item::FnDef(f) if f.buffer_intrinsic.is_none() => Some(f.name.clone()),
                    _ => None,
                })
                .collect(),
            members: Default::default(),
        }
    }

    pub(super) fn instantiate(
        &mut self,
        template: &str,
        arguments: Vec<crate::core::types::GenericArgument>,
        signature: FunctionSignature,
        depth: usize,
        span: Span,
    ) -> Result<String, Error> {
        let call_span = self.library_call_span.unwrap_or(span);
        if matches!(
            self.templates[template].compiler_intrinsic,
            Some(CompilerIntrinsic::PlainCopy(_))
        ) {
            validate_plain_copy(
                arguments[0].as_type().expect("intrinsic type argument"),
                call_span,
            )?;
        }
        if matches!(
            self.templates[template].compiler_intrinsic,
            Some(CompilerIntrinsic::ReferenceFree(_))
        ) {
            validate_reference_free(
                arguments[0].as_type().expect("intrinsic type argument"),
                call_span,
            )?;
        }
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
        self.check_generation(1, depth, span)?;
        let name = format!("__joky_generic_{}_{}", self.instances.len(), template);
        self.instances.push(Instance {
            name: name.clone(),
            template: template.into(),
            call_span,
            arguments,
            signature,
            depth,
        });
        Ok(name)
    }

    // Generic bodies and closure entries share one finite code-generation budget.
    // Cache hits do not request another entry, including recursive calls.
    pub(super) fn check_generation(
        &self,
        additional: usize,
        depth: usize,
        span: Span,
    ) -> Result<(), Error> {
        if depth > MAX_INSTANCE_DEPTH {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                format!("Generated function expansion exceeds {MAX_INSTANCE_DEPTH} dependency levels (requested depth {depth})"),
            ));
        }
        let instances = self.instances.len();
        let closures = self.generated_functions.len();
        if additional > MAX_GENERATED_FUNCTIONS.saturating_sub(instances + closures) {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                format!("Generated function limit {MAX_GENERATED_FUNCTIONS} exceeded ({instances} generic instances + {closures} closure entries, requesting {additional} more)"),
            ));
        }
        Ok(())
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
        if let Some(element) = &mut definition.buffer_element {
            **element = substitute(element, &substitutions, types)
                .map_err(|e| e.with_span(definition.span))?;
        }
        if let Some(
            CompilerIntrinsic::PlainCopy(ty)
            | CompilerIntrinsic::ReferenceFree(ty)
            | CompilerIntrinsic::ScriptSchema(ty)
            | CompilerIntrinsic::DirectoryType(ty),
        ) = &mut definition.compiler_intrinsic
        {
            *ty =
                substitute(ty, &substitutions, types).map_err(|e| e.with_span(definition.span))?;
        }
        for param in &mut definition.params {
            param.ty = substitute(&param.ty, &substitutions, types)
                .map_err(|e| e.with_span(param.span))?;
        }
        definition.ret_ty = instance.signature.ret.clone();
        super::super::types::map_syntax(
            &mut definition.body,
            &mut |ty, span| {
                *ty = substitute(ty, &substitutions, types).map_err(|e| e.with_span(span))?;
                Ok(())
            },
            &substitutions,
        )?;
        Ok(definition)
    }
}

pub(super) use crate::core::types::unify;

pub(super) fn unresolved(
    ty: &Type,
    params: &[Arc<TypeParameter>],
    substitutions: &crate::core::types::Substitutions,
) -> bool {
    use crate::core::types::walk::{TypeEdges, TypeWalk};
    let unresolved_constant = |c: &crate::core::types::ConstArgument| matches!(c,crate::core::types::ConstArgument::Parameter(p) if params.iter().any(|q|q.id==p.id) && !substitutions.constants.contains_key(&p.id));
    TypeWalk::new(ty, TypeEdges::Arguments).any(|ty| {
        matches!(ty, Type::Parameter(p)
            if params.iter().any(|q| p.id == q.id) && !substitutions.contains_key(&p.id))
        || match ty {
            Type::FixedArray(a) | Type::FixedArrayInit(a) => unresolved_constant(&a.length),
            Type::Record(r) => r.arguments.iter().any(|a| matches!(a,crate::core::types::GenericArgument::Const(c) if unresolved_constant(c))),
            Type::Enum(e) => e.arguments.iter().any(|a| matches!(a,crate::core::types::GenericArgument::Const(c) if unresolved_constant(c))),
            _ => false,
        }
    })
}

pub(super) struct InferenceState<'a> {
    pub params: &'a [Arc<TypeParameter>],
    pub substitutions: crate::core::types::Substitutions,
    pub default: bool,
}

/// Seed inference with explicitly provided type arguments, if any.
pub(super) fn seed_inference<'a>(
    parameters: &'a [Arc<TypeParameter>],
    explicit: &[crate::core::types::GenericArgument],
    span: Span,
) -> Result<InferenceState<'a>, Error> {
    for (p, a) in parameters.iter().zip(explicit) {
        if !matches!(
            (p.kind, a),
            (ParameterKind::Type, GenericArgument::Type(_))
                | (ParameterKind::Const, GenericArgument::Const(_))
        ) {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                format!("Generic argument kind mismatch for `{}`: got {a}", p.name),
            ));
        }
    }
    Ok(InferenceState {
        params: parameters,
        substitutions: parameters
            .iter()
            .zip(explicit)
            .map(|(p, ty)| (p.id, ty.clone()))
            .collect(),
        default: false,
    })
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
    ) -> Result<(Vec<crate::core::types::GenericArgument>, FunctionSignature), Error> {
        self.generic_signature_expected(expr, vars, functions, types, None)
    }

    pub(super) fn generic_signature_expected(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<(Vec<crate::core::types::GenericArgument>, FunctionSignature), Error> {
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
        let template = context.templates[&template_name].clone();
        let members = context.members.clone();
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
        if !type_args.is_empty()
            && type_args.len() != template.type_params.len()
            && !impl_prefix
            && (type_args.len() > template.type_params.len()
                || !template.type_params[type_args.len().min(template.type_params.len())..]
                    .iter()
                    .all(|p| p.callable.is_some()))
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!(
                    "Type argument count mismatch for `{callee}`: expected {}",
                    template.type_params.len()
                ),
            ));
        }
        drop(context);
        let mut inference = seed_inference(&template.type_params, type_args, *span)?;
        if let Some(expected) = expected {
            if unresolved(
                &template.ret_ty,
                &template.type_params,
                &inference.substitutions,
            ) {
                unify(
                    &template.ret_ty,
                    expected,
                    &template.type_params,
                    &mut inference.substitutions,
                    *span,
                )?;
            }
        }
        // First use already known argument types, then default only unresolved
        // parameters. Hints never evaluate arguments or move source effects.
        inference_passes(&mut inference, true, |inference| {
            for _ in 0..=template.type_params.len() {
                let before = inference.substitutions.clone();
                let mut local = vars.clone();
                for (index, (param, arg)) in template.params.iter().zip(args).enumerate() {
                    let formal = if functions[&template_name].can_borrow_argument(index)
                        && !self
                            .hint(arg, &local, functions, types, inference.default)
                            .is_some_and(|ty| ty.reference_type().is_some())
                    {
                        param
                            .ty
                            .reference_type()
                            .filter(|r| !r.mutable)
                            .map_or(&param.ty, |r| &r.target)
                    } else {
                        &param.ty
                    };
                    self.infer_generic_argument(formal, arg, &local, functions, types, inference)?;
                    self.hint_bindings(arg, &mut local, functions, types);
                }
                if before == inference.substitutions {
                    break;
                }
            }
            Ok(())
        })?;
        self.materialize_inference(&mut inference, types, *span)?;
        let substitutions = inference.substitutions;
        if template
            .type_params
            .iter()
            .any(|p| substitutions.argument(p).is_none())
        {
            // Type hints deliberately suppress errors. Before reporting a missing
            // outer parameter, surface an invalid member in its argument instead.
            let mut local = vars.clone();
            for arg in args {
                let member = self.member_call(arg, &local, functions, types)?;
                let call = member.as_ref().unwrap_or(arg);
                if let Expr::Call { callee, .. } = call {
                    if self
                        .function_name(callee)
                        .is_some_and(|name| self.generics.borrow().templates.contains_key(&name))
                    {
                        self.generic_signature(call, &local, functions, types)?;
                    }
                }
                self.hint_bindings(arg, &mut local, functions, types);
            }
        }
        let display_callee = callee.strip_prefix("joky/member/").unwrap_or(callee);
        let arguments: Vec<_> = template.type_params.iter().map(|p| {
            let argument = substitutions.argument(p).ok_or_else(|| type_error(span, ErrorCode::TypeMismatch,
                format!("Cannot infer {} parameter `{}` for `{display_callee}`; use explicit generic arguments", if p.kind == crate::core::types::ParameterKind::Const { "constant" } else { "type" }, p.name)))?;
            types.validate_argument(p, &argument).map_err(|e| e.with_span(*span))?;
            let Some(ty) = argument.as_type() else { return Ok(argument); };
            types.validate(ty).map_err(|e| e.with_span(*span))?;
            validate_nominal_type(ty, &self.nominals, span)?;
            if ty.contains_explicit_arena() && !matches!(template.compiler_intrinsic, Some(CompilerIntrinsic::MemDrop)) {
                return Err(type_error(span, ErrorCode::TypeMismatch, "Arena capabilities cannot be generic type arguments"));
            }
            if !matches!(template.compiler_intrinsic, Some(CompilerIntrinsic::MemDrop)) && p.callable.is_none() && (ty.contains_unstorable_closures() && !ty.is_scoped_shared_value() || !ty.is_copy_storage_value() && !ty.is_scoped_borrowed_value() && !ty.is_owner()) {
                return Err(type_error(span, ErrorCode::TypeMismatch,
                    format!("Generic type argument must be a Copy type or owning record, got {ty}")));
            }
            members.traits.check_bounds(p, ty, *span)?;
            types.check_associated_bounds(p, ty, &substitutions, *span)?;
            if let Some(bound) = &p.callable {
                let Some((mode, actual)) = super::closures::callable_signature(ty) else { return Err(type_error(span, ErrorCode::TypeMismatch, "Callable bound requires a synchronous language callable")); };
                let required = FunctionSignature::new(bound.signature.params.iter().map(|t| substitute(t, &substitutions, types)).collect::<Result<_, _>>()?, substitute(&bound.signature.ret, &substitutions, types)?);
                if mode > bound.mode || actual != required { return Err(type_error(span, ErrorCode::TypeMismatch, "Callable capability or signature does not satisfy its bound")); }
                if bound.noescape {
                    let target = ty.reference_type().map_or(ty, |r| &r.target);
                    if let Some(closure) = target.closure_info() {
                        self.generics.borrow_mut().noescape_closures.insert(closure.entry.clone());
                    } else if !super::closures::callable_noescape(target) {
                        return Err(type_error(span, ErrorCode::TypeMismatch, "Callable bound requires a synchronous @noescape callback"));
                    }
                }
            }
            Ok(argument)
        }).collect::<Result<_, Error>>()?;
        let params = template
            .params
            .iter()
            .map(|p| substitute(&p.ty, &substitutions, types).map_err(|e| e.with_span(*span)))
            .collect::<Result<_, _>>()?;
        let ret =
            substitute(&template.ret_ty, &substitutions, types).map_err(|e| e.with_span(*span))?;
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
        if matches!(formal, Type::Function(_)) {
            if let Expr::AnonymousFunction(function) = arg {
                if !super::closures::free_names(function, vars)?.is_empty() {
                    return Err(super::closures::capturing_function_pointer_error(
                        arg.span(),
                    ));
                }
            } else if self
                .hint(arg, vars, functions, types, inference.default)
                .is_some_and(|ty| ty.closure_info().is_some_and(|c| c.pointer_entry.is_none()))
            {
                return Err(super::closures::capturing_function_pointer_error(
                    arg.span(),
                ));
            }
        }
        if inference.default && !unresolved(formal, inference.params, &inference.substitutions) {
            return Ok(());
        }
        if let Type::Parameter(parameter) = formal {
            if let Some(bound) = &parameter.callable {
                let inference_types = types.with_parameters(inference.params)?;
                let signature = FunctionSignature::new(
                    bound
                        .signature
                        .params
                        .iter()
                        .map(|t| substitute(t, &inference.substitutions, &inference_types))
                        .collect::<Result<_, _>>()?,
                    substitute(
                        &bound.signature.ret,
                        &inference.substitutions,
                        &inference_types,
                    )?,
                );
                let actual = if let Expr::AnonymousFunction(function) = arg {
                    if signature
                        .params
                        .iter()
                        .any(|t| unresolved(t, inference.params, &inference.substitutions))
                    {
                        return Ok(());
                    }
                    let mut signature = signature;
                    let mut infer_environment_result = false;
                    if unresolved(&signature.ret, inference.params, &inference.substitutions) {
                        let mut hints = vars.clone();
                        for (p, ty) in function.params.iter().zip(&signature.params) {
                            hints.insert(p.name.clone(), payload_binding(ty, 0));
                        }
                        let Some(ret) = function.ret.clone().or_else(|| {
                            self.hint(&function.body, &hints, functions, types, inference.default)
                        }) else {
                            return Ok(());
                        };
                        infer_environment_result =
                            function.ret.is_none() && ret.contains_functions();
                        signature.ret = ret;
                    }
                    if infer_environment_result {
                        // Keep contextual inputs, but let the checked body name
                        // the environment hidden behind a signature-only hint.
                        let mut contextual = function.clone();
                        for (parameter, ty) in contextual.params.iter_mut().zip(&signature.params) {
                            if parameter.ty.is_none() {
                                parameter.ty = Some(ty.clone());
                            }
                        }
                        Some(Type::Record(self.prepare_closure(
                            &contextual,
                            vars,
                            functions,
                            types,
                            None,
                        )?))
                    } else {
                        Some(Type::Record(self.prepare_closure(
                            function,
                            vars,
                            functions,
                            types,
                            Some(&signature),
                        )?))
                    }
                } else {
                    self.hint(arg, vars, functions, types, inference.default)
                };
                if let Some(actual) = actual {
                    if matches!(&actual, Type::Function(f) if f.suspends) {
                        return Err(type_error(
                            &arg.span(),
                            ErrorCode::TypeMismatch,
                            "Callable bound requires a synchronous callback without @suspends",
                        ));
                    }
                    if let Some((_, signature)) = super::closures::callable_signature(&actual) {
                        for (formal, actual) in bound
                            .signature
                            .params
                            .iter()
                            .zip(&signature.params)
                            .chain(std::iter::once((&bound.signature.ret, &signature.ret)))
                        {
                            unify(
                                formal,
                                actual,
                                inference.params,
                                &mut inference.substitutions,
                                arg.span(),
                            )?;
                        }
                    }
                    unify(
                        formal,
                        &actual,
                        inference.params,
                        &mut inference.substitutions,
                        arg.span(),
                    )?;
                }
                return Ok(());
            }
        }
        if matches!(formal, Type::Parameter(_)) {
            if let Expr::AnonymousFunction(function) = arg {
                let actual =
                    Type::Record(self.prepare_closure(function, vars, functions, types, None)?);
                unify(
                    formal,
                    &actual,
                    inference.params,
                    &mut inference.substitutions,
                    arg.span(),
                )?;
                return Ok(());
            }
        }
        if let Expr::Array { elements, .. } = arg {
            let contextual = substitute(
                formal,
                &inference.substitutions,
                &types.with_parameters(inference.params)?,
            )?;
            if let Some(element) = super::arrays::element_type(&contextual) {
                if let Type::FixedArray(array) = &contextual {
                    let length = self
                        .array_static_length(elements, vars, functions, types)
                        .ok_or_else(|| {
                            type_error(
                                &arg.span(),
                                ErrorCode::TypeMismatch,
                                "FixedArray literal requires statically known spread lengths",
                            )
                        })?;
                    crate::core::types::unify_constant(
                        &array.length,
                        &crate::core::types::ConstArgument::Value(length),
                        inference.params,
                        &mut inference.substitutions,
                        arg.span(),
                    )?;
                }
                let mut local = vars.clone();
                for expr in elements {
                    if let Expr::Spread { value, .. } = expr {
                        if matches!(value.as_ref(), Expr::Array { .. }) {
                            let container =
                                self.dynamic_array_type(element.clone(), types, value.span())?;
                            self.infer_generic_argument(
                                &container, value, &local, functions, types, inference,
                            )?;
                        } else if let Some(actual) =
                            self.hint(value, &local, functions, types, inference.default)
                        {
                            if let Some(actual) = super::arrays::spread_element_type(&actual) {
                                unify(
                                    element,
                                    actual,
                                    inference.params,
                                    &mut inference.substitutions,
                                    expr.span(),
                                )?;
                            }
                        }
                    } else {
                        self.infer_generic_argument(
                            element, expr, &local, functions, types, inference,
                        )?;
                    }
                    self.hint_bindings(expr, &mut local, functions, types);
                }
                return Ok(());
            }
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
            if let Some(constructor) = self.enum_constructor(arg, vars)? {
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
        self.resolve_callee_expected(expr, vars, functions, types, None)
    }

    pub(super) fn resolve_callee_expected(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        expected: Option<&Type>,
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
            let (arguments, signature) =
                self.generic_signature_expected(expr, vars, functions, types, expected)?;
            let name = if matches!(
                info.compiler_intrinsic,
                Some(
                    CompilerIntrinsic::MemReplace
                        | CompilerIntrinsic::MemSwap
                        | CompilerIntrinsic::ArenaAlloc
                )
            ) {
                name.unwrap()
            } else {
                self.generics.borrow_mut().instantiate(
                    name.as_ref().unwrap(),
                    arguments,
                    signature.clone(),
                    self.instantiation_depth + 1,
                    *span,
                )?
            };
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

/// Rigid templates defer the check; every concrete call validates actual contents.
pub(super) fn validate_plain_copy(ty: &Type, span: Span) -> Result<(), Error> {
    if ty
        .scalar_types()
        .iter()
        .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)))
    {
        return Ok(());
    }
    if !ty.is_copy_storage_value() || ty.contains_references() || !ty.is_buffer_element() {
        return Err(type_error(
            &span,
            ErrorCode::TypeMismatch,
            format!("Type qualification requires a reference-free Copy storage value, got {ty}"),
        ));
    }
    Ok(())
}

/// Owning storage is admitted only when no borrowed data can be embedded.
pub(super) fn validate_reference_free(ty: &Type, span: Span) -> Result<(), Error> {
    if ty
        .scalar_types()
        .iter()
        .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)))
    {
        return Ok(());
    }
    if ty.contains_references() || !ty.is_buffer_element() {
        return Err(type_error(
            &span,
            ErrorCode::TypeMismatch,
            format!("Type qualification requires reference-free storage, got {ty}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};

    fn plan(source: &str) -> Result<ModulePlan, Error> {
        let ast = crate::parser::parse(source).unwrap();
        PreparedModule::new(&ast, &TypeRegistry::default(), &BindingRegistry::default())?
            .plan(&HashMap::new())
    }

    #[test]
    fn generated_function_budget_is_shared_and_allows_cached_recursion_at_capacity() {
        // One specialization, 2047 pairs of noncapturing entries, and one
        // capturing entry fill the budget. The recursive body is planned last.
        let mut source = String::from(
            "fn keep<T>(x: T, n: Int32) -> T { if n > 0 { keep(x, n - 1) } else { x } } fn first() { keep(1, 0); }",
        );
        for i in 0..2047 {
            source.push_str(&format!("fn closure_{i}() {{ let f = || {{ 1 }} }}"));
        }
        let overflow = format!("{source}fn extra() {{ let f = || {{ 2 }} }}");
        let error = plan(&overflow)
            .err()
            .expect("a noncapturing closure needs two entries");
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert_eq!(error.message(), "Generated function limit 4096 exceeded (1 generic instances + 4094 closure entries, requesting 2 more)");
        source
            .push_str("fn captured() { let x = 1; let f = || { x } } fn cached() { keep(2, 0); }");
        plan(&source).unwrap();

        for (extra, requested, marked) in [
            ("fn extra() { keep(true, 0); }", 1, "keep(true, 0)"),
            ("fn extra() { let f = || { 2 } }", 2, "|| { 2 }"),
        ] {
            let source = format!("{source}{extra}");
            let error = plan(&source).err().expect("budget must reject a new entry");
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            assert_eq!(error.message(), format!("Generated function limit 4096 exceeded (1 generic instances + 4095 closure entries, requesting {requested} more)"));
            assert_eq!(error.span().unwrap().slice(&source), Some(marked));
        }
    }
}
