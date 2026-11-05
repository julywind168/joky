//! Contextual integer literals. Hints inspect types without evaluating expressions.
use super::*;

const MAX_HINT_STEPS: usize = 65_536;

// A query can see both the source method call and a synthesized function call
// at the same span. Verify the AST on a bucket hit, including for embedding
// callers whose hand-built nodes have empty or repeated spans.
struct HintEntry {
    expr: Expr,
    ty: Option<Type>,
}

#[derive(Default)]
pub(super) struct HintWork {
    steps: std::cell::Cell<usize>,
    exhausted: std::cell::Cell<Option<Span>>,
    session: std::cell::RefCell<Option<Box<HintSession>>>,
    // A speculative hint may fail and still gain a type from its context.
    // Collect the cause only for a checked argument whose hint stays unknown.
    diagnostic: std::cell::RefCell<Option<Result<(), Error>>>,
}

pub(super) struct SavedHintScope(Option<Box<HintSession>>);

struct HintSession {
    // Only compared while the enclosing query or checked call keeps both inputs alive.
    types: *const TypeRegistry,
    functions: *const HashMap<String, FunctionInfo>,
    vars: HashMap<String, Binding>,
    entries: HashMap<(Span, std::mem::Discriminant<Expr>, bool), HintEntry>,
}

pub(super) fn has_nested_call(expr: &Expr) -> bool {
    let call = |arg: &Expr| matches!(arg, Expr::Call { .. } | Expr::MethodCall(_));
    match expr {
        Expr::Call { args, .. } => args.iter().any(call),
        Expr::MethodCall(method) => call(&method.receiver) || method.args.iter().any(call),
        _ => false,
    }
}

// Bound retained syntax and leave closure preparation to the checked callable
// path, whose speculative environment can be rolled back and replanned.
fn cacheable_call(expr: &Expr) -> bool {
    let mut pending = vec![expr];
    let mut count = 0;
    while let Some(node) = pending.pop() {
        count += 1;
        if count > 128 || matches!(node, Expr::AnonymousFunction(_)) {
            return false;
        }
        pending.extend(node.children());
    }
    true
}

impl HintWork {
    pub(super) fn checked<T>(&self, query: impl FnOnce() -> Option<T>) -> Result<Option<T>, Error> {
        let previous = self.diagnostic.replace(Some(Ok(())));
        let value = query();
        let diagnostic = self
            .diagnostic
            .replace(previous)
            .expect("active hint query");
        if value.is_none() {
            diagnostic?;
        }
        Ok(value)
    }

    pub(super) fn observe<T>(&self, result: Result<T, Error>, span: Span) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                if let Some(cause) = self.diagnostic.borrow_mut().as_mut() {
                    if cause.is_ok() || error.code() == ErrorCode::ResourceLimit {
                        let error = if error.span().is_none_or(|s| s.start() == s.end()) {
                            error.with_span(span)
                        } else {
                            error
                        };
                        *cause = Err(error);
                    }
                }
                None
            }
        }
    }

    pub(super) fn enter(
        &self,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<SavedHintScope> {
        if self.session.borrow().as_ref().is_some_and(|s| {
            std::ptr::eq(s.types, types) && std::ptr::eq(s.functions, functions) && s.vars == *vars
        }) {
            return None;
        }
        let scope = HintSession {
            types,
            functions,
            vars: vars.clone(),
            entries: HashMap::new(),
        };
        Some(SavedHintScope(self.session.replace(Some(Box::new(scope)))))
    }

    pub(super) fn leave(&self, saved: Option<SavedHintScope>) {
        if let Some(saved) = saved {
            self.session.replace(saved.0);
        }
    }

    pub(super) fn charge(&self, span: Span) -> bool {
        if self.steps.get() >= MAX_HINT_STEPS {
            if self.exhausted.get().is_none() {
                self.exhausted.set(Some(span));
            }
            return false;
        }
        self.steps.set(self.steps.get() + 1);
        true
    }

    pub(super) fn check(&self) -> Result<(), Error> {
        if let Some(span) = self.exhausted.get() {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Type inference exceeds its function work budget",
            ));
        }
        Ok(())
    }
}

pub(super) fn integer_literal(
    magnitude: u64,
    negative: bool,
    expected: Option<&Type>,
    span: Span,
) -> Result<(Type, Constant), Error> {
    if expected.is_some_and(Type::is_float) {
        let text = format!("{}{}", if negative { "-" } else { "" }, magnitude);
        return float_literal(&text, expected, span);
    }
    use crate::core::integer::IntegerType;
    let integer = expected
        .and_then(Type::integer)
        .unwrap_or(IntegerType::Int32);
    let ty = integer.ty();
    if !integer.accepts_literal(magnitude, negative) {
        return Err(type_error(
            &span,
            ErrorCode::TypeMismatch,
            if negative && !integer.signed() {
                format!("Unary `-` is not supported for {ty}")
            } else {
                format!("Integer literal is outside {ty} range")
            },
        ));
    }
    let bits = if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    let constant = match integer {
        IntegerType::Int8 => Constant::Int8(bits as i8),
        IntegerType::Int16 => Constant::Int16(bits as i16),
        IntegerType::Int32 => Constant::Int32(bits as i32),
        IntegerType::Int64 => Constant::Int64(bits as i64),
        IntegerType::UInt8 => Constant::UInt8(bits as u8),
        IntegerType::UInt16 => Constant::UInt16(bits as u16),
        IntegerType::UInt32 => Constant::UInt32(bits as u32),
        IntegerType::UInt64 => Constant::UInt64(bits),
        IntegerType::Rune => Constant::Rune(bits as u32),
    };
    Ok((ty, constant))
}

/// A floating default cannot supply a concrete context to integer siblings.
/// Prefer their integer default so checking the float reports the mismatch.
pub(super) fn sibling_hint(
    hints: impl Iterator<Item = Option<Type>>,
    default: bool,
) -> Option<Type> {
    let mut float = None;
    for ty in hints.flatten() {
        if !default || !ty.is_float() {
            return Some(ty);
        }
        float = Some(ty);
    }
    float
}

impl FunctionPlan {
    pub(super) fn binary_hint(
        &self,
        lhs: &Expr,
        rhs: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        let left = self.type_hint(lhs, vars, functions, types);
        if left.is_some() {
            return left;
        }
        let mut local = vars.clone();
        self.hint_bindings(lhs, &mut local, functions, types);
        left.or_else(|| self.type_hint(rhs, &local, functions, types))
    }

    pub(super) fn type_hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        self.hint(expr, vars, functions, types, false)
    }

    /// One traversal behind `type_hint` (strict) and `bound_hint` (defaults
    /// allowed). With `default`, unresolved integer literals fall back to
    /// Int32, aggregate constructors may default their parameters, and operator
    /// operands, branches, and match arms are searched for any concrete leaf.
    pub(super) fn hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        default: bool,
    ) -> Option<Type> {
        if !self.hint_work.charge(expr.span()) {
            return None;
        }
        if !matches!(expr, Expr::Call { .. } | Expr::MethodCall(_))
            || self.hint_work.session.borrow().is_none() && !has_nested_call(expr)
        {
            return self.hint_inner(expr, vars, functions, types, default);
        }
        let saved = self.hint_work.enter(vars, functions, types);
        let site = (expr.span(), std::mem::discriminant(expr), default);
        if saved.is_none() {
            if let Some(entry) = self
                .hint_work
                .session
                .borrow()
                .as_ref()
                .and_then(|s| s.entries.get(&site))
            {
                if entry.expr == *expr
                    && (entry.ty.is_some() || self.hint_work.diagnostic.borrow().is_none())
                {
                    return entry.ty.clone();
                }
            }
        }
        let ty = self.hint_inner(expr, vars, functions, types, default);
        if saved.is_none() && cacheable_call(expr) {
            let mut session = self.hint_work.session.borrow_mut();
            let entries = &mut session.as_mut().expect("active hint scope").entries;
            if entries.len() < 512 {
                entries.insert(
                    site,
                    HintEntry {
                        expr: expr.clone(),
                        ty: ty.clone(),
                    },
                );
            }
        }
        self.hint_work.leave(saved);
        ty
    }

    fn hint_inner(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        default: bool,
    ) -> Option<Type> {
        // Search the whole subtree for a concrete type before defaulting any
        // leaf. This also applies inside tuples and local bindings: a default
        // in an earlier branch must not hide a later branch's known type.
        if default {
            if let Some(ty) = self.hint(expr, vars, functions, types, false) {
                return Some(ty);
            }
        }
        if let Expr::Call { callee, args, .. } = expr {
            if callee == "$reserve_receiver" {
                return self.hint(&args[0], vars, functions, types, default);
            }
        }
        if let Some(input) = self.iteration_input(expr, vars, functions, types) {
            return self.hint(&input, vars, functions, types, default);
        }
        if let Some(cursor) = super::iterators::paired_binding(expr) {
            let Type::Reference(reference) = self.type_hint(cursor, vars, functions, types)? else {
                return None;
            };
            let Type::BufferPairIterator(pair) = &reference.target else {
                return None;
            };
            return Some(Type::reference(
                Type::BufferEntry(pair.clone()),
                reference.mutable,
            ));
        }
        if let Some(constant) = self
            .hint_work
            .observe(self.associated_constant(expr, vars, types), expr.span())
            .flatten()
        {
            return Some(constant.declaration.ty.clone());
        }
        if !self.scope.constants.is_empty() {
            if let Some(key) =
                expression_path(expr).and_then(|path| self.module_constant_key(&path, vars, types))
            {
                let context = self.generics.borrow();
                let constant = context.members.constants_by_key.get(&key)?;
                return Some(constant.declaration.ty.clone());
            }
        }
        if let Some(ty) = self.dyn_hint(expr, vars, functions, types) {
            return Some(ty);
        }
        // Addition has the operand type and equality always returns Bool.
        // Selecting their methods here would recursively infer operands twice.
        if !matches!(
            expr,
            Expr::BinOp {
                op: BinOp::Add | BinOp::Eq | BinOp::Ne,
                ..
            }
        ) {
            if let Some(call) = self
                .hint_work
                .observe(self.member_call(expr, vars, functions, types), expr.span())
                .flatten()
            {
                return self.hint(&call, vars, functions, types, default);
            }
        }
        if let Some((_, info)) = self
            .hint_work
            .observe(
                self.function_value_info(expr, vars, functions, types, false),
                expr.span(),
            )
            .flatten()
        {
            return Some(info.function_type());
        }
        if let Expr::Invoke { callee, .. } = expr {
            if let Some(ty) = self.type_hint(callee, vars, functions, types) {
                if let Some((_, signature)) = super::closures::callable_signature(&ty) {
                    return Some(signature.ret);
                }
            }
            if let Some(Type::Function(signature)) = self.type_hint(callee, vars, functions, types)
            {
                return Some(signature.signature.ret.clone());
            }
        }
        if let Expr::Call { callee, span, .. } = expr {
            if let Some(value) = self.local_callee(callee, vars, *span) {
                if let Some(ty) = self.type_hint(&value, vars, functions, types) {
                    if let Some((_, signature)) = super::closures::callable_signature(&ty) {
                        return Some(signature.ret);
                    }
                }
                if let Some(Type::Function(signature)) =
                    self.type_hint(&value, vars, functions, types)
                {
                    return Some(signature.signature.ret.clone());
                }
            }
        }
        if let Some(constructor) = self
            .hint_work
            .observe(self.enum_constructor(expr, vars), expr.span())
            .flatten()
        {
            return self
                .hint_work
                .observe(
                    self.constructor_type(
                        &constructor,
                        vars,
                        functions,
                        types,
                        generics::ConstructorContext {
                            expected: None,
                            default,
                        },
                    ),
                    expr.span(),
                )
                .map(Type::Enum);
        }
        if let Expr::Call { callee, args, .. } = expr {
            if callee == "alloc" {
                return args
                    .first()
                    .and_then(|arg| self.hint(arg, vars, functions, types, default))
                    .map(|ty| Type::reference(ty, true));
            }
        }
        match expr {
            Expr::Literal { literal, .. } => match literal {
                Literal::Float(_) => default.then_some(Type::Float64),
                Literal::Float32(_) => Some(Type::Float32),
                Literal::Float64(_) => Some(Type::Float64),
                Literal::Integer { .. } => default.then_some(Type::Int32),
                Literal::Int8(_) => Some(Type::Int8),
                Literal::Int16(_) => Some(Type::Int16),
                Literal::Int32(_) => Some(Type::Int32),
                Literal::Int64(_) => Some(Type::Int64),
                Literal::UInt8(_) => Some(Type::UInt8),
                Literal::UInt16(_) => Some(Type::UInt16),
                Literal::UInt32(_) => Some(Type::UInt32),
                Literal::Rune(_) => Some(Type::Rune),
                Literal::ConstParameter(_) | Literal::UInt64(_) => Some(Type::UInt64),
                Literal::Bool(_) => Some(Type::Bool),
                Literal::String(_) => Some(Type::String),
            },
            Expr::When(block) => self.when_hint(block, vars, functions, types),
            Expr::AnonymousFunction(function) => {
                let mut local = vars.clone();
                let params = function
                    .params
                    .iter()
                    .map(|p| {
                        let ty = p.ty.clone()?;
                        local.insert(p.name.clone(), payload_binding(&ty, 0));
                        Some(ty)
                    })
                    .collect::<Option<Vec<_>>>()?;
                let ret = function
                    .ret
                    .clone()
                    .or_else(|| self.hint(&function.body, &local, functions, types, default))?;
                // Hints carry signature shape only; the checked value has its own nominal identity.
                Some(Type::function_with_effect(params, ret, function.suspends))
            }
            Expr::Return { value, .. } => value.as_ref().map_or(Some(Type::Unit), |value| {
                self.hint(value, vars, functions, types, default)
            }),
            Expr::Ident { name, .. } => vars.get(name).map(|b| slot_type(&b.slot)),
            Expr::Call { callee, .. } if callee == "float32" => Some(Type::Float32),
            Expr::Call { callee, .. } if callee == "float64" => Some(Type::Float64),
            Expr::Call { callee, .. } if callee == "Ptr.null" => Some(Type::Ptr),
            Expr::Call { callee, .. } if callee == "Ptr.is_null" => Some(Type::Bool),
            Expr::Call { callee, .. } => IntegerType::conversion(callee)
                .map(IntegerType::ty)
                .or_else(|| {
                    if self
                        .function_name(callee)
                        .is_some_and(|name| self.generics.borrow().templates.contains_key(&name))
                    {
                        self.generic_result_hint(expr, vars, functions, types, default)
                    } else {
                        self.function_name(callee)
                            .and_then(|name| functions.get(&name))
                            .filter(|f| {
                                !matches!(f.compiler_intrinsic, Some(CompilerIntrinsic::Panic))
                            })
                            .map(|f| f.signature.ret.clone())
                    }
                })
                .or_else(|| {
                    callee
                        .split_once('.')
                        .and_then(|(name, _)| self.named_type(name).cloned())
                }),
            Expr::Record { .. } => self.hint_work.observe(
                self.record_type(
                    expr,
                    vars,
                    functions,
                    types,
                    generics::ConstructorContext {
                        expected: None,
                        default,
                    },
                ),
                expr.span(),
            ),
            Expr::Index { base, index, .. } => {
                let ty = self.hint(base, vars, functions, types, default)?;
                let target = ty.reference_type().map_or(&ty, |r| &r.target);
                let element = super::arrays::element_type(target)?.clone();
                Some(if super::indexing::slice_bounds(index).is_some() {
                    Type::BufferSlice(Arc::new(element))
                } else {
                    element
                })
            }
            Expr::Field { base, name, .. } => {
                match self
                    .hint(base, vars, functions, types, default)
                    .or_else(|| {
                        if let Expr::Ident { name, .. } = base.as_ref() {
                            self.named_type(name).cloned()
                        } else {
                            None
                        }
                    })? {
                    Type::Reference(r) => self
                        .hint_work
                        .observe(self.pointee(&r.target, types), base.span())?
                        .inline_field(name)
                        .map(|(_, ty)| ty.clone()),
                    ty @ (Type::Record(_) | Type::Tuple(_)) => {
                        ty.inline_field(name).map(|(_, ty)| ty.clone())
                    }
                    Type::Enum(enumeration)
                        if enumeration
                            .variants
                            .iter()
                            .any(|variant| variant.name == *name) =>
                    {
                        Some(Type::Enum(enumeration))
                    }
                    _ => None,
                }
            }
            Expr::Unary {
                op: UnaryOp::Unwrap | UnaryOp::Try,
                operand,
                ..
            } => {
                let ty = self.hint(operand, vars, functions, types, default)?;
                let Type::Enum(enumeration) = ty else {
                    return None;
                };
                let success = unwrap::success_variant(&enumeration, types)?;
                enumeration.variants[success].payload.clone()
            }
            Expr::Unary {
                op: UnaryOp::Deref,
                operand,
                ..
            } => {
                let ty = self.hint(operand, vars, functions, types, default)?;
                self.hint_work.observe(
                    self.pointee(&ty.reference_type()?.target, types),
                    expr.span(),
                )
            }
            Expr::Unary {
                op: UnaryOp::Borrow | UnaryOp::BorrowMut,
                operand,
                ..
            } => self
                .hint(operand, vars, functions, types, default)
                .map(|ty| {
                    Type::reference(
                        ty,
                        matches!(
                            expr,
                            Expr::Unary {
                                op: UnaryOp::BorrowMut,
                                ..
                            }
                        ),
                    )
                }),
            Expr::Unary {
                op: UnaryOp::Neg | UnaryOp::BitNot,
                operand,
                ..
            } => self.hint(operand, vars, functions, types, default),
            Expr::Unary {
                op: UnaryOp::Not, ..
            }
            | Expr::Logical { .. } => Some(Type::Bool),
            Expr::BinaryChain { .. } => {
                let mut local = vars.clone();
                sibling_hint(
                    expr.children().into_iter().map(|operand| {
                        let ty = self.hint(operand, &local, functions, types, default);
                        if ty.is_none() {
                            self.hint_bindings(operand, &mut local, functions, types);
                        }
                        ty
                    }),
                    default,
                )
            }
            Expr::IfChain { arms, fallback, .. } => {
                if fallback.is_none() {
                    return Some(Type::Unit);
                }
                sibling_hint(
                    arms.iter()
                        .map(|(_, body)| body)
                        .chain(fallback.as_deref())
                        .map(|body| self.hint(body, vars, functions, types, default)),
                    default,
                )
            }
            Expr::BinOp { op, lhs, rhs, .. } => {
                if op.is_comparison() {
                    return Some(Type::Bool);
                }
                let from_operands = || {
                    if !default {
                        return None;
                    }
                    sibling_hint(
                        [lhs, rhs]
                            .into_iter()
                            .map(|e| self.hint(e, vars, functions, types, true)),
                        true,
                    )
                };
                self.binary_hint(lhs, rhs, vars, functions, types)
                    .or_else(from_operands)
            }
            Expr::Concurrent {
                kind: ConcurrentKind::Race,
                branches,
                ..
            } => branches
                .first()
                .and_then(|branch| self.hint(branch, vars, functions, types, default)),
            Expr::Isolate { body, .. } => self
                .hint(body, vars, functions, types, default)
                .and_then(|ty| {
                    self.hint_work
                        .observe(self.isolate_result(&ty, types), expr.span())
                }),
            Expr::TaskScope { body, .. } => self.hint(body, vars, functions, types, default),
            Expr::Concurrent { branches, .. } => {
                let values = branches
                    .iter()
                    .map(|b| self.hint(b, vars, functions, types, default))
                    .collect::<Option<Vec<_>>>()?;
                if values.iter().all(|ty| *ty == Type::Unit) {
                    Some(Type::Unit)
                } else {
                    self.hint_work
                        .observe(crate::core::types::tuple_type(values, types), expr.span())
                }
            }
            Expr::Branch { .. }
            | Expr::Let { .. }
            | Expr::Var { .. }
            | Expr::Destructure { .. }
            | Expr::Assign { .. }
            | Expr::FieldAssign { .. }
            | Expr::If {
                else_branch: None, ..
            } => Some(Type::Unit),
            Expr::IndexedLoop(_) => Some(Type::Unit),
            Expr::While {
                condition, body, ..
            } => {
                if control_flow::is_true_literal(condition) && !control_flow::has_loop_break(body) {
                    None
                } else {
                    Some(Type::Unit)
                }
            }
            Expr::Array { elements, .. } => {
                // Container choice remains contextual even when an element has
                // a concrete type. Only the default pass selects dynamic Array.
                if !default {
                    return None;
                }
                let element = self.array_element_hint(elements, vars, functions, types, default)?;
                self.hint_work.observe(
                    self.dynamic_array_type(element, types, expr.span()),
                    expr.span(),
                )
            }
            Expr::Tuple { elements, .. } => {
                let elements = elements
                    .iter()
                    .map(|e| self.hint(e, vars, functions, types, default))
                    .collect::<Option<Vec<_>>>()?;
                self.hint_work
                    .observe(crate::core::types::tuple_type(elements, types), expr.span())
            }
            Expr::If {
                then_branch,
                else_branch: Some(other),
                ..
            } => sibling_hint(
                [then_branch, other]
                    .into_iter()
                    .map(|e| self.hint(e, vars, functions, types, default)),
                default,
            ),
            Expr::Match { value, arms, .. } => {
                // The typed walk needs the matched value's enum; without one,
                // the default pass still scans arm bodies for a concrete leaf.
                if let Some((matched_type, borrowed)) = self
                    .hint(value, vars, functions, types, false)
                    .and_then(|ty| {
                        self.hint_work
                            .observe(self.match_type(&ty, types, value.span()), value.span())
                    })
                {
                    let typed = arms.iter().find_map(|arm| {
                        let mut local = vars.clone();
                        self.hint_work
                            .observe(
                                self.check_match_pattern(&arm.pattern, &matched_type, types),
                                arm.pattern.span(),
                            )?
                            .hints(&mut local, borrowed);
                        self.hint(&arm.body, &local, functions, types, default)
                    });
                    if typed.is_some() || !default {
                        return typed;
                    }
                } else if !default {
                    return None;
                }
                arms.iter()
                    .find_map(|arm| self.hint(&arm.body, vars, functions, types, true))
            }
            Expr::Graph { exprs, .. } => self.graph_hint(exprs, vars, functions, types, default),
            Expr::Unsafe { exprs, .. }
            | Expr::Block { exprs, .. }
            | Expr::Sequence { exprs, .. } => {
                let mut local = vars.clone();
                for expr in exprs.iter().take(exprs.len().saturating_sub(1)) {
                    self.hint_bindings(expr, &mut local, functions, types);
                }
                exprs.last().map_or(Some(Type::Unit), |e| {
                    // A semicolon discards a value, but cannot make a panic
                    // produce Unit and constrain another branch's type.
                    self.hint(e, &local, functions, types, default).map(|ty| {
                        if matches!(expr, Expr::Sequence { trailing: true, .. }) {
                            Type::Unit
                        } else {
                            ty
                        }
                    })
                })
            }
            _ => None,
        }
    }
}

impl FunctionPlan {
    pub(super) fn aggregate_default_hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        self.bound_hint(expr, vars, functions, types)
            .filter(Type::is_aggregate)
    }

    // Only retry after contextual inference and literal defaults have both
    // finished. Another argument can determine an otherwise ambiguous value.
    pub(super) fn diagnose_argument_hints<'a>(
        &self,
        args: impl Iterator<Item = (&'a Expr, &'a Type)>,
        parameters: &[Arc<crate::core::types::TypeParameter>],
        substitutions: &crate::core::types::Substitutions,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Result<(), Error> {
        let mut local = vars.clone();
        for (arg, formal) in args {
            if generics::unresolved(formal, parameters, substitutions) {
                self.hint_work
                    .checked(|| self.bound_hint(arg, &local, functions, types))?;
            }
            self.hint_bindings(arg, &mut local, functions, types);
        }
        Ok(())
    }

    pub(super) fn bound_hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        self.hint(expr, vars, functions, types, true)
    }

    pub(super) fn hint_bindings(
        &self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) {
        match expr {
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
                let ty = annotation
                    .clone()
                    .or_else(|| self.bound_hint(value, vars, functions, types));
                self.hint_bindings(value, vars, functions, types);
                vars.remove(name);
                if let Some(ty) = ty {
                    vars.insert(name.clone(), hint_binding(ty));
                }
            }
            Expr::Destructure {
                pattern,
                annotation,
                value,
                ..
            } => {
                let ty = annotation
                    .clone()
                    .or_else(|| self.bound_hint(value, vars, functions, types));
                self.hint_bindings(value, vars, functions, types);
                bind_pattern(pattern, ty.as_ref(), vars);
            }
            Expr::Sequence { exprs, .. }
            | Expr::Array {
                elements: exprs, ..
            }
            | Expr::Tuple {
                elements: exprs, ..
            }
            | Expr::Call { args: exprs, .. } => {
                for expr in exprs {
                    self.hint_bindings(expr, vars, functions, types);
                }
            }
            Expr::BinaryChain { .. } => {
                for operand in expr.children() {
                    self.hint_bindings(operand, vars, functions, types);
                }
            }
            Expr::Index {
                base: lhs,
                index: rhs,
                ..
            }
            | Expr::BinOp { lhs, rhs, .. } => {
                self.hint_bindings(lhs, vars, functions, types);
                self.hint_bindings(rhs, vars, functions, types);
            }
            Expr::Record { fields, .. } => {
                for field in fields {
                    self.hint_bindings(&field.value, vars, functions, types);
                }
            }
            Expr::MethodCall(call) => {
                self.hint_bindings(&call.receiver, vars, functions, types);
                for arg in &call.args {
                    self.hint_bindings(arg, vars, functions, types);
                }
            }
            Expr::Field { base, .. } => self.hint_bindings(base, vars, functions, types),
            Expr::FieldAssign { value, .. }
            | Expr::Assign { value, .. }
            | Expr::Spread { value, .. }
            | Expr::Unary { operand: value, .. } => {
                self.hint_bindings(value, vars, functions, types)
            }
            _ => {}
        }
    }
}

fn hint_binding(ty: Type) -> Binding {
    payload_binding(&ty, 0)
}

fn bind_pattern(
    pattern: &crate::parser::ast::BindingPattern,
    ty: Option<&Type>,
    vars: &mut HashMap<String, Binding>,
) {
    use crate::parser::ast::BindingPattern;
    match pattern {
        BindingPattern::Sequence(p) => {
            let element = ty.and_then(super::arrays::element_type);
            for item in p.prefix.iter().chain(&p.suffix) {
                bind_pattern(item, element, vars);
            }
            if let Some(Some(name)) = &p.rest {
                vars.remove(name);
            }
        }
        BindingPattern::Name { name, .. } => {
            vars.remove(name);
            if let Some(ty) = ty {
                vars.insert(name.clone(), hint_binding(ty.clone()));
            }
        }
        BindingPattern::Tuple { elements, .. } => {
            if let Some(Type::Reference(reference)) = ty {
                if let Type::BufferEntry(pair) = &reference.target {
                    if let Type::Tuple(pair) = pair.as_ref() {
                        for (column, element) in elements.iter().enumerate() {
                            let ty = pair.elements.get(column).map(|t| {
                                Type::reference(t.clone(), column == 1 && reference.mutable)
                            });
                            bind_pattern(element, ty.as_ref(), vars);
                        }
                        return;
                    }
                }
            }
            for (index, element) in elements.iter().enumerate() {
                bind_pattern(
                    element,
                    match ty {
                        Some(Type::Tuple(tuple)) => tuple.elements.get(index),
                        _ => None,
                    },
                    vars,
                );
            }
        }
        BindingPattern::Record { inner, .. } => {
            for field in &inner.fields {
                bind_pattern(
                    &field.pattern,
                    ty.and_then(|ty| match ty {
                        Type::Record(record) => record
                            .fields
                            .iter()
                            .find(|f| f.name == field.name)
                            .map(|f| &f.ty),
                        _ => None,
                    }),
                    vars,
                );
            }
        }
        BindingPattern::Discard { .. } => {}
    }
}

pub(super) fn float_literal(
    text: &str,
    expected: Option<&Type>,
    span: Span,
) -> Result<(Type, Constant), Error> {
    let invalid = || {
        type_error(
            &span,
            ErrorCode::TypeMismatch,
            "Floating literal is outside the target finite range",
        )
    };
    if expected == Some(&Type::Float32) {
        let value: f32 = text.parse().map_err(|_| invalid())?;
        if !value.is_finite() {
            return Err(invalid());
        }
        Ok((Type::Float32, Constant::Float32(value)))
    } else {
        let value: f64 = text.parse().map_err(|_| invalid())?;
        if !value.is_finite() {
            return Err(invalid());
        }
        Ok((Type::Float64, Constant::Float64(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};

    #[test]
    fn nested_generic_hint_work_scales_with_chain_length() {
        let steps = |depth| {
            let calls = format!("{}1{}", "id(".repeat(depth), ")".repeat(depth));
            let methods = ".keep()".repeat(depth);
            let source = format!("fn id<T>(x:T)->T{{x}} struct Box<T>{{ value:T }} impl<T> Box<T>{{ fn keep(self)->Self{{self}} }} fn calls()->Int32{{{calls}}} fn methods(v:Box<Int32>)->Box<Int32>{{v{methods}}}");
            let ast = crate::parser::parse(&source).unwrap();
            let types = TypeRegistry::new().with_prelude(false);
            let plan = PreparedModule::new(&ast, &types, &BindingRegistry::default())
                .unwrap()
                .plan(&HashMap::new())
                .unwrap();
            ["calls", "methods"].map(|name| plan.bodies[name].hint_work.steps.get())
        };
        let short = steps(10);
        let long = steps(30);
        for (small, large) in short.into_iter().zip(long) {
            assert!(
                large <= 4 * small,
                "10 levels took {small} steps, 30 took {large}"
            );
        }
    }
    #[test]
    fn generic_hint_cache_checks_syntax_and_shadowed_bindings_at_the_same_span() {
        let mut module = crate::parser::parse(
            "
            fn id<T>(x: T) -> T { x }
            fn triple<A,B,C>(a:A, b:B, c:C) -> (A,B,C) { (a,b,c) }
            fn probe(x: Int32) -> Int32 {
                let (n, b, c) = triple(id(x), { let x = true; id(x) }, id(true))
                if b && c { n } else { 0 }
            }
        ",
        )
        .unwrap();
        let Item::FnDef(function) = &mut module.items[2] else {
            unreachable!()
        };
        let Expr::Block { exprs, .. } = &mut function.body else {
            unreachable!()
        };
        let Expr::Destructure { value, .. } = &mut exprs[0] else {
            unreachable!()
        };
        let Expr::Call { args, .. } = value.as_mut() else {
            unreachable!()
        };
        let first = args[0].clone();
        args[2] = args[2].clone().with_span(first.span());
        let Expr::Block { exprs, .. } = &mut args[1] else {
            unreachable!()
        };
        // Identical AST, but the second x is Bool instead of Int32.
        let Expr::Sequence { exprs, .. } = &mut exprs[0] else {
            unreachable!()
        };
        *exprs.last_mut().unwrap() = first;
        let mut vm = crate::vm::VmEngine::new_interpreter();
        vm.set_prelude_enabled(false);
        vm.load_module(&module).unwrap();
        assert_eq!(vm.call_int32_1("probe", 42).unwrap(), 42);
    }

    #[test]
    fn generic_hints_keep_concrete_types_and_report_the_exhausted_budget() {
        let mut vm = crate::vm::VmEngine::new_interpreter();
        vm.set_prelude_enabled(false);
        vm.load_source("fn current() -> Int32 { 42 }").unwrap();
        let prefix = "fn id<T>(x:T)->T{x}";
        for body in [
            "fn bad(n:Int32)->Int64 { id(id(n)) }",
            "fn bad()->Int32 { id(id(3000000000)) }",
        ] {
            let err = vm.load_source(&format!("{prefix}{body}")).unwrap_err();
            assert_eq!(err.code(), ErrorCode::TypeMismatch, "{err}");
        }
        // Repeated independent statements spend one function's shared budget.
        let expression = format!("{}1{};\n", "id(".repeat(12), ")".repeat(12));
        let source = format!("{prefix}fn heavy() {{ {} }}", expression.repeat(2000));
        let err = vm.load_source(&source).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ResourceLimit, "{err}");
        assert!(err.message().contains("Type inference"));
        assert!(err
            .span()
            .unwrap()
            .slice(&source)
            .is_some_and(|s| !s.is_empty()));
        assert_eq!(vm.call_int32_0("current").unwrap(), 42);
    }
}
