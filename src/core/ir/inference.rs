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
        match expr {
            Expr::Literal { literal, .. } => match literal {
                Literal::Integer { .. } => None,
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
                .or_else(|| functions.get(callee).map(|f| f.signature.ret.clone()))
                .or_else(|| {
                    callee
                        .split_once('.')
                        .and_then(|(name, _)| self.nominals.get(name).cloned())
                }),
            Expr::Record { name, .. } => self.nominals.get(name).cloned(),
            Expr::Field { base, name, .. } => {
                match self.type_hint(base, vars, functions, types).or_else(|| {
                    if let Expr::Ident { name, .. } = base.as_ref() {
                        self.nominals.get(name).cloned()
                    } else {
                        None
                    }
                })? {
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
            } => self.type_hint(operand, vars, functions, types),
            Expr::Unary {
                op: UnaryOp::Not, ..
            }
            | Expr::Logical { .. } => Some(Type::Bool),
            Expr::BinOp { op, lhs, rhs, .. } => {
                if op.is_comparison() {
                    Some(Type::Bool)
                } else {
                    self.binary_hint(lhs, rhs, vars, functions, types)
                }
            }
            Expr::Tuple { elements, .. } => {
                let elements = elements
                    .iter()
                    .map(|e| self.type_hint(e, vars, functions, types))
                    .collect::<Option<Vec<_>>>()?;
                crate::core::types::tuple_type(elements, types).ok()
            }
            Expr::If {
                then_branch,
                else_branch: Some(other),
                ..
            } => self
                .type_hint(then_branch, vars, functions, types)
                .or_else(|| self.type_hint(other, vars, functions, types)),
            Expr::Match { value, arms, .. } => {
                let Type::Enum(enumeration) = self.type_hint(value, vars, functions, types)? else {
                    return None;
                };
                arms.iter().find_map(|arm| {
                    let mut local = vars.clone();
                    if let crate::parser::ast::Pattern::Variant {
                        name,
                        binding: Some(binding),
                        ..
                    } = &arm.pattern
                    {
                        let variant_name = name.rsplit('.').next()?;
                        if let Some(ty) = enumeration
                            .variants
                            .iter()
                            .find(|v| v.name == variant_name)
                            .and_then(|v| v.payload.as_ref())
                        {
                            local.insert(binding.clone(), hint_binding(ty.clone()));
                        }
                    }
                    self.type_hint(&arm.body, &local, functions, types)
                })
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
                exprs
                    .last()
                    .and_then(|e| self.type_hint(e, &local, functions, types))
            }
            _ => None,
        }
    }
}

impl FunctionPlan {
    fn bound_hint(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
    ) -> Option<Type> {
        if let Some(ty) = self.type_hint(expr, vars, functions, types) {
            return Some(ty);
        }
        match expr {
            Expr::Literal {
                literal: Literal::Integer { .. },
                ..
            } => Some(Type::Int32),
            Expr::Unary {
                op: UnaryOp::Neg | UnaryOp::BitNot,
                operand,
                ..
            } => self.bound_hint(operand, vars, functions, types),
            Expr::BinOp { lhs, rhs, op, .. } if !op.is_comparison() => self
                .bound_hint(lhs, vars, functions, types)
                .or_else(|| self.bound_hint(rhs, vars, functions, types)),
            Expr::Tuple { elements, .. } => crate::core::types::tuple_type(
                elements
                    .iter()
                    .map(|e| self.bound_hint(e, vars, functions, types))
                    .collect::<Option<Vec<_>>>()?,
                types,
            )
            .ok(),
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
                    self.bound_hint(e, &local, functions, types)
                })
            }
            _ => None,
        }
    }

    fn hint_bindings(
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
    Binding {
        slot: (ty != Type::Unit).then_some(Slot { index: 0, ty }),
        mutable: false,
    }
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
