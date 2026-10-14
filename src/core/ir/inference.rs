//! Contextual integer literals. Hints inspect types without evaluating expressions.
use super::*;

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
        // Search the whole subtree for a concrete type before defaulting any
        // leaf. This also applies inside tuples and local bindings: a default
        // in an earlier branch must not hide a later branch's known type.
        if default {
            if let Some(ty) = self.hint(expr, vars, functions, types, false) {
                return Some(ty);
            }
        }
        if let Some(constant) = self.associated_constant(expr, vars, types).ok().flatten() {
            return Some(constant.declaration.ty.clone());
        }
        if let Some(ty) = self.dyn_hint(expr, vars, functions, types) {
            return Some(ty);
        }
        if let Some(call) = self
            .member_call(expr, vars, functions, types)
            .ok()
            .flatten()
        {
            return self.hint(&call, vars, functions, types, default);
        }
        if let Ok(Some((_, info))) = self.function_value_info(expr, vars, functions, types, false) {
            return Some(Type::function_with_effect(
                info.signature.params,
                info.signature.ret,
                info.suspends,
            ));
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
        if let Some(constructor) = self.enum_constructor(expr, vars).ok().flatten() {
            return self
                .constructor_type(
                    &constructor,
                    vars,
                    functions,
                    types,
                    generics::ConstructorContext {
                        expected: None,
                        default,
                    },
                )
                .ok()
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
                Literal::UInt64(_) => Some(Type::UInt64),
                Literal::Bool(_) => Some(Type::Bool),
                Literal::String(_) => Some(Type::String),
            },
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
                Some(Type::function(params, ret))
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
                        self.generic_signature(expr, vars, functions, types)
                            .ok()
                            .map(|(_, signature)| signature.ret)
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
            Expr::Record { .. } => self
                .record_type(
                    expr,
                    vars,
                    functions,
                    types,
                    generics::ConstructorContext {
                        expected: None,
                        default,
                    },
                )
                .ok(),
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
                        .pointee(&r.target, types)
                        .ok()?
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
                op: UnaryOp::Deref,
                operand,
                ..
            } => {
                let ty = self.hint(operand, vars, functions, types, default)?;
                self.pointee(&ty.reference_type()?.target, types).ok()
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
            Expr::BinOp { op, lhs, rhs, .. } => {
                if op.is_comparison() {
                    return Some(Type::Bool);
                }
                let from_operands = || {
                    if !default {
                        return None;
                    }
                    self.hint(lhs, vars, functions, types, true)
                        .or_else(|| self.hint(rhs, vars, functions, types, true))
                };
                self.binary_hint(lhs, rhs, vars, functions, types)
                    .or_else(from_operands)
            }
            Expr::Scope {
                kind: ScopeKind::Race,
                branches,
                ..
            } => branches
                .first()
                .and_then(|branch| self.hint(branch, vars, functions, types, default)),
            Expr::Let { .. }
            | Expr::Var { .. }
            | Expr::Destructure { .. }
            | Expr::Assign { .. }
            | Expr::FieldAssign { .. }
            | Expr::While { .. }
            | Expr::Scope { .. }
            | Expr::If {
                else_branch: None, ..
            } => Some(Type::Unit),
            Expr::Tuple { elements, .. } => {
                let elements = elements
                    .iter()
                    .map(|e| self.hint(e, vars, functions, types, default))
                    .collect::<Option<Vec<_>>>()?;
                crate::core::types::tuple_type(elements, types).ok()
            }
            Expr::If {
                then_branch,
                else_branch: Some(other),
                ..
            } => self
                .hint(then_branch, vars, functions, types, default)
                .or_else(|| self.hint(other, vars, functions, types, default)),
            Expr::Match { value, arms, .. } => {
                // The typed walk needs the matched value's enum; without one,
                // the default pass still scans arm bodies for a concrete leaf.
                if let Some((matched_type, borrowed)) = self
                    .hint(value, vars, functions, types, false)
                    .and_then(|ty| self.match_type(&ty, types, value.span()).ok())
                {
                    let typed = arms.iter().find_map(|arm| {
                        let mut local = vars.clone();
                        self.check_match_pattern(&arm.pattern, &matched_type, types)
                            .ok()?
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
            | Expr::Tuple {
                elements: exprs, ..
            }
            | Expr::Call { args: exprs, .. } => {
                for expr in exprs {
                    self.hint_bindings(expr, vars, functions, types);
                }
            }
            Expr::BinOp { lhs, rhs, .. } => {
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
        BindingPattern::Name { name, .. } => {
            vars.remove(name);
            if let Some(ty) = ty {
                vars.insert(name.clone(), hint_binding(ty.clone()));
            }
        }
        BindingPattern::Tuple { elements, .. } => {
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
