//! Synchronous task-arena intrinsics and automatic field dereferencing.
use super::*;

impl FunctionPlan {
    pub(super) fn pointee(&self, target: &Type, types: &TypeRegistry) -> Result<Type, Error> {
        super::super::types::reference_target(target, &self.nominals, types)
    }

    pub(super) fn reference_value(
        &mut self,
        value: Option<Slot>,
        moving: bool,
    ) -> Result<Option<Slot>, Error> {
        let Some(src) = value else { return Ok(None) };
        if src.ty.is_buffer() {
            return self.snapshot(&Some(src));
        }
        if src.ty.reference_type().is_none() {
            return Ok(Some(src));
        }
        let dst = self.slot(src.ty.clone())?;
        self.emit(Op::Reference {
            dst: dst.clone(),
            src,
            moving,
        });
        Ok(Some(dst))
    }

    pub(super) fn reserve_write(&mut self, src: Slot) -> Result<Slot, Error> {
        let dst = self.slot(src.ty.clone())?;
        self.emit(Op::Reserve {
            dst: dst.clone(),
            src,
        });
        Ok(dst)
    }

    pub(super) fn coerce_reference(
        &mut self,
        flow: Flow,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        if let (Flow::Value(Some(src)), Some(Type::Reference(target))) = (&flow, expected) {
            if src
                .ty
                .reference_type()
                .is_some_and(|r| r.mutable && !target.mutable && r.target == target.target)
            {
                let dst = self.slot(Type::Reference(target.clone()))?;
                self.emit(Op::Reference {
                    dst: dst.clone(),
                    src: src.clone(),
                    moving: false,
                });
                return Ok(Flow::Value(Some(dst)));
            }
        }
        Ok(flow)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn memory_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Option<Flow>, Error> {
        if let Expr::Call {
            callee,
            type_args,
            args,
            span,
        } = expr
        {
            if callee.starts_with("memory.")
                && (vars.contains_key("memory") || self.scope.modules.contains_key("memory"))
            {
                return Ok(None);
            }
            if matches!(
                callee.as_str(),
                "alloc" | "share" | "memory.read" | "memory.write"
            ) {
                let count = if callee == "memory.write" { 2 } else { 1 };
                if !type_args.is_empty() || args.len() != count {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!("{callee} expects {count} arguments and no type arguments"),
                    ));
                }
                let hint = if callee == "alloc" {
                    expected.and_then(Type::reference_type).map(|r| &r.target)
                } else {
                    None
                };
                let Flow::Value(value) =
                    self.expr_expected(&args[0], vars, functions, suspends, types, hint)?
                else {
                    if let Some(next) = args.get(1) {
                        return Err(unreachable_error(next));
                    }
                    return Ok(Some(Flow::Diverged));
                };
                if callee == "alloc" {
                    let target = slot_type(&value);
                    if !target.is_copy_scalar() && !target.is_aggregate() {
                        return Err(type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            "alloc requires Copy data, not a reference, String or opaque handle",
                        ));
                    }
                    let dst = self.slot(Type::reference(target.clone(), true))?;
                    self.emit(Op::Alloc {
                        dst: dst.clone(),
                        value,
                        target,
                    });
                    return Ok(Some(Flow::Value(Some(dst))));
                }
                let mut reference = value
                    .filter(|v| v.ty.reference_type().is_some())
                    .ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            format!("{callee} requires a reference"),
                        )
                    })?;
                let r = reference.ty.reference_type().unwrap();
                let target = self.pointee(&r.target, types)?;
                reference.ty = Type::reference(target, r.mutable);
                let r = reference.ty.reference_type().unwrap();
                if callee == "share" {
                    let dst = self.slot(Type::reference(r.target.clone(), false))?;
                    self.emit(Op::Reference {
                        dst: dst.clone(),
                        src: reference,
                        moving: false,
                    });
                    return Ok(Some(Flow::Value(Some(dst))));
                }
                let target = r.target.clone();
                if target.is_buffer() {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "Buffer control blocks are not accessible through memory.read/write",
                    ));
                }
                if callee == "memory.read" {
                    let dst = if target == Type::Unit {
                        None
                    } else {
                        Some(self.slot(target)?)
                    };
                    self.emit(Op::ReadMemory {
                        dst: dst.clone(),
                        reference,
                        path: vec![],
                    });
                    return Ok(Some(Flow::Value(dst)));
                }
                if !r.mutable {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        "Cannot write through a shared reference",
                    ));
                }
                // Reserve the target while evaluating the value; shared reads remain legal.
                let reference = self.reserve_write(reference)?;
                let Flow::Value(value) =
                    self.expr_expected(&args[1], vars, functions, suspends, types, Some(&target))?
                else {
                    return Ok(Some(Flow::Diverged));
                };
                if slot_type(&value) != target {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "memory.write value has the wrong type",
                    ));
                }
                self.emit(Op::WriteMemory {
                    reference,
                    path: vec![],
                    value,
                    target,
                });
                return Ok(Some(Flow::Value(None)));
            }
        }
        if let Some(flow) = self.local_field_expr(expr, vars, functions, suspends, types)? {
            return Ok(Some(flow));
        }
        // Stop at the nearest reference; recursively evaluate earlier pointer hops.
        let field = match expr {
            Expr::Field { .. } => expr,
            Expr::FieldAssign { target, .. } => target,
            _ => return Ok(None),
        };
        let mut root = field;
        let mut path = Vec::new();
        while let Expr::Field {
            base,
            name,
            type_args,
            span,
        } = root
        {
            if !type_args.is_empty() {
                return Ok(None);
            }
            path.push((name.clone(), *span));
            root = base;
            if self
                .type_hint(root, vars, functions, types)
                .is_some_and(|ty| ty.reference_type().is_some())
            {
                break;
            }
        }
        let Some(Type::Reference(r)) = self.type_hint(root, vars, functions, types) else {
            return Ok(None);
        };
        path.reverse();
        let pointee = self.pointee(&r.target, types)?;
        let mut target = pointee.clone();
        for (name, span) in &path {
            let Type::Record(record) = &target else {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Type {target} has no fields"),
                ));
            };
            target = record
                .fields
                .iter()
                .find(|f| &f.name == name)
                .map(|f| f.ty.clone())
                .ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::UnknownName,
                        format!("Unknown field {name}"),
                    )
                })?;
        }
        let Flow::Value(reference) = self.expr(root, vars, functions, suspends, types)? else {
            return Ok(Some(Flow::Diverged));
        };
        let mut reference = reference.unwrap();
        reference.ty = Type::reference(pointee, r.mutable);
        let path = path.into_iter().map(|(name, _)| name).collect();
        if let Expr::FieldAssign { value, span, .. } = expr {
            if !r.mutable {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Cannot write through a shared reference",
                ));
            }
            // Reserve the address before RHS effects; shared reads are allowed
            // until the write activates exclusive access.
            let reference = self.reserve_write(reference)?;
            let Flow::Value(value) =
                self.expr_expected(value, vars, functions, suspends, types, Some(&target))?
            else {
                return Ok(Some(Flow::Diverged));
            };
            if slot_type(&value) != target {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Field assignment has the wrong type",
                ));
            }
            self.emit(Op::WriteMemory {
                reference,
                path,
                value,
                target,
            });
            Ok(Some(Flow::Value(None)))
        } else {
            let dst = if target == Type::Unit {
                None
            } else {
                Some(self.slot(target)?)
            };
            self.emit(Op::ReadMemory {
                dst: dst.clone(),
                reference,
                path,
            });
            Ok(Some(Flow::Value(dst)))
        }
    }
}

pub(super) fn check_module(
    functions: &mut HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
) -> Result<(), Error> {
    let mut reference_capability = std::collections::HashSet::new();
    for (name, body) in bodies {
        let info = functions.get_mut(name).unwrap();
        let references = info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(|ty| ty.contains_references() || ty.contains_buffers());
        // Reference-bearing helpers are never C/VM entries. Only allocation-capable
        // call chains need the hidden arena argument. borrowing::check also imposes
        // storage preconditions on writable reference-element buffer signatures.
        info.internal |= references;
        if references || body.has_references() || body.has_buffers() {
            reference_capability.insert(name.clone());
        }
        info.uses_arena = body.ops.iter().any(|op| {
            matches!(
                op,
                Op::Alloc { .. }
                    | Op::Buffer(BufferOp::Storage {
                        intrinsic: crate::parser::ast::BufferIntrinsic::Allocate,
                        ..
                    })
            )
        });
    }
    loop {
        let mut changed = false;
        for (name, body) in bodies {
            if !reference_capability.contains(name)
                && body.ops.iter().any(|op| match op {
                    Op::Call { name, .. } => reference_capability.contains(name),
                    Op::Scope { branches, .. } => branches
                        .iter()
                        .any(|branch| reference_capability.contains(&branch.name)),
                    _ => false,
                })
            {
                reference_capability.insert(name.clone());
                changed = true;
            }
            if !functions[name].uses_arena
                && body.ops.iter().any(|op| match op {
                    Op::Call { name, .. } => functions[name].uses_arena,
                    Op::Scope { branches, .. } => branches
                        .iter()
                        .any(|branch| functions[&branch.name].uses_arena),
                    _ => false,
                })
            {
                functions.get_mut(name).unwrap().uses_arena = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (name, body) in bodies {
        let info = &functions[name];
        buffers::check_function(body, info)?;
        let mut local_bytes = 0usize;
        for (op, span) in body.ops.iter().zip(&body.op_spans) {
            if let Op::Local { target, .. } = op {
                if info.suspends {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Local borrowing is only supported in synchronous functions",
                    ));
                }
                if target
                    .scalar_types()
                    .iter()
                    .any(|ty| matches!(ty, Type::Parameter(_)))
                {
                    continue;
                }
                let layout = target
                    .memory_layout()
                    .map_err(|e| type_error(span, ErrorCode::ResourceLimit, e.to_string()))?;
                local_bytes = local_bytes
                    .checked_add(layout.size.max(1))
                    .and_then(|n| n.checked_add(layout.align - 1))
                    .filter(|&n| n <= 64 * 1024)
                    .ok_or_else(|| {
                        type_error(
                            span,
                            ErrorCode::ResourceLimit,
                            "Borrowed local storage exceeds the 64 KiB function limit",
                        )
                    })?;
            }
        }
        for ty in info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
        {
            if let Some(r) = ty.reference_type() {
                if r.mutable
                    && body
                        .pointee(&r.target, &body.type_registry)?
                        .contains_references()
                {
                    return Err(type_error(&info.span, ErrorCode::TypeMismatch,
                        "Exclusive references to objects containing references are construction-local; publish a shared reference before crossing a function boundary"));
                }
            }
        }
        borrowing::check(body, info, functions, &body.type_registry)?;
        for (op, span) in body.ops.iter().zip(&body.op_spans) {
            if let Op::Alloc { target, .. } = op {
                // Rigid templates have no concrete layout until instantiation.
                if !target
                    .scalar_types()
                    .iter()
                    .any(|ty| matches!(ty, Type::Parameter(_)))
                {
                    target
                        .memory_layout()
                        .map_err(|e| type_error(span, ErrorCode::ResourceLimit, e.to_string()))?;
                }
            }
            if let Op::FunctionRef(_, name) = op {
                if functions[name].uses_arena || reference_capability.contains(name) {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Arena functions cannot be registered as native callbacks",
                    ));
                }
            }
        }
    }
    Ok(())
}
