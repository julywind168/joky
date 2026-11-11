//! Loop indices infer one integer type from the complete body before emitting IR.
use super::*;
use crate::parser::ast::{BindingPattern, IndexedLoop};
use std::collections::HashSet;

struct Inference<'a> {
    plan: &'a FunctionPlan,
    functions: &'a HashMap<String, FunctionInfo>,
    types: &'a TypeRegistry,
    remaining: std::cell::Cell<usize>,
}

impl FunctionPlan {
    pub(super) fn indexed_loop(
        &mut self,
        loop_: &IndexedLoop,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        let inference = Inference {
            plan: self,
            functions,
            types,
            remaining: std::cell::Cell::new(100_000),
        };
        let ty = inference.infer(loop_, vars, 0)?;
        let integer = ty.integer().unwrap();
        let maximum = u64::MAX >> (64 - integer.bits() + u32::from(integer.signed()));
        let span = loop_.span;
        let ident = |name: &str| Expr::Ident {
            name: name.into(),
            span,
        };
        let literal = |literal| Expr::Literal { literal, span };
        let panic = Expr::Call {
            callee: "$joky_check.panic".into(),
            type_args: vec![],
            args: vec![literal(Literal::String("For index overflow".into()))],
            span,
        };
        let guard = |condition| Expr::If {
            condition: Box::new(condition),
            then_branch: Box::new(panic.clone()),
            else_branch: None,
            span,
        };
        let mut exprs = vec![guard(ident(&loop_.overflow))];
        if maximum != u64::MAX {
            exprs.push(guard(Expr::BinOp {
                op: BinOp::Gt,
                lhs: Box::new(ident(&loop_.counter)),
                rhs: Box::new(literal(Literal::UInt64(maximum))),
                span,
            }));
        }
        if loop_.name != "_" {
            exprs.push(Expr::Let {
                name: loop_.name.clone(),
                annotation: Some(ty.clone()),
                value: Box::new(Expr::Call {
                    callee: ty.to_string().to_lowercase(),
                    type_args: vec![],
                    args: vec![ident(&loop_.counter)],
                    span,
                }),
                span,
            });
        }
        exprs.push(Expr::If {
            condition: Box::new(Expr::BinOp {
                op: BinOp::Eq,
                lhs: Box::new(ident(&loop_.counter)),
                rhs: Box::new(literal(Literal::UInt64(u64::MAX))),
                span,
            }),
            then_branch: Box::new(Expr::Assign {
                name: loop_.overflow.clone(),
                op: None,
                value: Box::new(literal(Literal::Bool(true))),
                span,
            }),
            else_branch: Some(Box::new(Expr::Assign {
                name: loop_.counter.clone(),
                op: Some(BinOp::Add),
                value: Box::new(literal(Literal::UInt64(1))),
                span,
            })),
            span,
        });
        exprs.push(loop_.body.clone());
        self.expr(
            &Expr::Block { exprs, span },
            vars,
            functions,
            suspends,
            types,
        )
    }
}

impl Inference<'_> {
    fn hint(&self, expr: &Expr, vars: &HashMap<String, Binding>) -> Option<Type> {
        self.plan.type_hint(expr, vars, self.functions, self.types)
    }
    fn bind(vars: &mut HashMap<String, Binding>, name: &str, ty: Type) {
        vars.insert(name.into(), payload_binding(&ty, 0));
    }
    fn infer(
        &self,
        loop_: &IndexedLoop,
        vars: &HashMap<String, Binding>,
        depth: usize,
    ) -> Result<Type, Error> {
        let mut candidates = Vec::new();
        // Default exactly as an unconstrained integer literal; other widths are
        // selected only when constraints rule out the default.
        for ty in [
            Type::Int32,
            Type::Int8,
            Type::Int16,
            Type::Int64,
            Type::UInt8,
            Type::UInt16,
            Type::UInt32,
            Type::UInt64,
        ] {
            let mut local = vars.clone();
            Self::bind(&mut local, &loop_.name, ty.clone());
            let mut tracked = HashSet::from([loop_.name.clone()]);
            if self
                .visit(
                    &loop_.body,
                    &mut local,
                    &mut tracked,
                    None,
                    Some(&self.plan.return_type),
                    depth,
                )
                .is_ok()
            {
                if ty == Type::Int32 {
                    return Ok(ty);
                }
                candidates.push(ty);
            }
        }
        if self.remaining.get() == 0 {
            return Err(type_error(
                &loop_.span,
                ErrorCode::ResourceLimit,
                "Loop index inference exceeds its compilation budget",
            ));
        }
        if candidates.len() == 1 {
            return Ok(candidates.remove(0));
        }
        Err(type_error(&loop_.span, ErrorCode::TypeMismatch, format!("Cannot infer one integer type for loop index `{}`; uses have conflicting or ambiguous constraints", loop_.name)))
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        &self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        tracked: &mut HashSet<String>,
        expected: Option<&Type>,
        returns: Option<&Type>,
        depth: usize,
    ) -> Result<bool, ()> {
        if depth > 256 || self.remaining.get() == 0 {
            self.remaining.set(0);
            return Err(());
        }
        self.remaining.set(self.remaining.get() - 1);
        let descend = |expr: &Expr,
                       vars: &mut HashMap<String, Binding>,
                       tracked: &mut HashSet<String>,
                       expected: Option<&Type>| {
            self.visit(expr, vars, tracked, expected, returns, depth + 1)
        };
        let mut depends = false;
        if let Some(constructor) = self.plan.enum_constructor(expr, vars).ok().flatten() {
            let enumeration = self
                .plan
                .constructor_type(
                    &constructor,
                    vars,
                    self.functions,
                    self.types,
                    generics::ConstructorContext {
                        expected,
                        default: false,
                    },
                )
                .ok();
            let payload = enumeration.as_ref().and_then(|e| {
                e.variants
                    .iter()
                    .find(|v| v.name == constructor.name)
                    .and_then(|v| v.payload.as_ref())
            });
            for arg in constructor.args.unwrap_or_default() {
                depends |= descend(arg, vars, tracked, payload)?;
            }
            if depends {
                let actual = enumeration.map(Type::Enum).ok_or(())?;
                if expected.is_some_and(|t| t != &actual) {
                    return Err(());
                }
            }
            return Ok(depends);
        }
        match expr {
            Expr::Ident { name, .. } => depends = tracked.contains(name),
            Expr::Literal {
                literal:
                    Literal::Integer {
                        magnitude,
                        negative,
                    },
                span,
            } => {
                super::inference::integer_literal(*magnitude, *negative, expected, *span)
                    .map_err(|_| ())?;
            }
            Expr::Let {
                name,
                annotation,
                value,
                ..
            }
            | Expr::Var {
                name,
                annotation,
                value,
                ..
            } => {
                let dependent = descend(value, vars, tracked, annotation.as_ref())?;
                self.plan
                    .hint_bindings(expr, vars, self.functions, self.types);
                tracked.remove(name);
                if dependent {
                    tracked.insert(name.clone());
                }
            }
            Expr::Destructure {
                pattern,
                annotation,
                value,
                ..
            } => {
                let dependent = descend(value, vars, tracked, annotation.as_ref())?;
                self.plan
                    .hint_bindings(expr, vars, self.functions, self.types);
                pattern_names(pattern, &mut |name| {
                    tracked.remove(name);
                    if dependent {
                        tracked.insert(name.into());
                    }
                });
            }
            Expr::LetElse(binding) => {
                let dependent = descend(
                    &binding.value,
                    &mut vars.clone(),
                    &mut tracked.clone(),
                    binding.annotation.as_ref(),
                )?;
                descend(
                    &binding.fallback,
                    &mut vars.clone(),
                    &mut tracked.clone(),
                    None,
                )?;
                self.plan
                    .hint_bindings(expr, vars, self.functions, self.types);
                for name in binding.pattern.binding_names() {
                    tracked.remove(&name);
                    if dependent {
                        tracked.insert(name);
                    }
                }
            }
            Expr::Assign { name, value, .. } => {
                let ty = vars.get(name).map(|b| slot_type(&b.slot));
                descend(value, vars, tracked, ty.as_ref())?;
            }
            Expr::FieldAssign { target, value, .. } => {
                descend(target, vars, tracked, None)?;
                let ty = self.hint(target, vars);
                descend(value, vars, tracked, ty.as_ref())?;
            }
            Expr::BinaryChain { .. } => {
                let ty = self.hint(expr, vars).or_else(|| expected.cloned());
                for operand in expr.children() {
                    depends |= descend(operand, vars, tracked, ty.as_ref())?;
                    if depends
                        && self
                            .hint(operand, vars)
                            .zip(ty.as_ref())
                            .is_some_and(|(actual, expected)| actual != *expected)
                    {
                        return Err(());
                    }
                }
            }
            Expr::IfChain { arms, fallback, .. } => {
                let ty = expected.cloned().or_else(|| self.hint(expr, vars));
                for (condition, body) in arms {
                    descend(
                        condition,
                        &mut vars.clone(),
                        &mut tracked.clone(),
                        Some(&Type::Bool),
                    )?;
                    depends |= descend(body, &mut vars.clone(), &mut tracked.clone(), ty.as_ref())?;
                }
                if let Some(body) = fallback {
                    depends |= descend(body, &mut vars.clone(), &mut tracked.clone(), ty.as_ref())?;
                }
            }
            Expr::BinOp { op, lhs, rhs, .. } => {
                let ty = self
                    .plan
                    .binary_hint(lhs, rhs, vars, self.functions, self.types)
                    .or_else(|| {
                        expected
                            .filter(|t| !op.is_comparison() && t.is_numeric())
                            .cloned()
                    });
                depends = descend(lhs, vars, tracked, ty.as_ref())?
                    | descend(rhs, vars, tracked, ty.as_ref())?;
                if depends {
                    if let (Some(left), Some(right)) = (self.hint(lhs, vars), self.hint(rhs, vars))
                    {
                        if left != right {
                            return Err(());
                        }
                    }
                }
            }
            Expr::Unary { op, operand, .. } => {
                let target = expected.and_then(Type::reference_type).map(|r| &r.target);
                let operand_type = self.hint(operand, vars);
                depends = descend(
                    operand,
                    vars,
                    tracked,
                    if matches!(op, UnaryOp::Borrow | UnaryOp::BorrowMut) {
                        target
                    } else if matches!(op, UnaryOp::Deref | UnaryOp::Unwrap | UnaryOp::Try) {
                        operand_type.as_ref()
                    } else {
                        expected
                    },
                )?;
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                if let Ok(Some(call)) =
                    self.plan
                        .member_call(expr, vars, self.functions, self.types)
                {
                    return descend(&call, vars, tracked, expected);
                }
                if let Some(local) = self.plan.local_callee(callee, vars, *span) {
                    let call = Expr::Invoke {
                        callee: Box::new(local),
                        args: args.clone(),
                        span: *span,
                    };
                    return descend(&call, vars, tracked, expected);
                }
                // Resolve using candidate types without generating IR or instantiating code.
                let signature = if let Some(name) = self.plan.function_name(callee) {
                    if self.plan.generics.borrow().templates.contains_key(&name) {
                        self.plan
                            .generic_signature(expr, vars, self.functions, self.types)
                            .map(|(_, s)| s)
                            .ok()
                    } else {
                        self.functions.get(&name).map(|f| f.signature.clone())
                    }
                } else {
                    None
                };
                for (i, arg) in args.iter().enumerate() {
                    let formal = signature.as_ref().and_then(|s| s.params.get(i));
                    let formal = formal.map(|ty| {
                        if !self
                            .hint(arg, vars)
                            .is_some_and(|t| t.reference_type().is_some())
                        {
                            ty.reference_type()
                                .filter(|r| !r.mutable)
                                .map_or(ty, |r| &r.target)
                        } else {
                            ty
                        }
                    });
                    depends |= descend(arg, vars, tracked, formal)?;
                }
                if depends
                    && signature.is_none()
                    && self.plan.function_name(callee).is_some_and(|name| {
                        self.plan.generics.borrow().templates.contains_key(&name)
                    })
                {
                    return Err(());
                }
            }
            Expr::MethodCall(_) => {
                if let Ok(Some(call)) =
                    self.plan
                        .member_call(expr, vars, self.functions, self.types)
                {
                    return descend(&call, vars, tracked, expected);
                }
                for child in super::closures::children(expr) {
                    depends |= descend(child, vars, tracked, None)?;
                }
                if depends {
                    return Err(());
                }
            }
            Expr::Invoke { callee, args, .. } => {
                descend(callee, vars, tracked, None)?;
                let signature = self.hint(callee, vars).and_then(|t| {
                    super::closures::callable_signature(&t)
                        .map(|(_, s)| s)
                        .or_else(|| {
                            if let Type::Function(f) = t {
                                Some(f.signature.clone())
                            } else {
                                None
                            }
                        })
                });
                for (i, arg) in args.iter().enumerate() {
                    depends |= descend(
                        arg,
                        vars,
                        tracked,
                        signature.as_ref().and_then(|s| s.params.get(i)),
                    )?;
                }
            }
            Expr::Return { value, .. } => {
                if let Some(value) = value {
                    descend(value, vars, tracked, returns)?;
                }
            }
            Expr::Block { exprs, .. }
            | Expr::Unsafe { exprs, .. }
            | Expr::Sequence { exprs, .. } => {
                let mut local = vars.clone();
                let mut names = tracked.clone();
                let trailing = matches!(expr, Expr::Sequence { trailing: true, .. });
                for (i, child) in exprs.iter().enumerate() {
                    let last = i + 1 == exprs.len() && !trailing;
                    let value = descend(
                        child,
                        &mut local,
                        &mut names,
                        if last { expected } else { None },
                    )?;
                    if last {
                        depends = value;
                    }
                }
                return Ok(depends);
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                descend(condition, vars, tracked, Some(&Type::Bool))?;
                let ty = expected
                    .cloned()
                    .or_else(|| self.hint(then_branch, vars))
                    .or_else(|| else_branch.as_ref().and_then(|b| self.hint(b, vars)));
                depends = descend(
                    then_branch,
                    &mut vars.clone(),
                    &mut tracked.clone(),
                    ty.as_ref(),
                )?;
                if let Some(other) = else_branch {
                    depends |=
                        descend(other, &mut vars.clone(), &mut tracked.clone(), ty.as_ref())?;
                    if depends {
                        if let (Some(left), Some(right)) =
                            (self.hint(then_branch, vars), self.hint(other, vars))
                        {
                            if left != right {
                                return Err(());
                            }
                        }
                    }
                }
            }
            Expr::Match {
                value,
                arms,
                iteration,
                span,
                ..
            } => {
                if *iteration {
                    if let Some(lowered) = self.plan.borrowed_iteration(
                        value,
                        arms,
                        vars,
                        self.functions,
                        self.types,
                        *span,
                    ) {
                        return descend(&lowered, vars, tracked, expected);
                    }
                }
                let input_depends = descend(value, vars, tracked, None)?;
                let matched = self
                    .hint(value, vars)
                    .and_then(|ty| self.plan.match_type(&ty, self.types, value.span()).ok());
                let ty = expected.cloned().or_else(|| self.hint(expr, vars));
                let mut hints = Vec::new();
                for arm in arms {
                    let mut local = vars.clone();
                    let mut names = tracked.clone();
                    if let Some((ty, borrowed)) = &matched {
                        if let Ok(pattern) =
                            self.plan.check_match_pattern(&arm.pattern, ty, self.types)
                        {
                            let mut bindings = HashMap::new();
                            pattern.hints(&mut bindings, *borrowed);
                            for name in bindings.keys() {
                                names.remove(name);
                                if input_depends {
                                    names.insert(name.clone());
                                }
                            }
                            local.extend(bindings);
                        } else if input_depends {
                            return Err(());
                        }
                    }
                    if let Some(guard) = &arm.guard {
                        let mut preview = local.clone();
                        if let Some((ty, _)) = &matched {
                            if let Ok(pattern) =
                                self.plan.check_match_pattern(&arm.pattern, ty, self.types)
                            {
                                pattern.hints(&mut preview, Some(false));
                            }
                        }
                        depends |=
                            descend(guard, &mut preview, &mut names.clone(), Some(&Type::Bool))?;
                    }
                    hints.push(self.hint(&arm.body, &local));
                    depends |= descend(&arm.body, &mut local, &mut names, ty.as_ref())?;
                }
                if depends {
                    let mut known = hints.iter().flatten();
                    if let Some(first) = known.next() {
                        if known.any(|ty| ty != first) {
                            return Err(());
                        }
                    }
                }
            }
            Expr::While {
                condition, body, ..
            } => {
                descend(
                    condition,
                    &mut vars.clone(),
                    &mut tracked.clone(),
                    Some(&Type::Bool),
                )?;
                descend(body, &mut vars.clone(), &mut tracked.clone(), None)?;
            }
            Expr::IndexedLoop(loop_) => {
                let mut local = vars.clone();
                let ty = self.infer(loop_, vars, depth + 1).map_err(|_| ())?;
                Self::bind(&mut local, &loop_.name, ty);
                let mut names = tracked.clone();
                names.remove(&loop_.name);
                descend(&loop_.body, &mut local, &mut names, None)?;
            }
            Expr::AnonymousFunction(function) => {
                let mut local = vars.clone();
                let mut names = tracked.clone();
                let signature = expected.and_then(|t| match t {
                    Type::Function(f) => Some(f.signature.clone()),
                    _ => super::closures::callable_signature(t).map(|(_, s)| s),
                });
                for (index, param) in function.params.iter().enumerate() {
                    names.remove(&param.name);
                    local.remove(&param.name);
                    if let Some(ty) = param
                        .ty
                        .as_ref()
                        .or_else(|| signature.as_ref().and_then(|s| s.params.get(index)))
                    {
                        Self::bind(&mut local, &param.name, ty.clone());
                    }
                }
                let ret = function
                    .ret
                    .as_ref()
                    .or_else(|| signature.as_ref().map(|s| &s.ret));
                self.visit(&function.body, &mut local, &mut names, ret, ret, depth + 1)?;
            }
            Expr::Tuple { elements, .. } => {
                for (i, element) in elements.iter().enumerate() {
                    let ty = if let Some(Type::Tuple(t)) = expected {
                        t.elements.get(i)
                    } else {
                        None
                    };
                    depends |= descend(element, vars, tracked, ty)?;
                }
            }
            Expr::Record { fields, base, .. } => {
                let ty = self
                    .plan
                    .record_type(
                        expr,
                        vars,
                        self.functions,
                        self.types,
                        generics::ConstructorContext {
                            expected,
                            default: false,
                        },
                    )
                    .ok();
                for field in fields {
                    let field_type = match &ty {
                        Some(Type::Record(record)) => record
                            .fields
                            .iter()
                            .find(|f| f.name == field.name)
                            .map(|f| &f.ty),
                        _ => None,
                    };
                    depends |= descend(&field.value, vars, tracked, field_type)?;
                }
                if let Some(base) = base {
                    depends |= descend(base, vars, tracked, ty.as_ref())?;
                }
            }
            Expr::Array { elements, .. } => {
                let inferred = elements.iter().find_map(|e| self.hint(e, vars));
                let ty = expected
                    .and_then(super::arrays::element_type)
                    .or(inferred.as_ref());
                for element in elements {
                    depends |= descend(element, vars, tracked, ty)?;
                }
                if depends {
                    if let Some(ty) = ty {
                        if elements
                            .iter()
                            .filter_map(|e| self.hint(e, vars))
                            .any(|t| &t != ty)
                        {
                            return Err(());
                        }
                    }
                }
            }
            Expr::Index { base, index, .. } => {
                depends = descend(base, vars, tracked, None)?;
                if let Some((start, end, _)) = super::indexing::slice_bounds(index) {
                    for bound in [start, end].into_iter().flatten() {
                        descend(bound, vars, tracked, Some(&Type::UInt64))?;
                    }
                } else {
                    descend(index, vars, tracked, Some(&Type::UInt64))?;
                }
            }
            _ => {
                for child in super::closures::children(expr) {
                    depends |= descend(child, vars, tracked, None)?;
                }
            }
        }
        if depends {
            if let (Some(expected), Some(actual)) = (expected, self.hint(expr, vars)) {
                if &actual != expected {
                    return Err(());
                }
            }
        }
        Ok(depends)
    }
}

fn pattern_names(pattern: &BindingPattern, visit: &mut impl FnMut(&str)) {
    match pattern {
        BindingPattern::Name { name, .. } => visit(name),
        BindingPattern::Tuple { elements, .. } => {
            for item in elements {
                pattern_names(item, visit);
            }
        }
        BindingPattern::Record { inner, .. } => {
            for field in &inner.fields {
                pattern_names(&field.pattern, visit);
            }
        }
        BindingPattern::Sequence(p) => {
            for item in p.prefix.iter().chain(&p.suffix) {
                pattern_names(item, visit);
            }
            if let Some(Some(name)) = &p.rest {
                visit(name);
            }
        }
        BindingPattern::Discard { .. } => {}
    }
}
