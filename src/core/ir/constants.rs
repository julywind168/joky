//! Bounded scalar constant evaluation after ordinary IR type checking.
use super::*;
use crate::core::integer::IntegerOutcome;
use std::rc::Rc;

pub(super) fn validate_all(
    members: &Rc<members::Members>,
    functions: &HashMap<String, FunctionInfo>,
    types: &TypeRegistry,
    nominals: &Arc<HashMap<String, Type>>,
    generics: Rc<std::cell::RefCell<generics::GenericContext>>,
) -> Result<(), Error> {
    // Resolve dependency order iteratively; constants never recurse through planners.
    let names: HashMap<_, _> = members
        .constants
        .iter()
        .enumerate()
        .map(|(index, constant)| {
            (
                members::member_key(&constant.target, &constant.declaration.name),
                index,
            )
        })
        .collect();
    let mut edges = vec![Vec::new(); members.constants.len()];
    for (index, constant) in members.constants.iter().enumerate() {
        validate_expression(&constant.declaration.value)?;
        let mut pending = vec![&constant.declaration.value];
        while let Some(expr) = pending.pop() {
            match expr {
                Expr::Field {
                    base, name, span, ..
                } => {
                    let root = expression_path(base).expect("validated constant path");
                    let context = constant.declaration.scope.as_ref();
                    let imports = context.map_or_else(
                        || generics.borrow().scope.clone(),
                        |scope| scope.imports.clone(),
                    );
                    let spelling_visible = (!root.contains('.')
                        || imports.types.contains_key(&root))
                        && !imports
                            .types
                            .get(&root)
                            .is_some_and(|binding| binding.resolved.is_some());
                    let root = context
                        .map_or(nominals.as_ref(), |scope| scope.nominals.as_ref())
                        .get(&root)
                        .map_or(root.clone(), members::owner_name);
                    let key = format!("{root}.{name}");
                    let visible = spelling_visible
                        && context.map_or_else(
                            || !members.private.contains(&key),
                            |scope| scope.members.contains(&key),
                        );
                    let dependency =
                        names
                            .get(&key)
                            .copied()
                            .filter(|_| visible)
                            .ok_or_else(|| {
                                type_error(
                                    span,
                                    ErrorCode::UnknownName,
                                    format!("Unknown associated constant `{key}`"),
                                )
                            })?;
                    edges[index].push(dependency);
                }
                Expr::Unary { operand, .. } => pending.push(operand),
                Expr::BinOp { lhs, rhs, .. } => {
                    pending.push(lhs);
                    pending.push(rhs);
                }
                Expr::Call { args, .. } => pending.extend(args),
                _ => {}
            }
        }
    }
    let mut state = vec![0; members.constants.len()];
    let mut depths = vec![0usize; members.constants.len()];
    for root in 0..members.constants.len() {
        let mut pending = vec![(root, false)];
        while let Some((index, exit)) = pending.pop() {
            let constant = &members.constants[index];
            if exit {
                depths[index] = 1 + edges[index].iter().map(|&i| depths[i]).max().unwrap_or(0);
                if depths[index] > 32 {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::ResourceLimit,
                        "Constant dependency depth exceeds 32 levels",
                    ));
                }
                let value = evaluate(constant, functions, types, nominals, generics.clone())?;
                *constant.value.borrow_mut() = Some(value);
                state[index] = 2;
            } else if state[index] != 2 {
                if state[index] == 1 {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::InvalidState,
                        "Cyclic associated constant dependency",
                    ));
                }
                state[index] = 1;
                pending.push((index, true));
                pending.extend(edges[index].iter().rev().map(|&i| (i, false)));
            }
        }
    }
    Ok(())
}

fn evaluate(
    constant: &Rc<members::ConstMember>,
    functions: &HashMap<String, FunctionInfo>,
    types: &TypeRegistry,
    nominals: &Arc<HashMap<String, Type>>,
    generics: Rc<std::cell::RefCell<generics::GenericContext>>,
) -> Result<Expr, Error> {
    let declaration = &constant.declaration;
    let name = format!("{}.<const:{}>", constant.target, declaration.name);
    let definition = FnDef {
        public: false,
        scope: declaration.scope.clone(),
        name: name.clone(),
        type_params: vec![],
        params: vec![],
        ret_ty: declaration.ty.clone(),
        suspends: false,
        body: declaration.value.clone(),
        span: declaration.span,
    };
    let mut functions = functions.clone();
    functions.insert(
        name,
        FunctionInfo {
            signature: crate::FunctionSignature::new(vec![], declaration.ty.clone()),
            suspends: false,
            may_fail: false,
            uses_arena: false,
            frame_slots: 0,
            internal: true,
            span: declaration.span,
        },
    );
    let local = types.with_parameters(&constant.parameters)?;
    let plan = FunctionPlan::new(&definition, &functions, &local, nominals, generics, 0)?;
    let mut slots: Vec<Option<Constant>> = vec![None; plan.slot_count];
    for (op, span) in plan.ops.iter().zip(&plan.op_spans) {
        let (dst, value) = match op {
            Op::Literal(dst, value) => (dst, value.clone()),
            Op::Copy(dst, src) => (dst, slots[src.index].clone().unwrap()),
            Op::Convert(dst, src) => (dst, convert(slots[src.index].as_ref().unwrap(), &dst.ty)),
            Op::Unary(dst, op, src) => (dst, unary(op, slots[src.index].as_ref().unwrap())),
            Op::Binary(dst, op, lhs, rhs) => (
                dst,
                binary(
                    op,
                    slots[lhs.index].as_ref().unwrap(),
                    slots[rhs.index].as_ref().unwrap(),
                    *span,
                )?,
            ),
            _ => {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidState,
                    "Unsupported constant expression",
                ))
            }
        };
        slots[dst.index] = Some(value);
    }
    let result = slots[plan.result.as_ref().unwrap().index].take().unwrap();
    let literal = match result {
        Constant::Int32(n) => Literal::Int32(n),
        Constant::Int64(n) => Literal::Int64(n),
        Constant::UInt32(n) => Literal::UInt32(n),
        Constant::UInt64(n) => Literal::UInt64(n),
        Constant::Bool(n) => Literal::Bool(n),
        Constant::String(_) => unreachable!(),
    };
    Ok(Expr::Literal {
        literal,
        span: declaration.span,
    })
}

fn validate_expression(expr: &Expr) -> Result<(), Error> {
    let mut pending = vec![expr];
    let mut count = 0;
    while let Some(expr) = pending.pop() {
        count += 1;
        if count > 256 {
            return Err(type_error(
                &expr.span(),
                ErrorCode::ResourceLimit,
                "Constant expressions are limited to 256 syntax nodes",
            ));
        }
        match expr {
            Expr::Literal { literal, .. } if !matches!(literal, Literal::String(_)) => {},
            Expr::Field { base, type_args, .. } if type_args.is_empty() && expression_path(base).is_some() => {},
            Expr::Unary { operand, .. } => pending.push(operand),
            Expr::BinOp { lhs, rhs, .. } => { pending.push(lhs); pending.push(rhs); },
            Expr::Call { callee, args, .. } if IntegerType::conversion(callee).is_some() => pending.extend(args),
            _ => return Err(type_error(&expr.span(), ErrorCode::InvalidState, "Constants allow scalar literals, associated constants, integer conversions and unary/binary operators")),
        }
    }
    Ok(())
}

fn convert(value: &Constant, ty: &Type) -> Constant {
    let bits = match value {
        Constant::Int32(n) => *n as u64,
        Constant::Int64(n) => *n as u64,
        Constant::UInt32(n) => u64::from(*n),
        Constant::UInt64(n) => *n,
        _ => unreachable!(),
    };
    match ty {
        Type::Int32 => Constant::Int32(bits as i32),
        Type::Int64 => Constant::Int64(bits as i64),
        Type::UInt32 => Constant::UInt32(bits as u32),
        Type::UInt64 => Constant::UInt64(bits),
        _ => unreachable!(),
    }
}

fn unary(op: &UnaryOp, value: &Constant) -> Constant {
    match (op, value) {
        (UnaryOp::Neg, Constant::Int32(n)) => Constant::Int32(n.wrapping_neg()),
        (UnaryOp::Neg, Constant::Int64(n)) => Constant::Int64(n.wrapping_neg()),
        (UnaryOp::Not, Constant::Bool(n)) => Constant::Bool(!n),
        (UnaryOp::BitNot, Constant::Int32(n)) => Constant::Int32(!n),
        (UnaryOp::BitNot, Constant::Int64(n)) => Constant::Int64(!n),
        (UnaryOp::BitNot, Constant::UInt32(n)) => Constant::UInt32(!n),
        (UnaryOp::BitNot, Constant::UInt64(n)) => Constant::UInt64(!n),
        _ => unreachable!(),
    }
}

fn binary(op: &BinOp, lhs: &Constant, rhs: &Constant, span: Span) -> Result<Constant, Error> {
    // The wrapping, division and shift rules come from the shared kernel; a
    // trap here is a compile-time rejection of the constant expression.
    macro_rules! integer {
        ($apply:ident, $a:expr, $b:expr, $variant:ident) => {
            match crate::core::integer::$apply(op, *$a, *$b) {
                Ok(IntegerOutcome::Value(value)) => Constant::$variant(value),
                Ok(IntegerOutcome::Bool(value)) => Constant::Bool(value),
                Err(_) => {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidState,
                        "Invalid division or shift in constant expression",
                    ))
                }
            }
        };
    }
    Ok(match (lhs, rhs) {
        (Constant::Int32(a), Constant::Int32(b)) => integer!(apply_i32, a, b, Int32),
        (Constant::Int64(a), Constant::Int64(b)) => integer!(apply_i64, a, b, Int64),
        (Constant::UInt32(a), Constant::UInt32(b)) => integer!(apply_u32, a, b, UInt32),
        (Constant::UInt64(a), Constant::UInt64(b)) => integer!(apply_u64, a, b, UInt64),
        (Constant::Bool(a), Constant::Bool(b)) => {
            Constant::Bool(if *op == BinOp::Eq { a == b } else { a != b })
        }
        _ => unreachable!(),
    })
}
