//! Synchronous task-arena intrinsics and automatic field dereferencing.
use super::*;

impl FunctionPlan {
    pub(super) fn pointee(&self, target: &Type, types: &TypeRegistry) -> Result<Type, Error> {
        if target.is_closure_value() {
            return Ok(target.clone());
        }
        super::super::types::reference_target(target, &self.nominals, types)
    }

    pub(super) fn reference_value(
        &mut self,
        value: Option<Slot>,
        moving: bool,
    ) -> Result<Option<Slot>, Error> {
        let Some(src) = value else { return Ok(None) };
        if src.ty.is_owner() {
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
        if let (Flow::Value(Some(src)), Some(Type::Function(expected))) = (&flow, expected) {
            if let Some(closure) = src.ty.closure_info() {
                if expected.suspends
                    || expected.signature != closure.signature
                    || closure.pointer_entry.is_none()
                {
                    return Err(type_error(
                        &self.current_span,
                        ErrorCode::TypeMismatch,
                        "Only signature-matched noncapturing closures convert to function pointers",
                    ));
                }
                let name = closure.pointer_entry.clone().unwrap();
                let dst = self.slot(Type::Function(expected.clone()))?;
                self.emit(Op::Closure(Box::new(closures::ClosureOp::Consume(
                    src.clone(),
                ))));
                self.emit(Op::FunctionRef(dst.clone(), name));
                return Ok(Flow::Value(Some(dst)));
            }
        }
        if let (Flow::Value(Some(src)), Some(Type::Reference(target))) = (&flow, expected) {
            if let Some(interface) = expected.and_then(Type::dyn_interface) {
                let actual = src.ty.reference_type().ok_or_else(|| {
                    type_error(
                        &self.current_span,
                        ErrorCode::TypeMismatch,
                        "Dyn erasure requires an explicit borrow of a stable value",
                    )
                })?;
                if target.mutable && !actual.mutable {
                    return Err(type_error(
                        &self.current_span,
                        ErrorCode::TypeMismatch,
                        "A shared borrow cannot become an exclusive dyn view",
                    ));
                }
                if actual.target != target.target {
                    self.type_registry.check_dyn_erasure(
                        &actual.target,
                        interface,
                        self.current_span,
                    )?;
                    let dst = self.slot(Type::Reference(target.clone()))?;
                    self.dyn_erasures.push(DynErasure {
                        source: src.clone(),
                        target: dst.clone(),
                        span: self.current_span,
                    });
                    self.emit(Op::Reference {
                        dst: dst.clone(),
                        src: src.clone(),
                        moving: false,
                    });
                    let table = self.dyn_entries(&actual.target, interface, self.current_span)?;
                    self.emit(Op::DynTable {
                        dst: Slot {
                            index: dst.index + 1,
                            ty: Type::Ptr,
                        },
                        table,
                    });
                    return Ok(Flow::Value(Some(dst)));
                }
            }
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
                "alloc" | "share" | "memory.read" | "memory.write" | "memory.replace"
            ) {
                let count = if matches!(callee.as_str(), "memory.write" | "memory.replace") {
                    2
                } else {
                    1
                };
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
                    if !target.is_copy_storage_value() || target.reference_type().is_some() {
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
                if target.contains_dyn() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Dyn storage cannot be read, written or replaced directly",
                    ));
                }
                if target.contains_closures() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Closure storage can only be accessed by checked callable operations",
                    ));
                }
                if target.is_buffer_target() && (callee != "memory.replace" || !target.is_owner()) {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        if callee == "memory.replace" {
                            "memory.replace requires a value target, not an Element or Slice view"
                        } else {
                            "Buffer control blocks are not accessible through memory.read/write"
                        },
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
                        format!("{callee} value has the wrong type"),
                    ));
                }
                if callee == "memory.replace" {
                    if target.is_owner() {
                        // Preparation may fail; both values retain their cleanup until
                        // the infallible swap commits. No source Drop runs here.
                        let value = self.snapshot(&value)?.unwrap();
                        if target.is_inline_owner() {
                            let place = self.slot(Type::reference(target.clone(), true))?;
                            self.emit(Op::Local {
                                dst: place.clone(),
                                value: Some(value.clone()),
                                target: target.clone(),
                            });
                            self.register_owner(value.clone(), Some(place.clone()));
                            self.emit(Op::Buffer(BufferOp::Swap {
                                reference,
                                value: value.clone(),
                                place: Some(place.clone()),
                            }));
                            let result = self.read_owner(place, &value, vec![], target)?;
                            return Ok(Some(Flow::Value(result)));
                        }
                        self.emit(Op::Buffer(BufferOp::Swap {
                            reference,
                            value: value.clone(),
                            place: None,
                        }));
                        return Ok(Some(Flow::Value(Some(value))));
                    }
                    let dst = if target == Type::Unit {
                        None
                    } else {
                        Some(self.slot(target.clone())?)
                    };
                    self.emit(Op::ReadMemory {
                        dst: dst.clone(),
                        reference: reference.clone(),
                        path: vec![],
                    });
                    self.emit(Op::WriteMemory {
                        reference,
                        path: vec![],
                        value,
                        target,
                    });
                    return Ok(Some(Flow::Value(dst)));
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
            self.check_field(record, name, span)?;
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
        if target.is_owner() {
            return Err(type_error(&expr.span(), ErrorCode::InvalidAssignment,
                "Cannot move or replace an owning field through a borrowed wrapper; borrow the field instead"));
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
    let addressed: std::collections::HashSet<_> = bodies
        .values()
        .flat_map(|b| &b.ops)
        .flat_map(|op| match op {
            Op::FunctionRef(slot, name) if matches!(slot.ty, Type::Function(_)) => {
                vec![name.clone()]
            }
            Op::DynTable { table, .. } => table.methods.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut reference_capability = std::collections::HashSet::new();
    for (name, body) in bodies {
        let info = functions.get_mut(name).unwrap();
        let references = info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(|ty| ty.contains_references() || ty.contains_buffers() && *ty != Type::String);
        // Reference-bearing helpers are never C/VM entries. Allocation and
        // reclamation need the hidden arena argument. borrowing::check also imposes
        // storage preconditions on writable reference-element buffer signatures.
        if references || body.has_references() || body.has_buffers() {
            reference_capability.insert(name.clone());
        }
        info.uses_arena = body.ops.iter().any(|op| match op {
            Op::Alloc { .. } | Op::Text { .. } => true,
            Op::Buffer(BufferOp::Storage { intrinsic, .. }) => intrinsic.allocates(),
            _ => false,
        });
        info.needs_arena = body.is_destructor || body.has_buffers() || addressed.contains(name) || info.uses_arena || body.ops.iter().any(|op| {
            matches!(op, Op::Buffer(BufferOp::Storage { intrinsic, .. }) if intrinsic.needs_arena())
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
                    Op::TaskBranch { entry, .. } => reference_capability.contains(entry),
                    _ => false,
                })
            {
                reference_capability.insert(name.clone());
                changed = true;
            }
            if !functions[name].needs_arena
                && body.ops.iter().any(|op| match op {
                    Op::Call { name, .. } => functions[name].needs_arena,
                    Op::Scope { branches, .. } => branches
                        .iter()
                        .any(|branch| functions[&branch.name].needs_arena),
                    Op::TaskBranch { entry, .. } => functions[entry].needs_arena,
                    _ => false,
                })
            {
                functions.get_mut(name).unwrap().needs_arena = true;
                changed = true;
            }
            if !functions[name].uses_arena
                && body.ops.iter().any(|op| match op {
                    Op::Call { name, .. } => functions[name].uses_arena,
                    Op::Cleanup(CleanupOp::Register {
                        hooks, elements, ..
                    }) => hooks
                        .iter()
                        .chain(elements.iter().flat_map(|e| &e.hooks))
                        .any(|hook| functions[&hook.name].uses_arena),
                    Op::Scope { branches, .. } => branches
                        .iter()
                        .any(|branch| functions[&branch.name].uses_arena),
                    Op::TaskBranch { entry, .. } => functions[entry].uses_arena,
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
    borrowing::infer_module(functions, bodies)?;
    for (name, body) in bodies {
        let info = functions.get_mut(name).unwrap();
        info.instance_entry = !info.internal;
        // References carry nominal identities; validate the full resolved root.
        if let Some(Type::Reference(reference)) =
            info.signature.params.first().cloned().filter(|ty| {
                !info.internal
                    && !info.suspends
                    && info.parameter_noescape(0)
                    && ty
                        .reference_type()
                        .is_some_and(|r| matches!(r.target, Type::Record(_)))
            })
        {
            let target = body.pointee(&reference.target, &body.type_registry)?;
            info.signature.params[0] = Type::reference(target, reference.mutable);
        }
        info.instance_entry = crate::core::instance::eligible(info);
        let references = info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(|ty| ty.contains_references() || ty.contains_buffers() && *ty != Type::String);
        info.internal |= (info.signature.has_text()
            && info
                .signature
                .params
                .iter()
                .chain(std::iter::once(&info.signature.ret))
                .any(Type::is_aggregate))
            || references
            || info
                .signature
                .params
                .iter()
                .chain(std::iter::once(&info.signature.ret))
                .any(Type::contains_functions);
    }
    for (name, body) in bodies {
        let info = &functions[name];
        if info.signature.ret.contains_closures() {
            return Err(type_error(
                &info.span,
                ErrorCode::TypeMismatch,
                "Closure values and references cannot be returned",
            ));
        }
        if info.suspends && info.signature.params.iter().any(Type::contains_closures) {
            return Err(type_error(
                &info.span,
                ErrorCode::InvalidCall,
                "Closure values cannot enter suspending frames",
            ));
        }
        for (index, ty) in info.signature.params.iter().enumerate() {
            if ty.contains_dyn() && !info.parameter_noescape(index) {
                return Err(type_error(
                    &info.span,
                    ErrorCode::InvalidCall,
                    "Dyn parameters must be proved not to escape",
                ));
            }
        }
        buffers::check_function(body, info)?;
        let mut local_bytes = 0usize;
        for (op, span) in body.ops.iter().zip(&body.op_spans) {
            let storage = match op {
                Op::Local {
                    target,
                    value: None,
                    ..
                } if target.is_closure_value() => None,
                Op::Local { target, .. } => Some(target),
                Op::Cleanup(CleanupOp::Register {
                    value,
                    place: None,
                    hooks,
                    ..
                }) if !hooks.is_empty() => Some(&value.ty),
                _ => None,
            };
            if let Some(target) = storage {
                if info.suspends && !target.is_copy_value() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "Local borrowing is only supported in synchronous functions",
                    ));
                }
                if target
                    .scalar_types()
                    .iter()
                    .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)))
                {
                    continue;
                }
                let layout = target
                    .storage_layout()
                    .map_err(|e| type_error(span, ErrorCode::ResourceLimit, e.to_string()))?;
                local_bytes = local_bytes
                    .checked_add(layout.size.max(1))
                    .and_then(|n| {
                        n.checked_add(
                            if info.suspends {
                                layout.align.max(8)
                            } else {
                                layout.align
                            } - 1,
                        )
                    })
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
                    && !r.target.is_closure_value()
                {
                    return Err(type_error(&info.span, ErrorCode::TypeMismatch,
                        "Exclusive references to objects containing references are construction-local; publish a shared reference before crossing a function boundary"));
                }
            }
        }
        for (callee, index, span) in &body.implicit_borrows {
            if !functions[callee].parameter_noescape(*index) {
                return Err(type_error(span, ErrorCode::InvalidCall,
                    "Implicit borrowing requires a parameter proved not to return or retain the borrow; pass an explicit reference if its lifetime permits"));
            }
        }
        closures::check(body, info)?;
        borrowing::check(body, info, functions, &body.type_registry)?;
        for (op, span) in body.ops.iter().zip(&body.op_spans) {
            if let Op::Alloc { target, .. } = op {
                // Rigid templates have no concrete layout until instantiation.
                if !target
                    .scalar_types()
                    .iter()
                    .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)))
                {
                    target
                        .memory_layout()
                        .map_err(|e| type_error(span, ErrorCode::ResourceLimit, e.to_string()))?;
                }
            }
            if let Op::FunctionRef(slot, name) = op {
                if !matches!(slot.ty, Type::Function(_))
                    && (functions[name].uses_arena || reference_capability.contains(name))
                {
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
