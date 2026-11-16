//! Bounded constant evaluation after ordinary IR type checking.
use super::*;
use crate::core::integer::IntegerOutcome;
use std::rc::Rc;

const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_STRING_BYTES: usize = 16 * MAX_STRING_BYTES;
const MAX_EXPRESSION_STRING_BYTES: usize = MAX_TOTAL_STRING_BYTES;

const MAX_VALUE_NODES: usize = 4096;
const MAX_TOTAL_VALUE_NODES: usize = 65536;

/// Evaluated data with its resolved nominal/layout identity, never a constructor
/// expression to recheck in the consumer's namespace or field visibility scope.
pub(super) struct Value {
    ty: Type,
    // Includes enum tags and inactive payload lanes; inactive String lanes are NullPtr.
    lanes: Vec<Constant>,
}

impl Value {
    fn string_bytes(&self) -> usize {
        self.lanes
            .iter()
            .map(|value| match value {
                Constant::String(text) => text.len(),
                _ => 0,
            })
            .sum()
    }

    pub(super) fn materialize(&self, plan: &mut FunctionPlan) -> Result<Flow, Error> {
        if self.ty == Type::Unit {
            return Ok(Flow::Value(None));
        }
        let slot = plan.slot(self.ty.clone())?;
        for (dst, value) in slot.scalars().zip(&self.lanes) {
            if let Constant::String(text) = value {
                // Follow ordinary record construction: each text allocation
                // has a registered temporary before it joins the aggregate.
                // Failure partway through construction must release earlier
                // fields even before the whole aggregate is registered.
                let text_slot = if self.ty == Type::String {
                    dst.clone()
                } else {
                    plan.slot(Type::String)?
                };
                plan.emit(Op::Text {
                    dst: text_slot.clone(),
                    source: TextSource::Literal(text.clone()),
                });
                plan.register_owner(text_slot.clone(), None);
                if self.ty != Type::String {
                    plan.emit(Op::Copy(dst, text_slot));
                }
            } else {
                plan.emit(Op::Literal(dst, value.clone()));
            }
        }
        if self.ty != Type::String && self.ty.is_owner() {
            plan.register_owner(slot.clone(), None);
        }
        Ok(Flow::Value(Some(slot)))
    }
}

/// Count expanded inline components, including zero-sized fields. A shared type
/// DAG can describe exponentially many components without using scalar lanes.
fn value_nodes(ty: &Type, span: Span) -> Result<usize, Error> {
    if ty.layout_depth() > 32 {
        return Err(type_error(
            &span,
            ErrorCode::ResourceLimit,
            "Constant aggregate nesting exceeds 32 levels",
        ));
    }
    let mut pending = vec![ty];
    let mut nodes = 0;
    while let Some(ty) = pending.pop() {
        nodes += 1;
        if nodes > MAX_VALUE_NODES {
            return Err(type_error(
                &span,
                ErrorCode::ResourceLimit,
                "Constant aggregate values are limited to 4096 components",
            ));
        }
        match ty {
            Type::Record(record) => pending.extend(record.fields.iter().map(|field| &field.ty)),
            Type::Tuple(tuple) => pending.extend(&tuple.elements),
            Type::Enum(enumeration) => pending.extend(
                enumeration
                    .variants
                    .iter()
                    .filter_map(|variant| variant.payload.as_ref()),
            ),
            _ => {}
        }
    }
    Ok(nodes)
}

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
    let mut literal_bytes = Vec::with_capacity(members.constants.len());
    for (index, constant) in members.constants.iter().enumerate() {
        let context = constant.declaration.scope.as_ref();
        let imports = context.map_or_else(
            || generics.borrow().scope.clone(),
            |scope| scope.imports.clone(),
        );
        let source_nominals = context.map_or(nominals.as_ref(), |scope| scope.nominals.as_ref());
        literal_bytes.push(validate_expression(
            &constant.declaration.value,
            constant.declaration.ty == Type::String,
            |expr| {
                enums::resolve_constructor(
                    expr,
                    &imports,
                    source_nominals,
                    members,
                    context.map(AsRef::as_ref),
                    |_| false,
                )
            },
        )?);
        let mut pending = vec![&constant.declaration.value];
        while let Some(expr) = pending.pop() {
            if let Some(constructor) = enums::resolve_constructor(
                expr,
                &imports,
                source_nominals,
                members,
                context.map(AsRef::as_ref),
                |_| false,
            )? {
                // Constructor names are not dependencies; only their payloads
                // can refer to other constants, including forward references.
                pending.extend(constructor.args.unwrap_or(&[]));
                continue;
            }
            match expr {
                Expr::Ident { name, span } => {
                    let dependency = imports
                        .constants
                        .get(name)
                        .and_then(|key| names.get(key))
                        .copied()
                        .ok_or_else(|| {
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
                    if let Some(root) = expression_path(base) {
                        let spelling = format!("{root}.{name}");
                        if let Some(dependency) = imports
                            .constants
                            .get(&spelling)
                            .and_then(|key| names.get(key))
                        {
                            edges[index].push(*dependency);
                            continue;
                        }
                        let nominal = context
                            .map_or(nominals.as_ref(), |scope| scope.nominals.as_ref())
                            .get(&root);
                        // A nominal namespace denotes an associated constant;
                        // otherwise the suffix is a field projection and only
                        // its base contributes a dependency.
                        if nominal.is_some() || crate::core::types::primitive_type(&root).is_some()
                        {
                            let spelling_visible = (!root.contains('.')
                                || imports.types.contains_key(&root))
                                && !imports
                                    .types
                                    .get(&root)
                                    .is_some_and(|binding| binding.resolved.is_some());
                            let owner = nominal.map_or_else(|| root.clone(), members::owner_name);
                            let key = format!("{owner}.{name}");
                            let visible = spelling_visible
                                && context.map_or_else(
                                    || !members.private.contains(&key),
                                    |scope| scope.members.contains(&key),
                                );
                            let scoped =
                                members.scoped_key(key.clone(), context.map(AsRef::as_ref));
                            if let Some(dependency) =
                                names.get(&scoped).copied().filter(|_| visible)
                            {
                                edges[index].push(dependency);
                            } else if imports.constants.contains_key(&root) {
                                // Types and values have separate namespaces. If
                                // no associated constant matches, a same-named
                                // module value can still supply a record field.
                                pending.push(base);
                            } else {
                                return Err(type_error(
                                    span,
                                    ErrorCode::UnknownName,
                                    format!("Unknown associated constant `{key}`"),
                                ));
                            }
                            continue;
                        }
                    }
                    pending.push(base);
                }
                // Constructor names and field labels are not value references.
                _ => pending.extend(expr.children()),
            }
        }
    }
    let mut state = vec![0; members.constants.len()];
    let mut depths = vec![0usize; members.constants.len()];
    let mut string_bytes = 0;
    let mut aggregate_nodes = 0;
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
                if matches!(
                    constant.declaration.ty,
                    Type::Tuple(_) | Type::Record(_) | Type::Enum(_)
                ) {
                    let nodes = value_nodes(&constant.declaration.ty, constant.declaration.span)?;
                    if nodes > MAX_TOTAL_VALUE_NODES - aggregate_nodes {
                        return Err(type_error(
                            &constant.declaration.span,
                            ErrorCode::ResourceLimit,
                            "Evaluated aggregate constants exceed 65536 components in total",
                        ));
                    }
                    aggregate_nodes += nodes;
                }
                // Bound literal expansion before planning: a small expression
                // can reference a large template many times, even if a later
                // projection discards its text. Dependency edges keep repeats.
                let mut input_bytes = literal_bytes[index];
                for &dependency in &edges[index] {
                    input_bytes += members.constants[dependency]
                        .value
                        .borrow()
                        .as_ref()
                        .expect("evaluated dependency")
                        .string_bytes();
                    if input_bytes > MAX_EXPRESSION_STRING_BYTES {
                        break;
                    }
                }
                if input_bytes > MAX_EXPRESSION_STRING_BYTES {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::ResourceLimit,
                        "Constant expressions are limited to 16 MiB of expanded string data",
                    ));
                }
                let value = evaluate(constant, functions, types, nominals, generics.clone())?;
                // Every string leaf counts, including repeated fields and aliases.
                let bytes = value.string_bytes();
                if bytes > MAX_STRING_BYTES {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::ResourceLimit,
                        "Constant values are limited to 1 MiB of UTF-8 string data",
                    ));
                }
                if bytes > MAX_TOTAL_STRING_BYTES - string_bytes {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::ResourceLimit,
                        "Evaluated string constants exceed 16 MiB in total",
                    ));
                }
                string_bytes += bytes;
                *constant.value.borrow_mut() = Some(value);
                state[index] = 2;
            } else if state[index] != 2 {
                if state[index] == 1 {
                    return Err(type_error(
                        &constant.declaration.span,
                        ErrorCode::InvalidState,
                        format!(
                            "Cyclic {} constant dependency involving `{}`",
                            if constant.owner.target().is_some() {
                                "associated"
                            } else {
                                "module"
                            },
                            constant.declaration.name
                        ),
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
) -> Result<Value, Error> {
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
        name,
        type_params: vec![],
        params: vec![],
        ret_ty: declaration.ty.clone(),
        suspends: false,
        body: declaration.value.clone(),
        span: declaration.span,
    };
    // Constant bodies have no parameters or callable identity. The planner
    // derives their slots from the declaration without registering a synthetic entry.
    let local = types.with_parameters(&constant.parameters)?;
    let plan = FunctionPlan::new(&definition, functions, &local, nominals, generics, 0)?;
    let mut slots: Vec<Option<Constant>> = vec![None; plan.slot_count];
    for (op, span) in plan.ops.iter_spanned() {
        match op {
            Op::Cleanup(CleanupOp::Register { .. }) => continue,
            Op::Buffer(
                BufferOp::Move { dst, src }
                | BufferOp::Extract { dst, src, .. }
                | BufferOp::Detach { dst, src, .. },
            ) => {
                if dst.index != src.index {
                    for (dst, src) in dst.scalars().zip(src.scalars()) {
                        slots[dst.index] = slots[src.index].take();
                    }
                }
                continue;
            }
            Op::Buffer(BufferOp::End(owner)) => {
                // A scalar projection can still refer to the source slot after
                // its owner ends. Only the owning lanes lose their payloads.
                for slot in owner.scalars().filter(|slot| slot.ty.is_owner_lane()) {
                    slots[slot.index] = None;
                }
                continue;
            }
            _ => {}
        }
        let (dst, value) = match op {
            Op::Literal(dst, value) => (dst, value.clone()),
            Op::Text {
                dst,
                source: TextSource::Literal(text),
            } => (dst, Constant::String(text.clone())),
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
    let lanes = plan
        .result
        .iter()
        .flat_map(Slot::scalars)
        .map(|slot| slots[slot.index].take().expect("evaluated constant lane"))
        .collect();
    Ok(Value {
        ty: declaration.ty.clone(),
        lanes,
    })
}

fn validate_expression<'a>(
    expr: &'a Expr,
    string: bool,
    constructor: impl Fn(&'a Expr) -> Result<Option<enums::Constructor<'a>>, Error>,
) -> Result<usize, Error> {
    let mut pending = vec![(expr, string)];
    let mut count = 0;
    let mut string_bytes = 0;
    while let Some((expr, string)) = pending.pop() {
        count += 1;
        if count > 256 {
            return Err(type_error(
                &expr.span(),
                ErrorCode::ResourceLimit,
                "Constant expressions are limited to 256 syntax nodes",
            ));
        }
        if !string {
            if let Some(constructor) = constructor(expr)? {
                pending.extend(
                    constructor
                        .args
                        .unwrap_or(&[])
                        .iter()
                        .map(|expr| (expr, false)),
                );
                continue;
            }
        }
        match expr {
            Expr::Literal { literal: Literal::ConstParameter(_), .. } => return Err(type_error(&expr.span(),ErrorCode::TypeMismatch,"Associated constants cannot depend on const parameters; return the value from a method")),
            Expr::Literal { literal: Literal::String(text), span } => {
                if text.len() > MAX_STRING_BYTES {
                    return Err(type_error(span, ErrorCode::ResourceLimit, "String constants are limited to 1 MiB of UTF-8 bytes each"));
                }
                string_bytes += text.len();
            },
            Expr::Literal { .. } => {},
            // A bare name is a module constant reference, and a dotted path is
            // either a module constant or an associated constant. Both are
            // checked for existence and visibility by the dependency scan.
            Expr::Ident { .. } | Expr::GenericName(_) => {},
            Expr::Field { base, type_args, .. } if type_args.is_empty() => pending.push((base, false)),
            Expr::Tuple { elements, .. } if !string => pending.extend(elements.iter().map(|expr| (expr, false))),
            Expr::Record { .. } if !string => pending.extend(expr.children().into_iter().map(|expr| (expr, false))),
            Expr::Block { exprs, .. } if !string && exprs.is_empty() => {},
            Expr::Unary { operand, .. } if !string => pending.push((operand, false)),
            Expr::BinOp { lhs, rhs, .. } if !string => { pending.push((lhs, false)); pending.push((rhs, false)); },
            Expr::BinaryChain { rest, .. } if !string => { count += rest.len().saturating_sub(1); pending.extend(expr.children().into_iter().map(|expr| (expr, false))); },
            Expr::Call { callee, args, .. } if !string && (IntegerType::conversion(callee).is_some() || matches!(callee.as_str(), "float32" | "float64")) => pending.extend(args.iter().map(|expr| (expr, false))),
            _ if string => return Err(type_error(&expr.span(), ErrorCode::InvalidState, "String constants require a string literal, a string constant reference or an aggregate field read")),
            _ => return Err(type_error(&expr.span(), ErrorCode::InvalidState, "Constants allow scalar literals, string literals, tuple/record/enum construction and field reads, module and associated constants, integer conversions and unary/binary operators")),
        }
    }
    Ok(string_bytes)
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
