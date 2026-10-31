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
        if let (Flow::Value(Some(src)), Some(target @ Type::DynTrait(_))) = (&flow, expected) {
            if &src.ty != target {
                let interface = target.dyn_interface().unwrap();
                if src.ty.contains_explicit_arena() {
                    return Err(type_error(
                        &self.current_span,
                        ErrorCode::TypeMismatch,
                        "Arena owners cannot be stored in owning dyn objects",
                    ));
                }
                if src.ty.contains_references() || !src.ty.is_buffer_element() {
                    return Err(type_error(
                        &self.current_span,
                        ErrorCode::TypeMismatch,
                        "Owning dyn erasure requires a reference-free storable value",
                    ));
                }
                self.type_registry
                    .check_dyn_erasure(&src.ty, interface, self.current_span)?;
                let table = self.dyn_entries(&src.ty, interface, self.current_span)?;
                use crate::parser::ast::BufferIntrinsic;
                let buffer = self.slot(Type::Buffer(Arc::new(src.ty.clone())))?;
                let capacity = self.slot(Type::UInt64)?;
                self.emit(Op::Literal(capacity.clone(), Constant::UInt64(1)));
                self.emit(Op::Buffer(BufferOp::Storage {
                    intrinsic: BufferIntrinsic::Allocate,
                    element: src.ty.clone(),
                    args: vec![capacity],
                    dst: Some(buffer.clone()),
                }));
                self.register_owner(buffer.clone(), None);
                let reference = self.slot(Type::reference(buffer.ty.clone(), true))?;
                self.emit(Op::Buffer(BufferOp::Borrow {
                    reservation: false,
                    dst: reference.clone(),
                    src: buffer.clone(),
                    place: None,
                }));
                self.emit(Op::Buffer(BufferOp::Storage {
                    intrinsic: BufferIntrinsic::InitializeNext,
                    element: src.ty.clone(),
                    args: vec![reference, src.clone()],
                    dst: None,
                }));
                let dispatch = self.slot(Type::Ptr)?;
                self.emit(Op::DynTable {
                    dst: dispatch.clone(),
                    table,
                });
                self.emit(Op::DynPack {
                    owner: buffer.clone(),
                    table: dispatch,
                    indirect: src.ty.is_owner() && !src.ty.is_inline_owner(),
                });
                let dst = self.slot(target.clone())?;
                self.emit(Op::Buffer(BufferOp::Move {
                    dst: dst.clone(),
                    src: buffer,
                }));
                return Ok(Flow::Value(Some(dst)));
            }
        }
        if let (Flow::Value(Some(src)), Some(Type::Function(expected))) = (&flow, expected) {
            if let Type::Function(actual) = &src.ty {
                if actual.signature == expected.signature
                    && actual.suspends == expected.suspends
                    && actual.noescape
                    && !expected.noescape
                {
                    let dst = self.slot(Type::Function(expected.clone()))?;
                    self.emit(Op::Copy(dst.clone(), src.clone()));
                    return Ok(Flow::Value(Some(dst)));
                }
            }
            if let Some(closure) = src.ty.closure_info() {
                if closure.pointer_entry.is_none() {
                    return Err(super::closures::capturing_function_pointer_error(
                        self.current_span,
                    ));
                }
                if expected.suspends || expected.signature != closure.signature {
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

    fn check_memory_target(target: &Type, span: Span) -> Result<(), Error> {
        if target.contains_dyn() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Dyn storage cannot be read, written or replaced directly",
            ));
        }
        if target.contains_closures() {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Closure storage can only be accessed by checked callable operations",
            ));
        }
        if target.is_buffer_target() && !target.is_owner() {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Memory operations require a value target, not an Element or Slice view",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn dereferenced_place(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        reservation: bool,
        mutable: bool,
    ) -> Result<Option<Flow>, Error> {
        let mut root = expr;
        let mut path = Vec::new();
        while let Expr::Field {
            base,
            name,
            type_args,
            ..
        } = root
        {
            if !type_args.is_empty() {
                return Ok(None);
            }
            path.push(name.clone());
            root = base;
        }
        // Automatic field dereferencing stops at a stored reference. Let the
        // ordinary field planner evaluate that pointer hop before projecting.
        if matches!(root, Expr::Index { .. }) {
            if let Some(mut target) = self.type_hint(root, vars, functions, types) {
                for name in path.iter().rev() {
                    if target.reference_type().is_some() {
                        return Ok(None);
                    }
                    target = self.field_member(&target, name, &expr.span())?.1;
                }
            }
        }
        let span = root.span();
        let flow = match root {
            Expr::Index { .. } => {
                self.index_reference(root, mutable, vars, functions, suspends, types)?
            }
            Expr::Unary {
                op: UnaryOp::Deref,
                operand,
                ..
            } => self.expr(operand, vars, functions, suspends, types)?,
            _ => return Ok(None),
        };
        let Flow::Value(value) = flow else {
            return Ok(Some(Flow::Diverged));
        };
        let mut reference = value
            .filter(|v| v.ty.reference_type().is_some())
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Dereferencing requires a checked reference",
                )
            })?;
        let r = reference.ty.reference_type().unwrap();
        let mutable = r.mutable;
        let target = self.pointee(&r.target, types)?;
        reference.ty = Type::reference(target.clone(), mutable);
        if path.is_empty() {
            return Ok(Some(Flow::Value(Some(reference))));
        }
        path.reverse();
        let target = self.field_target(target, &path, expr.span())?;
        let dst = self.slot(Type::reference(target, mutable))?;
        self.emit(Op::Project {
            dst: dst.clone(),
            src: reference,
            path,
            reservation,
        });
        Ok(Some(Flow::Value(Some(dst))))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn mem_call(
        &mut self,
        intrinsic: &CompilerIntrinsic,
        args: &[Expr],
        params: &[Type],
        span: Span,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        if args.len() != 2 {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Memory operation expects two arguments",
            ));
        }
        let Flow::Value(reference) =
            self.expr_expected(&args[0], vars, functions, suspends, types, Some(&params[0]))?
        else {
            return Err(unreachable_error(&args[1]));
        };
        let mut reference = reference
            .filter(|v| v.ty.reference_type().is_some())
            .ok_or_else(|| {
                type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Memory operation requires a mutable reference",
                )
            })?;
        if reference.ty != params[0] {
            return Err(type_error(
                &args[0].span(),
                ErrorCode::TypeMismatch,
                "Memory operation has a mismatched reference type",
            ));
        }
        let r = reference.ty.reference_type().unwrap();
        if !r.mutable {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                "Cannot write through a shared reference",
            ));
        }
        let target = self.pointee(&r.target, types)?;
        Self::check_memory_target(&target, span)?;
        reference.ty = Type::reference(target.clone(), true);
        // The target is reserved before RHS effects, then activated at commit.
        let reference = self.reserve_write(reference)?;
        let expected = if matches!(intrinsic, CompilerIntrinsic::MemReplace) {
            target.clone()
        } else {
            Type::reference(target.clone(), true)
        };
        let Flow::Value(value) =
            self.expr_expected(&args[1], vars, functions, suspends, types, Some(&expected))?
        else {
            return Ok(Flow::Diverged);
        };
        if slot_type(&value) != expected {
            return Err(type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Memory operation has mismatched value types",
            ));
        }
        if matches!(intrinsic, CompilerIntrinsic::MemSwap) {
            self.emit(Op::Buffer(BufferOp::SwapReferences {
                left: reference,
                right: value.unwrap(),
                target,
            }));
            return Ok(Flow::Value(None));
        }
        if target.is_owner() {
            // Preparation may fail; both values retain cleanup until the infallible commit.
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
                return self
                    .read_owner(place, &value, vec![], target)
                    .map(Flow::Value);
            }
            self.emit(Op::Buffer(BufferOp::Swap {
                reference,
                value: value.clone(),
                place: None,
            }));
            return Ok(Flow::Value(Some(value)));
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
        Ok(Flow::Value(dst))
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
            if callee == "alloc" {
                if !type_args.is_empty() || args.len() != 1 {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "alloc expects one argument and no type arguments",
                    ));
                }
                let hint = expected.and_then(Type::reference_type).map(|r| &r.target);
                let Flow::Value(value) =
                    self.expr_expected(&args[0], vars, functions, suspends, types, hint)?
                else {
                    return Ok(Some(Flow::Diverged));
                };
                let target = slot_type(&value);
                if target.contains_fixed_array()
                    || !target.is_copy_storage_value()
                    || target.reference_type().is_some()
                {
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
        }
        let (place, borrow, mutable) = match expr {
            Expr::Unary {
                op: UnaryOp::Borrow | UnaryOp::BorrowMut,
                operand,
                ..
            } => (
                operand.as_ref(),
                true,
                matches!(
                    expr,
                    Expr::Unary {
                        op: UnaryOp::BorrowMut,
                        ..
                    }
                ),
            ),
            Expr::FieldAssign { target, .. } => (target.as_ref(), false, true),
            _ => (expr, false, false),
        };
        if let Some(flow) =
            self.dereferenced_place(place, vars, functions, suspends, types, !borrow, mutable)?
        {
            let Flow::Value(Some(reference)) = flow else {
                return Ok(Some(Flow::Diverged));
            };
            let r = reference.ty.reference_type().unwrap();
            let target = r.target.clone();
            if mutable && !r.mutable {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidAssignment,
                    "Cannot write or reborrow through a shared reference as mutable",
                ));
            }
            if borrow {
                if target.reference_type().is_some()
                    || super::indexing::is_index_place(place)
                        && !matches!(target, Type::BufferSlice(_))
                        && target.contains_references()
                {
                    return Err(type_error(
                        &expr.span(),
                        ErrorCode::TypeMismatch,
                        if target.reference_type().is_some() {
                            "Borrowing a reference value is not supported"
                        } else {
                            "Borrowing reference-bearing element storage is not supported"
                        },
                    ));
                }
                let dst = self.slot(Type::reference(target, mutable))?;
                self.emit(Op::Reference {
                    dst: dst.clone(),
                    src: reference,
                    moving: false,
                });
                return Ok(Some(Flow::Value(Some(dst))));
            }
            Self::check_memory_target(&target, expr.span())?;
            if let Expr::FieldAssign {
                value, op, span, ..
            } = expr
            {
                let reference = self.reserve_write(reference)?;
                let old = self.compound_snapshot(
                    op,
                    target.clone(),
                    None,
                    Some((reference.clone(), vec![])),
                    span,
                )?;
                let Flow::Value(value) =
                    self.expr_expected(value, vars, functions, suspends, types, Some(&target))?
                else {
                    return Ok(Some(Flow::Diverged));
                };
                let value = self.compound_value(op, old, value, span)?;
                if slot_type(&value) != target {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "Dereference assignment has the wrong type",
                    ));
                }
                if target.is_owner() {
                    self.replace_capture(reference, value.unwrap())?;
                } else {
                    self.emit(Op::WriteMemory {
                        reference,
                        path: vec![],
                        value,
                        target,
                    });
                }
                return Ok(Some(Flow::Value(None)));
            }
            if !target.is_copy_value() {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::InvalidAssignment,
                    "Cannot move a non-Copy value through a reference; use mem.replace",
                ));
            }
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
        if let Some(flow) = self.local_field_expr(expr, vars, functions, suspends, types)? {
            return Ok(Some(flow));
        }
        // Stop at the nearest reference; recursively evaluate earlier pointer hops.
        let field = match expr {
            Expr::Field { .. } => expr,
            Expr::FieldAssign { target, .. } => target,
            _ if borrow && matches!(place, Expr::Field { .. }) => place,
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
            target = self.field_member(&target, name, span)?.1;
        }
        if borrow && !target.is_place() {
            return Ok(None);
        }
        if borrow && mutable && !r.mutable {
            return Err(type_error(
                &expr.span(),
                ErrorCode::InvalidAssignment,
                "Cannot borrow an array field through a shared reference as mutable",
            ));
        }
        if target.is_owner() && !borrow {
            return Err(type_error(&expr.span(), ErrorCode::InvalidAssignment,
                "Cannot move or replace an owning field through a borrowed wrapper; borrow the field instead"));
        }
        let Flow::Value(reference) = self.expr(root, vars, functions, suspends, types)? else {
            return Ok(Some(Flow::Diverged));
        };
        let mut reference = reference.unwrap();
        reference.ty = Type::reference(pointee, r.mutable);
        let path: Vec<_> = path.into_iter().map(|(name, _)| name).collect();
        if borrow {
            let dst = self.slot(Type::reference(target, mutable))?;
            self.emit(Op::Project {
                dst: dst.clone(),
                src: reference,
                path,
                reservation: false,
            });
            return Ok(Some(Flow::Value(Some(dst))));
        }

        if let Expr::FieldAssign {
            value, op, span, ..
        } = expr
        {
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
            let old = self.compound_snapshot(
                op,
                target.clone(),
                None,
                Some((reference.clone(), path.clone())),
                span,
            )?;
            let Flow::Value(value) =
                self.expr_expected(value, vars, functions, suspends, types, Some(&target))?
            else {
                return Ok(Some(Flow::Diverged));
            };
            let value = self.compound_value(op, old, value, span)?;
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
        if info.signature.ret.is_explicit_arena()
            && !matches!(info.compiler_intrinsic, Some(CompilerIntrinsic::ArenaNew))
        {
            return Err(type_error(
                &info.span,
                ErrorCode::TypeMismatch,
                "Returning an Arena owner is not supported; pass &Arena to allocate result storage",
            ));
        }
        let references = info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(|ty| {
                ty.contains_fixed_array()
                    || ty.contains_references()
                    || ty.contains_buffers() && *ty != Type::String
            });
        // Reference-bearing helpers are never C/VM entries. Allocation and
        // reclamation need the hidden arena argument. borrowing::check also imposes
        // storage preconditions on writable reference-element buffer signatures.
        if references || body.has_references() || body.has_buffers() {
            reference_capability.insert(name.clone());
        }
        info.uses_arena = body.ops.iter().any(|op| match op {
            Op::Alloc { .. } | Op::Text { .. } | Op::DomainOwner { .. } | Op::Place(_) => true,
            // Local fixed-array aliases become Place operations after this pass,
            // including unused by-value parameters. Lowering needs an arena.
            Op::Local { target, .. } if target.is_place() => true,
            Op::Buffer(BufferOp::SwapReferences { target, .. }) if target.is_place() => true,
            Op::Buffer(BufferOp::Storage { intrinsic, .. }) => intrinsic.allocates(),
            _ => false,
        });
        info.needs_arena = body.is_destructor || body.has_buffers() || addressed.contains(name) || info.uses_arena || body.ops.iter().any(|op| matches!(op, Op::ArenaNew { .. } | Op::ArenaAlloc { .. })) || body.ops.iter().any(|op| {
            matches!(op, Op::Buffer(BufferOp::Storage { intrinsic, .. }) if intrinsic.needs_arena())
        });
    }
    loop {
        let mut changed = false;
        for (name, body) in bodies {
            if !reference_capability.contains(name)
                && body.ops.iter().any(|op| match op {
                    Op::Call { name, .. } => reference_capability.contains(name),
                    Op::Concurrent { branches, .. } => branches
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
                    Op::Concurrent { branches, .. } => branches
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
                && body.ops.iter().any(|op| {
                    op.cleanups().any(|cleanup| {
                        let CleanupOp::Register {
                            hooks, elements, ..
                        } = cleanup;
                        hooks
                            .iter()
                            .chain(elements.iter().flat_map(|e| &e.hooks))
                            .any(|hook| functions[&hook.name].uses_arena)
                    }) || match op {
                        Op::Call { name, .. } => functions[name].uses_arena,
                        Op::Concurrent { branches, .. } => branches
                            .iter()
                            .any(|branch| functions[&branch.name].uses_arena),
                        Op::TaskBranch { entry, .. } => functions[entry].uses_arena,
                        _ => false,
                    }
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
    let domain_targets: std::collections::HashSet<_> = bodies
        .values()
        .flat_map(|b| b.domain_calls.iter().map(|(name, _)| name.as_str()))
        .collect();
    for (name, body) in bodies {
        let info = functions.get_mut(name).unwrap();
        info.instance_entry = !info.internal || domain_targets.contains(name.as_str());
        // References carry nominal identities; validate the full resolved root.
        for index in 0..info.signature.params.len() {
            if (!info.internal || domain_targets.contains(name.as_str()))
                && !info.suspends
                && info.parameter_noescape(index)
            {
                if let Some(reference) = info.signature.params[index]
                    .reference_type()
                    .filter(|r| matches!(r.target, Type::Record(_)))
                {
                    let target = body.pointee(&reference.target, &body.type_registry)?;
                    info.signature.params[index] = Type::reference(target, reference.mutable);
                }
            }
        }
        info.instance_entry = crate::core::instance::eligible(info);
        if info.instance_entry {
            let root = if crate::core::instance::constructor(info) {
                &info.signature.ret
            } else {
                &info.signature.params[0].reference_type().unwrap().target
            };
            info.instance_entry =
                crate::core::instance::persistent_graph(root, &body.nominals, &body.type_registry)?;
        }
        // Preserve source visibility before filtering the ordinary host ABI.
        info.script_entry = !info.internal;
        info.parallel_entry = !info.internal && info.suspends;
        let references = info
            .signature
            .params
            .iter()
            .chain(std::iter::once(&info.signature.ret))
            .any(|ty| {
                ty.contains_fixed_array()
                    || ty.contains_references()
                    || ty.contains_buffers() && *ty != Type::String
            });
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
    crate::core::state_domain::infer(functions, bodies);
    super::every::validate(functions)?;
    // Runtime schema views must retain definitions behind buffers after the
    // compiler's nominal table has gone away. Storage/ABI lanes stay identical.
    for (name, info) in functions
        .iter_mut()
        .filter(|(_, info)| info.domain_entry || info.domain_request_entry)
    {
        let body = &bodies[name];
        for ty in &mut info.signature.params {
            *ty = super::domain_schema::freeze(body, ty)?;
        }
        info.signature.ret = super::domain_schema::freeze(body, &info.signature.ret)?;
    }

    for body in bodies.values() {
        for (operation, span) in &body.domain_calls {
            if !functions
                .get(operation)
                .is_some_and(|info| info.domain_entry)
            {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Domain operation must be a proved synchronous, host-free flat state entry",
                ));
            }
        }
    }
    for (name, body) in bodies {
        let info = &functions[name];
        if info.signature.ret.contains_closures()
            && !info.signature.ret.is_scoped_shared_value()
            && (info.signature.ret.contains_unstorable_closures()
                || info.signature.ret.reference_type().is_some())
        {
            return Err(type_error(
                &info.span,
                ErrorCode::TypeMismatch,
                "Only reference-free move closure values can be returned",
            ));
        }
        if info.suspends
            && info
                .signature
                .params
                .iter()
                .any(Type::contains_unstorable_closures)
        {
            return Err(type_error(
                &info.span,
                ErrorCode::InvalidCall,
                "Only reference-free move closure values can enter suspending frames",
            ));
        }
        for (index, ty) in info.signature.params.iter().enumerate() {
            if ty.contains_dyn_view() && !info.parameter_noescape(index) {
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
                Op::Local { target, .. } if !target.is_place() => Some(target),
                _ => None,
            };
            for target in storage
                .into_iter()
                .chain(op.cleanup_storage().map(|(_, ty)| ty))
            {
                if info.suspends
                    && !target.is_copy_value()
                    && !target.is_suspend_safe_owner()
                    && !target.is_callable_parameter()
                {
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
            if let Op::Alloc { target, .. } | Op::ArenaAlloc { target, .. } = op {
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
                if let Type::Function(contract) = &slot.ty {
                    let target = &functions[name];
                    if contract.noescape
                        && target
                            .signature
                            .params
                            .iter()
                            .enumerate()
                            .any(|(index, ty)| {
                                ty.contains_references() && !target.parameter_noescape(index)
                            })
                    {
                        return Err(type_error(span, ErrorCode::InvalidCall,
                            "Function value does not satisfy @noescape: an argument loan may escape"));
                    }
                }
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
