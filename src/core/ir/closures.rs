//! Compiler-owned anonymous entries and static closure responsibility.
use super::*;
use crate::core::types::{CallableKind, ClosureInfo, RecordField, RecordType};
use crate::parser::ast::{AnonymousFunction, Param};
use std::collections::{BTreeSet, HashSet, VecDeque};

pub(crate) enum ClosureOp {
    Init(Slot),
    BorrowCapture { dst: Slot, reference: Slot },
    Move { dst: Slot, src: Slot },
    Access(Slot),
    Consume(Slot),
    End(Slot),
}

#[derive(Clone)]
pub(super) struct ClosureDefinition {
    environment: Arc<RecordType>,
    captures: Vec<(String, Type, bool)>,
    moving: bool,
    capabilities: Vec<Option<CallableKind>>,
}

pub(super) fn children(expr: &Expr) -> Vec<&Expr> {
    expr.children()
}

fn binding_names(pattern: &crate::parser::ast::BindingPattern, names: &mut Vec<String>) {
    use crate::parser::ast::BindingPattern;
    match pattern {
        BindingPattern::Sequence(p) => {
            for item in p.prefix.iter().chain(&p.suffix) {
                binding_names(item, names);
            }
            if let Some(Some(name)) = &p.rest {
                names.push(name.clone());
            }
        }
        BindingPattern::Name { name, .. } => names.push(name.clone()),
        BindingPattern::Tuple { elements, .. } => {
            for p in elements {
                binding_names(p, names);
            }
        }
        BindingPattern::Record { inner, .. } => {
            for f in &inner.fields {
                binding_names(&f.pattern, names);
            }
        }
        BindingPattern::Discard { .. } => {}
    }
}

/// Bound lexical traversal prevents shadowed locals from becoming captures or permissions.
fn walk_free(
    function: &AnonymousFunction,
    mut inspect: impl FnMut(&Expr, &HashSet<String>),
) -> Result<(), Error> {
    fn visit(
        expr: &Expr,
        bound: &mut HashSet<String>,
        inspect: &mut impl FnMut(&Expr, &HashSet<String>),
        depth: usize,
        budget: &mut usize,
    ) -> Result<(), Error> {
        if depth > 256 || *budget == 0 {
            return Err(type_error(
                &expr.span(),
                ErrorCode::ResourceLimit,
                "Closure capture analysis exceeds its compilation budget",
            ));
        }
        *budget -= 1;
        inspect(expr, bound);
        let mut descend = |expr: &Expr, bound: &mut HashSet<String>| {
            visit(expr, bound, inspect, depth + 1, budget)
        };
        match expr {
            Expr::AnonymousFunction(f) => {
                let mut inner = bound.clone();
                inner.extend(f.params.iter().map(|p| p.name.clone()));
                descend(&f.body, &mut inner)?;
            }
            Expr::Let { name, value, .. } | Expr::Var { name, value, .. } => {
                descend(value, bound)?;
                bound.insert(name.clone());
            }
            Expr::Destructure { pattern, value, .. } => {
                descend(value, bound)?;
                let mut names = Vec::new();
                binding_names(pattern, &mut names);
                bound.extend(names);
            }
            Expr::Match { value, arms, .. } => {
                descend(value, bound)?;
                for arm in arms {
                    let mut inner = bound.clone();
                    if let Some(p) = arm.pattern.payload() {
                        let mut names = Vec::new();
                        p.names(&mut names);
                        inner.extend(names);
                    }
                    descend(&arm.body, &mut inner)?;
                }
            }
            Expr::Block { .. } | Expr::Unsafe { .. } | Expr::Graph { .. } => {
                let mut inner = bound.clone();
                for child in children(expr) {
                    descend(child, &mut inner)?;
                }
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                descend(condition, &mut bound.clone())?;
                descend(then_branch, &mut bound.clone())?;
                if let Some(other) = else_branch {
                    descend(other, &mut bound.clone())?;
                }
            }
            Expr::IndexedLoop(loop_) => {
                let mut inner = bound.clone();
                inner.insert(loop_.name.clone());
                descend(&loop_.body, &mut inner)?;
            }
            Expr::While {
                condition, body, ..
            } => {
                descend(condition, &mut bound.clone())?;
                descend(body, &mut bound.clone())?;
            }
            _ => {
                for child in children(expr) {
                    descend(child, bound)?;
                }
            }
        }
        Ok(())
    }
    let mut bound = function.params.iter().map(|p| p.name.clone()).collect();
    visit(&function.body, &mut bound, &mut inspect, 0, &mut 1_000_000)
}

pub(super) fn free_names(
    function: &AnonymousFunction,
    outer: &HashMap<String, Binding>,
) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    walk_free(function, |expr, bound| {
        let name = match expr {
            Expr::Ident { name, .. } | Expr::Assign { name, .. } => Some(name),
            Expr::Call { callee, .. } => Some(callee),
            _ => None,
        };
        if let Some(name) = name {
            let root = name.split('.').next().unwrap();
            if outer.contains_key(root) && !bound.contains(root) && !names.iter().any(|n| n == root)
            {
                names.push(root.to_owned());
            }
        }
    })?;
    Ok(names)
}

pub(super) fn capturing_function_pointer_error(span: Span) -> Error {
    type_error(
        &span,
        ErrorCode::TypeMismatch,
        "A capturing closure cannot be used as a function pointer; use a named function or a noncapturing closure. If the callback needs local state, use an explicit loop",
    )
}

pub(super) fn callable_signature(ty: &Type) -> Option<(CallableKind, crate::FunctionSignature)> {
    match ty {
        Type::Function(f) => Some((CallableKind::Fn, f.signature.clone())),
        Type::Parameter(p) => p.callable.as_ref().map(|b| (b.mode, b.signature.clone())),
        Type::Record(r) => r.closure.as_ref().map(|c| (c.mode, c.signature.clone())),
        Type::Reference(r) => {
            let (mode, signature) = callable_signature(&r.target)?;
            (mode != CallableKind::FnOnce && (r.mutable || mode == CallableKind::Fn))
                .then_some((mode, signature))
        }
        _ => None,
    }
}

pub(super) fn callable_suspends(ty: &Type) -> bool {
    match ty {
        Type::Function(f) => f.suspends,
        Type::Parameter(p) => p.callable.as_ref().is_some_and(|b| b.suspends),
        Type::Record(r) => r.closure.as_ref().is_some_and(|c| c.suspends),
        Type::Reference(r) => callable_suspends(&r.target),
        _ => false,
    }
}

pub(super) fn callable_noescape(ty: &Type) -> bool {
    match ty {
        Type::Function(f) => f.noescape,
        Type::Parameter(p) => p.callable.as_ref().is_some_and(|b| b.noescape),
        Type::Reference(r) => callable_noescape(&r.target),
        _ => false,
    }
}

impl FunctionPlan {
    pub(super) fn replace_capture(&mut self, reference: Slot, value: Slot) -> Result<(), Error> {
        let value = self.snapshot(&Some(value))?.unwrap();
        let temporary = if value.ty.is_inline_owner() {
            let dst = self.slot(Type::reference(value.ty.clone(), true))?;
            self.emit(Op::Local {
                dst: dst.clone(),
                value: Some(value.clone()),
                target: value.ty.clone(),
            });
            self.register_owner(value.clone(), Some(dst.clone()));
            Some(dst)
        } else {
            None
        };
        self.emit(Op::Buffer(BufferOp::Swap {
            reference,
            value: value.clone(),
            place: temporary,
        }));
        self.emit(Op::EndLocal(value));
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn anonymous_function(
        &mut self,
        function: &AnonymousFunction,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        let expected_signature = expected.and_then(|ty| match ty {
            Type::Function(f) => Some(&f.signature),
            _ => ty.closure_info().map(|c| &c.signature),
        });
        let environment =
            self.prepare_closure(function, vars, functions, types, expected_signature)?;
        let captures: Vec<_> = free_names(function, vars)?
            .into_iter()
            .map(|name| {
                let binding = &vars[&name];
                (name, slot_type(&binding.slot), binding.mutable)
            })
            .collect();
        if suspends
            && !matches!(expected, Some(Type::Function(_)))
            && !environment.closure.as_ref().unwrap().storable
        {
            return Err(type_error(
                &function.span,
                ErrorCode::InvalidCall,
                "Only reference-free move closure environments can enter suspending functions",
            ));
        }
        if let Some(Type::Function(pointer)) = expected {
            let signature = &pointer.signature;
            if pointer.suspends != function.suspends {
                return Err(type_error(
                    &function.span,
                    ErrorCode::TypeMismatch,
                    "Closure suspension effect does not match its expected function type",
                ));
            }
            if !captures.is_empty() {
                return Err(capturing_function_pointer_error(function.span));
            }
            let dst = self.slot(Type::Function(Arc::new(crate::core::types::FunctionType {
                signature: signature.clone(),
                suspends: function.suspends,
                noescape: pointer.noescape,
            })))?;
            self.emit(Op::FunctionRef(
                dst.clone(),
                environment
                    .closure
                    .as_ref()
                    .unwrap()
                    .pointer_entry
                    .clone()
                    .unwrap(),
            ));
            return Ok(Flow::Value(Some(dst)));
        }
        let dst = self.slot(Type::Record(environment.clone()))?;
        let place = self.slot(Type::reference(dst.ty.clone(), true))?;
        self.emit(Op::Local {
            dst: place.clone(),
            value: None,
            target: dst.ty.clone(),
        });
        self.closure_places.insert(dst.index, place.clone());
        self.emit(Op::Literal(
            dst.scalars().next().unwrap(),
            if environment.closure.as_ref().unwrap().storable {
                Constant::NullPtr
            } else {
                Constant::UInt8(0)
            },
        ));
        for (index, (name, _, _)) in captures.iter().enumerate() {
            let expression = Expr::Ident {
                name: name.clone(),
                span: function.span,
            };
            let Flow::Value(value) = (if function.moving {
                self.expr(&expression, &mut vars.clone(), functions, false, types)?
            } else {
                let mut captured = vars.clone();
                if slot_type(&captured[name].slot)
                    .reference_type()
                    .is_some_and(|r| r.mutable)
                {
                    captured.get_mut(name).unwrap().mutable = true;
                }
                self.borrow_local(
                    &expression,
                    environment.fields[index + 1]
                        .ty
                        .reference_type()
                        .unwrap()
                        .mutable,
                    &captured,
                    false,
                    function.span,
                )?
            }) else {
                return Ok(Flow::Diverged);
            };
            if let Some(value) = value {
                self.emit(Op::Copy(
                    dst.field_path(&[format!("$capture{index}")]),
                    value,
                ));
            }
        }
        self.emit(Op::Local {
            dst: place.clone(),
            value: Some(dst.clone()),
            target: dst.ty.clone(),
        });
        if dst.ty.is_owner() {
            self.register_owner(dst.clone(), Some(place));
        }
        self.emit(Op::Closure(Box::new(ClosureOp::Init(dst.clone()))));
        Ok(Flow::Value(Some(dst)))
    }

    pub(super) fn prepare_closure(
        &self,
        function: &AnonymousFunction,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        expected: Option<&crate::FunctionSignature>,
    ) -> Result<Arc<RecordType>, Error> {
        let captures: Vec<_> = free_names(function, vars)?
            .into_iter()
            .map(|name| {
                let binding = &vars[&name];
                (name, slot_type(&binding.slot), binding.mutable)
            })
            .collect();
        if captures.iter().any(|(_, ty, _)| ty.contains_dyn_view()) {
            return Err(type_error(
                &function.span,
                ErrorCode::TypeMismatch,
                "Dyn views cannot be captured by closures",
            ));
        }
        let capabilities: Vec<_> = captures
            .iter()
            .map(|(name, _, _)| {
                vars[name]
                    .slot
                    .as_ref()
                    .and_then(|s| self.callable_modes.get(&s.index).copied())
            })
            .collect();
        let expected_signature = expected;
        if let Some(signature) = expected_signature {
            if signature.params.len() != function.params.len() {
                return Err(type_error(
                    &function.span,
                    ErrorCode::TypeMismatch,
                    "Anonymous function parameter count does not match its expected signature",
                ));
            }
        }
        let mut hints = vars.clone();
        let mut params = Vec::new();
        for (index, parameter) in function.params.iter().enumerate() {
            let ty = parameter.ty.clone().or_else(|| expected_signature.map(|s| s.params[index].clone())).ok_or_else(|| type_error(&parameter.span, ErrorCode::TypeMismatch, "Anonymous function parameter needs a type annotation or expected signature"))?;
            if expected_signature.is_some_and(|s| s.params[index] != ty) {
                return Err(type_error(
                    &parameter.span,
                    ErrorCode::TypeMismatch,
                    "Anonymous function parameter type mismatch",
                ));
            }
            types
                .validate(&ty)
                .map_err(|e| e.with_span(parameter.span))?;
            hints.insert(parameter.name.clone(), payload_binding(&ty, 0));
            params.push(Param {
                mutable: parameter.mutable,
                name: parameter.name.clone(),
                ty,
                span: parameter.span,
            });
        }
        let ret = function
            .ret
            .clone()
            .or_else(|| expected_signature.map(|s| s.ret.clone()))
            .or_else(|| self.bound_hint(&function.body, &hints, functions, types))
            .ok_or_else(|| {
                type_error(
                    &function.span,
                    ErrorCode::TypeMismatch,
                    "Anonymous function return type needs an annotation or expected signature",
                )
            })?;
        if expected_signature.is_some_and(|s| s.ret != ret) {
            return Err(type_error(
                &function.span,
                ErrorCode::TypeMismatch,
                "Anonymous function return type mismatch",
            ));
        }
        // Signature-only hints cannot name an environment returned by a nested
        // literal. Let its checked body supply the concrete return identity.
        let infer_return =
            function.ret.is_none() && expected_signature.is_none() && ret.contains_functions();
        let signature = crate::FunctionSignature::new(
            params.iter().map(|p| p.ty.clone()).collect(),
            ret.clone(),
        );
        let cached = self
            .generics
            .borrow()
            .closures
            .iter()
            .find(|c| c.parent == self.source_function && c.site == function.span)
            .cloned();
        let environment = if let Some(cached) = cached {
            let previous = &cached.environment.closure.as_ref().unwrap().signature;
            if previous.params != signature.params || !infer_return && previous.ret != signature.ret
            {
                return Err(type_error(
                    &function.span,
                    ErrorCode::TypeMismatch,
                    "Anonymous function already has a different inferred signature",
                ));
            }
            cached.environment
        } else {
            let count = self.generics.borrow().closures.len();
            self.generics.borrow().check_generation(
                if captures.is_empty() { 2 } else { 1 },
                self.instantiation_depth + 1,
                function.span,
            )?;
            let entry = format!("__joky_closure_{count}_{}", self.source_function);
            let mut pointer_entry = captures.is_empty().then(|| format!("{entry}_pointer"));
            let mut mutations = HashSet::new();
            walk_free(function, |expr, bound| {
                let local = vars
                    .iter()
                    .filter(|(n, _)| !bound.contains(*n))
                    .map(|(n, b)| (n.clone(), b.clone()))
                    .collect();
                let lowered = self
                    .member_call(expr, &local, functions, types)
                    .ok()
                    .flatten();
                let mut pending = vec![lowered.as_ref().unwrap_or(expr)];
                while let Some(expr) = pending.pop() {
                    let root = match expr {
                        Expr::Assign { name, .. } => Some(name.clone()),
                        Expr::FieldAssign { target, .. }
                        | Expr::Unary {
                            op: UnaryOp::BorrowMut,
                            operand: target,
                            ..
                        } => {
                            let mut target = target.as_ref();
                            while let Expr::Field { base, .. } | Expr::Index { base, .. } = target {
                                target = base;
                            }
                            if let Expr::Unary {
                                op: UnaryOp::Deref,
                                operand,
                                ..
                            } = target
                            {
                                expression_path(operand)
                            } else {
                                expression_path(target)
                            }
                        }
                        Expr::Call { callee, .. }
                            if captures.iter().any(|(n, ty, _)| {
                                n == callee
                                    && callable_signature(ty)
                                        .is_some_and(|(m, _)| m == CallableKind::FnMut)
                            }) =>
                        {
                            Some(callee.clone())
                        }
                        _ => None,
                    };
                    if let Some(root) = root {
                        let root = root.split('.').next().unwrap();
                        if !bound.contains(root) && captures.iter().any(|(name, _, _)| name == root)
                        {
                            mutations.insert(root.to_owned());
                        }
                    }
                    if let Expr::Call { callee, args, .. } = expr {
                        for (index, arg) in args.iter().enumerate() {
                            let exclusive = self
                                .function_name(callee)
                                .and_then(|n| functions.get(&n))
                                .and_then(|f| f.signature.params.get(index))
                                .is_some_and(|t| t.reference_type().is_some_and(|r| r.mutable));
                            if exclusive {
                                if let Some(root) = expression_path(arg) {
                                    let root = root.split('.').next().unwrap();
                                    if !bound.contains(root)
                                        && captures.iter().any(|(n, _, _)| n == root)
                                    {
                                        mutations.insert(root.to_owned());
                                    }
                                }
                            }
                        }
                    }
                    // The main lexical visitor handles original children with their scopes.
                    if lowered.is_some() {
                        pending.extend(children(expr));
                    }
                }
            })?;
            let mut mode = if function.suspends {
                CallableKind::FnOnce
            } else if !mutations.is_empty() {
                CallableKind::FnMut
            } else {
                CallableKind::Fn
            };
            let mut storable = function.moving;
            let mut pending: Vec<_> = captures.iter().map(|(_, ty, _)| ty.clone()).collect();
            let mut checked = Vec::new();
            let mut budget = 65_536usize;
            while storable {
                let Some(ty) = pending.pop() else {
                    break;
                };
                budget = budget.checked_sub(checked.len().max(1)).ok_or_else(|| {
                    type_error(
                        &function.span,
                        ErrorCode::ResourceLimit,
                        "Closure storage analysis exceeds its compilation budget",
                    )
                })?;
                if checked.contains(&ty) {
                    continue;
                }
                checked.push(ty.clone());
                let ty = self.pointee(&ty, types)?;
                storable = !ty.contains_references() && !ty.contains_unstorable_closures();
                pending.extend(ty.buffer_elements().iter().map(|ty| (**ty).clone()));
            }
            if function.suspends && !storable {
                return Err(type_error(
                    &function.span,
                    ErrorCode::TypeMismatch,
                    "Suspending closures require reference-free move captures",
                ));
            }
            // A null owner lane gives even an empty movable environment a stable
            // cleanup/move identity. It never allocates or owns a buffer payload.
            let identity = if storable {
                Type::Buffer(Arc::new(Type::Unit))
            } else {
                Type::UInt8
            };
            let mut fields = vec![RecordField {
                private: true,
                name: "$identity".into(),
                ty: identity.clone(),
                offset: 0,
            }];
            let mut scalars = vec![identity.clone()];
            let mut owners = identity.buffer_elements().to_vec();
            let mut depth = 1;
            for (index, (name, ty, _)) in captures.iter().enumerate() {
                if ty.contains_fixed_array() {
                    return Err(type_error(
                        &function.span,
                        ErrorCode::TypeMismatch,
                        "FixedArray captures are not supported",
                    ));
                }
                let ty = if function.moving {
                    ty.clone()
                } else {
                    Type::reference(ty.clone(), mutations.contains(name))
                };
                if ty.is_owner() {
                    owners.extend_from_slice(ty.buffer_elements());
                }
                depth = depth.max(1 + ty.layout_depth());
                fields.push(RecordField {
                    private: true,
                    name: format!("$capture{index}"),
                    offset: scalars.len(),
                    ty: ty.clone(),
                });
                scalars.extend_from_slice(ty.scalar_types());
            }
            if scalars.len() > 65536 || depth > 256 {
                return Err(type_error(
                    &function.span,
                    ErrorCode::ResourceLimit,
                    "Closure environment exceeds its layout budget",
                ));
            }
            let make_environment = |mode| {
                Arc::new(RecordType {
                    custom_drop: false,
                    array: false,
                    list: false,
                    map: false,
                    set: false,
                    closure: Some(Arc::new(ClosureInfo {
                        suspends: function.suspends,
                        storable,
                        mode,
                        signature: signature.clone(),
                        entry: entry.clone(),
                        pointer_entry: pointer_entry.clone(),
                    })),
                    owners: owners.clone(),
                    repr_c: false,
                    indirect: false,
                    id: crate::core::types::fresh_type_id(),
                    name: format!("<closure:{count}>"),
                    parameters: vec![],
                    arguments: vec![],
                    template: None,
                    fields: fields
                        .iter()
                        .map(|f| RecordField {
                            private: f.private,
                            name: f.name.clone(),
                            ty: f.ty.clone(),
                            offset: f.offset,
                        })
                        .collect(),
                    scalars: scalars.clone(),
                    depth,
                })
            };
            let mut environment = make_environment(mode);
            let mut inputs = params.clone();
            inputs.insert(
                0,
                Param {
                    mutable: false,
                    name: "$environment".into(),
                    ty: if mode == CallableKind::FnOnce {
                        Type::Record(environment.clone())
                    } else {
                        Type::reference(
                            Type::Record(environment.clone()),
                            mode == CallableKind::FnMut,
                        )
                    },
                    span: function.span,
                },
            );
            let mut definition = FnDef {
                every: None,
                kind: crate::parser::ast::FunctionKind::Ordinary,
                noescape: None,
                public: false,
                buffer_intrinsic: None,
                compiler_intrinsic: None,
                buffer_element: None,
                scope: self.definition_scope.clone(),
                name: entry.clone(),
                type_params: vec![],
                params: inputs,
                ret_ty: ret.clone(),
                suspends: function.suspends,
                body: function.body.clone(),
                span: function.span,
            };
            let mut info = functions
                .get(&self.source_function)
                .cloned()
                .or_else(|| {
                    self.generics
                        .borrow()
                        .generated_functions
                        .get(&self.source_function)
                        .cloned()
                })
                .unwrap();
            info.closure_entry = true;
            info.noescape = false;
            info.noescape_params = vec![0];
            info.inferred_noescape.clear();
            info.inferred_retained = None;
            info.infer_noescape = true;
            info.internal = true;
            info.instance_entry = false;
            info.domain_entry = false;
            info.suspends = function.suspends;
            info.compiler_intrinsic = None;
            info.may_fail = false;
            info.uses_arena = false;
            info.needs_arena = true;
            info.span = function.span;
            let mut planned = None;
            if let Some(name) = &pointer_entry {
                let mut pointer_info = info.clone();
                pointer_info.closure_entry = false;
                pointer_info.signature = signature.clone();
                pointer_info.noescape_params.clear();
                self.generics
                    .borrow_mut()
                    .generated_functions
                    .insert(name.clone(), pointer_info);
            }
            let saved = {
                let c = self.generics.borrow();
                (
                    c.closures.clone(),
                    c.closure_definitions.clone(),
                    c.generated_functions.clone(),
                    c.generated_bodies.len(),
                    c.instances.len(),
                    c.noescape_closures.clone(),
                )
            };
            for attempt in 0..2 {
                info.signature = crate::FunctionSignature::new(
                    definition.params.iter().map(|p| p.ty.clone()).collect(),
                    ret.clone(),
                );
                self.generics
                    .borrow_mut()
                    .generated_functions
                    .insert(entry.clone(), info.clone());
                self.generics.borrow_mut().closure_definitions.insert(
                    entry.clone(),
                    ClosureDefinition {
                        environment: environment.clone(),
                        captures: captures.clone(),
                        moving: function.moving,
                        capabilities: capabilities.clone(),
                    },
                );
                match FunctionPlan::new_with_return_inference(
                    &definition,
                    functions,
                    types,
                    &self.nominals,
                    self.generics.clone(),
                    self.instantiation_depth + 1,
                    infer_return,
                ) {
                    Ok(plan) => {
                        planned = Some(plan);
                        break;
                    }
                    Err(error)
                        if attempt == 0
                            && function.moving
                            && error.message()
                                == "Move capture requires a consuming closure entry" =>
                    {
                        {
                            let mut c = self.generics.borrow_mut();
                            c.closures.clone_from(&saved.0);
                            c.closure_definitions.clone_from(&saved.1);
                            c.generated_functions.clone_from(&saved.2);
                            c.generated_bodies.truncate(saved.3);
                            c.instances.truncate(saved.4);
                            c.noescape_closures.clone_from(&saved.5);
                        }
                        mode = CallableKind::FnOnce;
                        environment = make_environment(mode);
                        definition.params[0].ty = Type::Record(environment.clone());
                    }
                    Err(error) => return Err(error),
                }
            }
            let return_type = planned.as_ref().unwrap().return_type.clone();
            // An inferred environment has no source-spellable function-pointer
            // signature. Do not re-plan a second copy with a different identity.
            if return_type.contains_closures() {
                if let Some(name) = pointer_entry.take() {
                    self.generics.borrow_mut().generated_functions.remove(&name);
                }
                Arc::make_mut(Arc::make_mut(&mut environment).closure.as_mut().unwrap())
                    .pointer_entry = None;
            }
            info.signature.ret = return_type.clone();
            definition.ret_ty = return_type.clone();
            Arc::make_mut(Arc::make_mut(&mut environment).closure.as_mut().unwrap())
                .signature
                .ret = return_type;
            self.generics
                .borrow_mut()
                .generated_functions
                .insert(entry.clone(), info.clone());
            self.generics
                .borrow_mut()
                .generated_bodies
                .push(NestedTask {
                    name: entry,
                    info: info.clone(),
                    plan: planned.unwrap(),
                });
            if let Some(name) = pointer_entry {
                definition.name = name.clone();
                definition.params.remove(0);
                info.closure_entry = false;
                info.noescape_params.clear();
                info.signature = environment.closure.as_ref().unwrap().signature.clone();
                self.generics
                    .borrow_mut()
                    .generated_functions
                    .insert(name.clone(), info.clone());
                let plan = FunctionPlan::new(
                    &definition,
                    functions,
                    types,
                    &self.nominals,
                    self.generics.clone(),
                    self.instantiation_depth + 1,
                )?;
                self.generics
                    .borrow_mut()
                    .generated_bodies
                    .push(NestedTask { name, info, plan });
            }
            self.generics
                .borrow_mut()
                .closures
                .push(super::generics::ClosureInstance {
                    parent: self.source_function.clone(),
                    site: function.span,
                    environment: environment.clone(),
                });
            environment
        };
        Ok(environment)
    }

    pub(super) fn bind_closure_captures(
        &mut self,
        vars: &mut HashMap<String, Binding>,
    ) -> Result<(), Error> {
        let definition = self
            .generics
            .borrow()
            .closure_definitions
            .get(&self.source_function)
            .cloned();
        let Some(definition) = definition else {
            return Ok(());
        };
        let mode = definition.environment.closure.as_ref().unwrap().mode;
        let environment = Type::Record(definition.environment.clone());
        let source = Slot {
            index: 0,
            ty: if mode == CallableKind::FnOnce {
                environment.clone()
            } else {
                Type::reference(environment.clone(), mode == CallableKind::FnMut)
            },
        };
        self.consuming_captures = definition.moving;
        for (index, (name, ty, mutable)) in definition.captures.iter().enumerate() {
            let field = format!("$capture{index}");
            if mode == CallableKind::FnOnce {
                let selected = source.field_path(std::slice::from_ref(&field));
                if let Some(mode) = definition.capabilities[index] {
                    self.callable_modes.insert(selected.index, mode);
                }
                let projection = self.slot(Type::reference(ty.clone(), true))?;
                self.emit(Op::Project {
                    dst: projection.clone(),
                    src: vars["$environment"].place.clone().unwrap(),
                    path: vec![field.clone()],
                    reservation: false,
                });
                if ty.is_owner() || ty.is_closure_value() {
                    self.captured_fields
                        .insert(selected.index, (source.clone(), vec![field]));
                }
                if ty.is_closure_value() {
                    self.emit(Op::Closure(Box::new(ClosureOp::Init(selected.clone()))));
                }
                vars.insert(
                    name.clone(),
                    Binding {
                        slot: (*ty != Type::Unit).then_some(selected),
                        mutable: *mutable,
                        place: Some(projection),
                    },
                );
            } else {
                let field_ty = &definition.environment.fields[index + 1].ty;
                let projection = self.slot(Type::reference(
                    field_ty.clone(),
                    mode == CallableKind::FnMut,
                ))?;
                self.emit(Op::Project {
                    dst: projection.clone(),
                    src: source.clone(),
                    path: vec![field],
                    reservation: false,
                });
                let place = if definition.moving {
                    projection
                } else {
                    self.read_local(projection)?.unwrap()
                };
                let slot = if *ty == Type::Unit {
                    None
                } else {
                    let slot = Slot {
                        index: self.slot_count,
                        ty: ty.clone(),
                    };
                    self.slot_count += ty.scalar_types().len();
                    if let Some(mode) = definition.capabilities[index] {
                        self.callable_modes.insert(slot.index, mode);
                    }
                    if ty.is_closure_value() {
                        self.emit(Op::Closure(Box::new(ClosureOp::Init(slot.clone()))));
                    }
                    if ty.is_owner() || ty.is_closure_value() {
                        self.captured_owners.insert(slot.index);
                        self.emit(Op::Closure(Box::new(ClosureOp::BorrowCapture {
                            dst: slot.clone(),
                            reference: place.clone(),
                        })));
                    }
                    Some(slot)
                };
                vars.insert(
                    name.clone(),
                    Binding {
                        slot,
                        mutable: *mutable && mode == CallableKind::FnMut,
                        place: Some(place),
                    },
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke_callable(
        &mut self,
        value: Slot,
        args: &[Expr],
        span: Span,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        // A borrowed function pointer is still an indirect call, not a closure
        // environment. Preserve its noescape contract when loading the pointer.
        if value
            .ty
            .reference_type()
            .is_some_and(|r| matches!(r.target, Type::Function(_)))
        {
            let pointer = self.read_local(value)?.unwrap();
            return self
                .invoke_function_value(pointer, args, span, vars, functions, suspends, types);
        }
        if suspends && value.ty.contains_unstorable_closures() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Only reference-free move closure environments can enter suspending functions",
            ));
        }
        let value = if self.captured_fields.contains_key(&value.index)
            && self
                .callable_modes
                .get(&value.index)
                .copied()
                .or_else(|| callable_signature(&value.ty).map(|(m, _)| m))
                == Some(CallableKind::FnOnce)
        {
            let place = vars
                .values()
                .find(|b| b.slot.as_ref().is_some_and(|s| s.index == value.index))
                .and_then(|b| b.place.clone())
                .unwrap();
            self.read_owner(place, &value, vec![], value.ty.clone())?
                .unwrap()
        } else {
            value
        };
        let (actual_mode, signature) = callable_signature(&value.ty).ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::InvalidCall,
                "The reference does not provide the required callable capability",
            )
        })?;
        let mode = self
            .callable_modes
            .get(&value.index)
            .copied()
            .unwrap_or(actual_mode);
        if mode == CallableKind::FnOnce && self.captured_owners.contains(&value.index) {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                if self.consuming_captures {
                    "Move capture requires a consuming closure entry"
                } else {
                    "Borrowed closure captures cannot consume non-Copy values; use move fn"
                },
            ));
        }
        let callee_suspends = callable_suspends(&value.ty);
        if callee_suspends && !suspends {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Caller of a suspending callable must be marked @suspends",
            ));
        }
        let noescape = callable_noescape(&value.ty);
        let concrete = value
            .ty
            .reference_type()
            .map_or(&value.ty, |r| &r.target)
            .closure_info()
            .cloned();
        let mut consumed = None;
        let environment = if value.ty.reference_type().is_some() {
            if mode == CallableKind::FnOnce {
                return Err(type_error(
                    &span,
                    ErrorCode::InvalidCall,
                    "FnOnce invocation requires an owned callable",
                ));
            }
            value
        } else {
            self.emit(Op::Closure(Box::new(ClosureOp::Access(value.clone()))));
            let value = if mode == CallableKind::FnOnce {
                let value = self.snapshot(&Some(value))?.unwrap();
                self.emit(Op::Closure(Box::new(ClosureOp::Consume(value.clone()))));
                if actual_mode == CallableKind::FnOnce {
                    value
                } else {
                    consumed = Some(value.clone());
                    value
                }
            } else {
                value
            };
            if actual_mode == CallableKind::FnOnce {
                value
            } else {
                let binding = vars
                    .iter()
                    .find(|(_, b)| b.slot.as_ref().is_some_and(|s| s.index == value.index))
                    .map(|(name, _)| name.clone());
                let name = binding.unwrap_or_else(|| format!("$closure_call_{}", self.slot_count));
                if !vars.contains_key(&name) {
                    let binding = self.local_binding(
                        &name,
                        Binding {
                            slot: Some(value.clone()),
                            mutable: true,
                            place: None,
                        },
                    )?;
                    vars.insert(name.clone(), binding);
                }
                let expression = Expr::Ident { name, span };
                let Flow::Value(Some(reference)) = self.borrow_local(
                    &expression,
                    mode == CallableKind::FnMut || actual_mode == CallableKind::FnMut,
                    vars,
                    false,
                    span,
                )?
                else {
                    unreachable!()
                };
                reference
            }
        };
        let Some(args) = self.checked_arguments(
            "callable",
            args,
            &signature.params,
            None,
            span,
            |i| args.get(i + 1),
            vars,
            functions,
            suspends,
            types,
        )?
        else {
            return Ok(Flow::Diverged);
        };
        let environment = if concrete
            .as_ref()
            .is_some_and(|c| c.mode == CallableKind::Fn)
            && environment.ty.reference_type().is_some_and(|r| r.mutable)
        {
            let expected = Type::reference(
                environment.ty.reference_type().unwrap().target.clone(),
                false,
            );
            let Flow::Value(Some(value)) =
                self.coerce_reference(Flow::Value(Some(environment)), Some(&expected))?
            else {
                unreachable!()
            };
            value
        } else {
            environment
        };
        let name = if let Some(closure) = concrete {
            closure.entry
        } else {
            let contract = crate::FunctionSignature::new(
                std::iter::once(environment.ty.clone())
                    .chain(signature.params.iter().cloned())
                    .collect(),
                signature.ret.clone(),
            );
            let contract = (contract, noescape, callee_suspends);
            let mut context = self.generics.borrow_mut();
            let index = context
                .abstract_callables
                .iter()
                .position(|s| s == &contract)
                .unwrap_or_else(|| {
                    let index = context.abstract_callables.len();
                    context.abstract_callables.push(contract);
                    index
                });
            format!("__joky_callable_{index}")
        };
        let mut arguments = Vec::new();
        extend_argument(&mut arguments, Some(environment));
        arguments.extend(args);
        let result = if signature.ret == Type::Unit {
            None
        } else {
            Some(self.slot(signature.ret.clone())?)
        };
        if let Some(dst) = result.as_ref().filter(|s| s.ty.is_place()) {
            self.emit(Op::Place(PlaceOp::Allocate(dst.clone())));
        }
        let state = callee_suspends.then(|| {
            let state = self.state_count;
            self.state_count += 1;
            state
        });
        self.emit(Op::Call {
            indirect: None,
            name,
            args: arguments,
            result: result.clone(),
            state,
        });
        if let Some(value) = consumed {
            self.emit(Op::EndLocal(value));
        }
        Ok(Flow::Value(result))
    }
}

/// Definite move validity follows the IR CFG, including loops and abrupt exits.
pub(super) fn check(plan: &FunctionPlan, info: &FunctionInfo) -> Result<(), Error> {
    fn environments(slot: &Slot, budget: &mut usize, span: Span) -> Result<Vec<usize>, Error> {
        let mut slots = vec![slot.clone()];
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        while let Some(slot) = slots.pop() {
            *budget = budget.checked_sub(1).ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::ResourceLimit,
                    "Closure move analysis exceeds its compilation budget",
                )
            })?;
            if slot.ty.is_closure_value() {
                result.push(slot.index);
            }
            if let Type::Record(record) = &slot.ty {
                if !seen.insert((slot.index, record.id)) {
                    continue;
                }
                slots.extend(
                    record
                        .fields
                        .iter()
                        .map(|field| slot.field_path(std::slice::from_ref(&field.name))),
                );
            }
        }
        Ok(result)
    }
    let mut budget = 2_000_000usize;
    let mut initial = BTreeSet::new();
    let mut index = 0;
    for ty in &info.signature.params {
        initial.extend(environments(
            &Slot {
                index,
                ty: ty.clone(),
            },
            &mut budget,
            plan.span,
        )?);
        index += ty.parameter_slots();
    }
    let mut labels = vec![plan.ops.len(); plan.label_count];
    for (pc, op) in plan.ops.iter().enumerate() {
        if let Op::Label(label) = op {
            labels[*label] = pc;
        }
    }
    let mut states = vec![None; plan.ops.len() + 1];
    states[0] = Some(initial);
    let mut pending = VecDeque::from([0]);
    while let Some(pc) = pending.pop_front() {
        if pc == plan.ops.len() {
            continue;
        }
        budget = budget.checked_sub(1).ok_or_else(|| {
            type_error(
                &plan.span,
                ErrorCode::ResourceLimit,
                "Closure move analysis exceeds its compilation budget",
            )
        })?;
        let mut live = states[pc].clone().unwrap();
        if let Op::Closure(op) = &plan.ops[pc] {
            let used = match op.as_ref() {
                ClosureOp::Init(_) | ClosureOp::End(_) | ClosureOp::BorrowCapture { .. } => None,
                ClosureOp::Move { src, .. } | ClosureOp::Access(src) | ClosureOp::Consume(src) => {
                    Some(src)
                }
            };
            if used.is_some_and(|src| !live.contains(&src.index)) {
                return Err(type_error(
                    &plan.op_spans[pc],
                    ErrorCode::InvalidAssignment,
                    "Use of a moved or uninitialized closure",
                ));
            }
            match op.as_ref() {
                ClosureOp::Init(dst) => {
                    live.insert(dst.index);
                }
                ClosureOp::Move { dst, src } => {
                    if !src.ty.is_copy_value() {
                        live.remove(&src.index);
                    }
                    live.insert(dst.index);
                }
                ClosureOp::Consume(src) | ClosureOp::End(src) => {
                    live.remove(&src.index);
                }
                ClosureOp::Access(_) | ClosureOp::BorrowCapture { .. } => {}
            }
        }
        match &plan.ops[pc] {
            Op::Call {
                result: Some(dst), ..
            }
            | Op::ReadOwner { dst: Some(dst), .. } => {
                live.extend(environments(dst, &mut budget, plan.span)?);
            }
            Op::Buffer(BufferOp::Move { dst, src }) => {
                for index in environments(src, &mut budget, plan.span)? {
                    live.remove(&index);
                }
                live.extend(environments(dst, &mut budget, plan.span)?);
            }
            Op::Buffer(
                BufferOp::Extract { dst, owner, .. } | BufferOp::Detach { dst, owner, .. },
            ) => {
                for index in environments(owner, &mut budget, plan.span)? {
                    live.remove(&index);
                }
                live.extend(environments(dst, &mut budget, plan.span)?);
            }
            Op::Buffer(BufferOp::End(src)) => {
                for index in environments(src, &mut budget, plan.span)? {
                    live.remove(&index);
                }
            }
            _ => {}
        }
        let successors = match &plan.ops[pc] {
            Op::Panic { .. } => vec![],
            Op::Jump(label) => vec![labels[*label]],
            Op::Branch {
                then_label,
                else_label,
                ..
            } => vec![labels[*then_label], labels[*else_label]],
            _ => vec![pc + 1],
        };
        for next in successors {
            let joined = states[next].as_ref().map_or_else(
                || live.clone(),
                |old| old.intersection(&live).copied().collect(),
            );
            if states[next].as_ref() != Some(&joined) {
                states[next] = Some(joined);
                pending.push_back(next);
            }
        }
    }
    Ok(())
}
