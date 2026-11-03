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
        .map(|(index, constant)| (constant.key.clone(), index))
        .collect();
    let mut edges = vec![Vec::new(); members.constants.len()];
    for (index, constant) in members.constants.iter().enumerate() {
        validate_expression(&constant.declaration.value)?;
        let context = constant.declaration.scope.as_ref();
        let imports = context.map_or_else(
            || generics.borrow().scope.clone(),
            |scope| scope.imports.clone(),
        );
        let mut pending = vec![&constant.declaration.value];
        while let Some(expr) = pending.pop() {
            match expr {
                // A bare name binds only through the declaring scope's
                // constant spellings: a same-module constant, a selector
                // import or a prelude name. Local bindings cannot occur in a
                // constant expression, so nothing else can shadow the name.
                Expr::Ident { name, span } => {
                    let key = imports.constants.get(name).cloned().ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::UnknownName,
                            format!("Unknown module constant `{name}`"),
                        )
                    })?;
                    let dependency = names.get(&key).copied().ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::UnknownName,
                            format!("Unknown module constant `{name}`"),
                        )
                    })?;
                    edges[index].push(dependency);
                }
                Expr::Field {
                    base, name, span, ..
                } => {
                    let root = expression_path(base).expect("validated constant path");
                    // A module constant resolves by declaration identity before
                    // any nominal typing: `cfg.LIMIT` names a constant, not a
                    // member. Every legal spelling — the defining module's own
                    // names included — is already visibility-filtered by
                    // `ImportScope::constants`, which never binds another
                    // module's private constants.
                    let spelling = format!("{root}.{name}");
                    if let Some(key) = imports.constants.get(&spelling).cloned() {
                        let dependency = names.get(&key).copied().ok_or_else(|| {
                            type_error(
                                span,
                                ErrorCode::UnknownName,
                                format!("Unknown module constant `{spelling}`"),
                            )
                        })?;
                        edges[index].push(dependency);
                        continue;
                    }
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
                    let scoped = members.scoped_key(key.clone(), context.map(AsRef::as_ref));
                    let dependency =
                        names
                            .get(&scoped)
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
                Expr::BinaryChain { .. } => pending.extend(expr.children()),
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
    let name = format!("{}.<const:{}>", constant.key, declaration.name);
    let definition = FnDef {
        every: None,
        kind: crate::parser::ast::FunctionKind::Ordinary,
        noescape: None,
        public: false,
        buffer_intrinsic: None,
        compiler_intrinsic: None,
        buffer_element: None,
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
            every: None,
            closure_entry: false,
            noescape: false,
            noescape_params: vec![],
            infer_noescape: false,
            inferred_noescape: Vec::new(),
            inferred_retained: None,
            inferred_arena_retained: Vec::new(),
            returns_interior: true,
            compiler_intrinsic: None,
            builtin: None,
            extern_abi: crate::core::builtins::ExternAbi::Scalar,
            signature: crate::FunctionSignature::new(vec![], declaration.ty.clone()),
            suspends: false,
            uses_concurrency: false,
            may_fail: false,
            uses_arena: false,
            needs_arena: false,
            frame_slots: 0,
            internal: true,
            instance_entry: false,
            domain_entry: false,
            domain_request_entry: false,
            script_entry: false,
            parallel_entry: false,
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
        Constant::Float32(n) => Literal::Float32(n),
        Constant::Float64(n) => Literal::Float64(n),
        Constant::Int8(n) => Literal::Int8(n),
        Constant::Int16(n) => Literal::Int16(n),
        Constant::Int32(n) => Literal::Int32(n),
        Constant::Int64(n) => Literal::Int64(n),
        Constant::UInt8(n) => Literal::UInt8(n),
        Constant::UInt16(n) => Literal::UInt16(n),
        Constant::UInt32(n) => Literal::UInt32(n),
        Constant::Rune(n) => Literal::Rune(n),
        Constant::UInt64(n) => Literal::UInt64(n),
        Constant::Bool(n) => Literal::Bool(n),
        Constant::String(_) | Constant::NullPtr => unreachable!(),
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
            Expr::Literal { literal: Literal::ConstParameter(_), .. } => return Err(type_error(&expr.span(),ErrorCode::TypeMismatch,"Associated constants cannot depend on const parameters; return the value from a method")),
            Expr::Literal { literal, .. } if !matches!(literal, Literal::String(_)) => {},
            // A bare name is a module constant reference, and a dotted path is
            // either a module constant or an associated constant. Both are
            // checked for existence and visibility by the dependency scan.
            Expr::Ident { .. } | Expr::GenericName(_) => {},
            Expr::Field { base, type_args, .. } if type_args.is_empty() && expression_path(base).is_some() => {},
            Expr::Unary { operand, .. } => pending.push(operand),
            Expr::BinOp { lhs, rhs, .. } => { pending.push(lhs); pending.push(rhs); },
            Expr::BinaryChain { rest, .. } => { count += rest.len().saturating_sub(1); pending.extend(expr.children()); },
            Expr::Call { callee, args, .. } if (IntegerType::conversion(callee).is_some() || matches!(callee.as_str(), "float32" | "float64")) => pending.extend(args),
            _ => return Err(type_error(&expr.span(), ErrorCode::InvalidState, "Constants allow scalar literals, module and associated constants, integer conversions and unary/binary operators")),
        }
    }
    Ok(())
}

fn convert(value: &Constant, ty: &Type) -> Constant {
    if ty.is_float() {
        macro_rules! cast {
            ($t:ty, $variant:ident) => {
                Constant::$variant(match value {
                    Constant::Int8(n) => *n as $t,
                    Constant::Int16(n) => *n as $t,
                    Constant::Int32(n) => *n as $t,
                    Constant::Int64(n) => *n as $t,
                    Constant::UInt8(n) => *n as $t,
                    Constant::UInt16(n) => *n as $t,
                    Constant::UInt32(n) => *n as $t,
                    Constant::UInt64(n) => *n as $t,
                    Constant::Float32(n) => *n as $t,
                    Constant::Float64(n) => *n as $t,
                    _ => unreachable!(),
                })
            };
        }
        return if *ty == Type::Float32 {
            cast!(f32, Float32)
        } else {
            cast!(f64, Float64)
        };
    }
    let bits = match value {
        Constant::Int8(n) => *n as u64,
        Constant::Int16(n) => *n as u64,
        Constant::Int32(n) => *n as u64,
        Constant::Int64(n) => *n as u64,
        Constant::UInt8(n) => u64::from(*n),
        Constant::UInt16(n) => u64::from(*n),
        Constant::UInt32(n) => u64::from(*n),
        Constant::Rune(n) => u64::from(*n),
        Constant::UInt64(n) => *n,
        _ => unreachable!(),
    };
    match ty {
        Type::Int8 => Constant::Int8(bits as u8 as i8),
        Type::Int16 => Constant::Int16(bits as u16 as i16),
        Type::Int32 => Constant::Int32(bits as i32),
        Type::Int64 => Constant::Int64(bits as i64),
        Type::UInt8 => Constant::UInt8(bits as u8),
        Type::UInt16 => Constant::UInt16(bits as u16),
        Type::UInt32 => Constant::UInt32(bits as u32),
        Type::Rune => Constant::Rune(bits as u32),
        Type::UInt64 => Constant::UInt64(bits),
        _ => unreachable!(),
    }
}

fn unary(op: &UnaryOp, value: &Constant) -> Constant {
    match (op, value) {
        (UnaryOp::Neg, Constant::Float32(n)) => Constant::Float32(-n),
        (UnaryOp::Neg, Constant::Float64(n)) => Constant::Float64(-n),
        (UnaryOp::Neg, Constant::Int8(n)) => Constant::Int8(n.wrapping_neg()),
        (UnaryOp::Neg, Constant::Int16(n)) => Constant::Int16(n.wrapping_neg()),
        (UnaryOp::Neg, Constant::Int32(n)) => Constant::Int32(n.wrapping_neg()),
        (UnaryOp::Neg, Constant::Int64(n)) => Constant::Int64(n.wrapping_neg()),
        (UnaryOp::Not, Constant::Bool(n)) => Constant::Bool(!n),
        (UnaryOp::BitNot, Constant::Int8(n)) => Constant::Int8(!n),
        (UnaryOp::BitNot, Constant::Int16(n)) => Constant::Int16(!n),
        (UnaryOp::BitNot, Constant::Int32(n)) => Constant::Int32(!n),
        (UnaryOp::BitNot, Constant::Int64(n)) => Constant::Int64(!n),
        (UnaryOp::BitNot, Constant::UInt8(n)) => Constant::UInt8(!n),
        (UnaryOp::BitNot, Constant::UInt16(n)) => Constant::UInt16(!n),
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
    macro_rules! float {
        ($apply:ident, $a:expr, $b:expr, $variant:ident) => {
            match crate::core::float::$apply(op, *$a, *$b) {
                crate::core::float::FloatOutcome::Value(n) => Constant::$variant(n),
                crate::core::float::FloatOutcome::Bool(n) => Constant::Bool(n),
            }
        };
    }
    Ok(match (lhs, rhs) {
        (Constant::Float32(a), Constant::Float32(b)) => float!(apply_f32, a, b, Float32),
        (Constant::Float64(a), Constant::Float64(b)) => float!(apply_f64, a, b, Float64),
        (Constant::Int8(a), Constant::Int8(b)) => integer!(apply_i8, a, b, Int8),
        (Constant::Int16(a), Constant::Int16(b)) => integer!(apply_i16, a, b, Int16),
        (Constant::Int32(a), Constant::Int32(b)) => integer!(apply_i32, a, b, Int32),
        (Constant::Int64(a), Constant::Int64(b)) => integer!(apply_i64, a, b, Int64),
        (Constant::UInt8(a), Constant::UInt8(b)) => integer!(apply_u8, a, b, UInt8),
        (Constant::UInt16(a), Constant::UInt16(b)) => integer!(apply_u16, a, b, UInt16),
        (Constant::UInt32(a), Constant::UInt32(b)) => integer!(apply_u32, a, b, UInt32),
        (Constant::UInt64(a), Constant::UInt64(b)) => integer!(apply_u64, a, b, UInt64),
        (Constant::Rune(a), Constant::Rune(b)) => {
            Constant::Bool(if *op == BinOp::Eq { a == b } else { a != b })
        }
        (Constant::Bool(a), Constant::Bool(b)) => {
            Constant::Bool(if *op == BinOp::Eq { a == b } else { a != b })
        }
        _ => unreachable!(),
    })
}
