//! Contextual integer literals. Hints inspect types without evaluating expressions.
use super::*;

pub(super) fn integer_literal(
    magnitude: u64,
    negative: bool,
    expected: Option<&Type>,
    span: Span,
) -> Result<(Type, Constant), Error> {
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
        IntegerType::Int32 => Constant::Int32(bits as i32),
        IntegerType::Int64 => Constant::Int64(bits as i64),
        IntegerType::UInt32 => Constant::UInt32(bits as u32),
        IntegerType::UInt64 => Constant::UInt64(bits),
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
    fn hint(
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
        if let Some(call) = self
            .member_call(expr, vars, functions, types)
            .ok()
            .flatten()
        {
            return self.hint(&call, vars, functions, types, default);
        }
        if let Some(constructor) = self.enum_constructor(expr, vars) {
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
            if let Some(arg) = args.first().filter(|_| {
                !callee.starts_with("memory.")
                    || (!vars.contains_key("memory") && !self.scope.modules.contains_key("memory"))
            }) {
                match callee.as_str() {
                    "alloc" => {
                        return self
                            .hint(arg, vars, functions, types, default)
                            .map(|ty| Type::reference(ty, true))
                    }
                    "share" | "memory.read" => {
                        let ty = self.hint(arg, vars, functions, types, default)?;
                        let r = ty.reference_type()?;
                        return Some(if callee == "share" {
                            Type::reference(r.target.clone(), false)
                        } else {
                            r.target.clone()
                        });
                    }
                    "memory.write" => return Some(Type::Unit),
                    _ => {}
                }
            }
        }
        match expr {
            Expr::Literal { literal, .. } => match literal {
                Literal::Integer { .. } => default.then_some(Type::Int32),
                Literal::Int32(_) => Some(Type::Int32),
                Literal::Int64(_) => Some(Type::Int64),
                Literal::UInt32(_) => Some(Type::UInt32),
                Literal::UInt64(_) => Some(Type::UInt64),
                Literal::Bool(_) => Some(Type::Bool),
                Literal::String(_) => Some(Type::String),
            },
            Expr::Ident { name, .. } => vars.get(name).map(|b| slot_type(&b.slot)),
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
                            .and_then(|name| functions.get(&name).map(|f| f.signature.ret.clone()))
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
                    Type::Reference(r) => match &r.target {
                        Type::Record(record) => record
                            .fields
                            .iter()
                            .find(|f| f.name == *name)
                            .map(|f| f.ty.clone()),
                        _ => None,
                    },
                    Type::Record(record) => record
                        .fields
                        .iter()
                        .find(|field| field.name == *name)
                        .map(|field| field.ty.clone()),
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
            Expr::Let { .. }
            | Expr::Var { .. }
            | Expr::Destructure { .. }
            | Expr::Assign { .. }
            | Expr::FieldAssign { .. }
            | Expr::While { .. }
            | Expr::Scope { .. }
            | Expr::Sequence { trailing: true, .. }
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
                if let Some(Type::Enum(enumeration)) =
                    self.hint(value, vars, functions, types, false)
                {
                    let typed = arms.iter().find_map(|arm| {
                        let mut local = vars.clone();
                        if let crate::parser::ast::Pattern::Variant {
                            name,
                            binding: Some(binding),
                            ..
                        } = &arm.pattern
                        {
                            let variant_name = self.pattern_variant(&enumeration, name)?;
                            if let Some(ty) = enumeration
                                .variants
                                .iter()
                                .find(|v| v.name == variant_name)
                                .and_then(|v| v.payload.as_ref())
                            {
                                local.insert(binding.clone(), hint_binding(ty.clone()));
                            }
                        }
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
            Expr::Block { exprs, .. }
            | Expr::Sequence {
                exprs,
                trailing: false,
                ..
            } => {
                let mut local = vars.clone();
                for expr in exprs.iter().take(exprs.len().saturating_sub(1)) {
                    self.hint_bindings(expr, &mut local, functions, types);
                }
                exprs.last().map_or(Some(Type::Unit), |e| {
                    self.hint(e, &local, functions, types, default)
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
        BindingPattern::Discard { .. } => {}
    }
}
