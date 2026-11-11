//! Stable function storage and lexical lifetimes for borrowed locals and mixed owners.
use super::*;
use std::collections::HashSet;

// A projection stays within one local object; it never follows a reference.
pub(super) fn local_path(mut expr: &Expr) -> Option<(&str, Vec<String>)> {
    let mut path = Vec::new();
    while let Expr::Field {
        base,
        name,
        type_args,
        ..
    } = expr
    {
        if !type_args.is_empty() {
            return None;
        }
        path.push(name.clone());
        expr = base;
    }
    let Expr::Ident { name, .. } = expr else {
        return None;
    };
    path.reverse();
    Some((name, path))
}

#[derive(Default)]
pub(super) struct BorrowedLocals {
    explicit: HashSet<String>,
    captured: HashSet<String>,
    receivers: HashMap<String, Vec<(Vec<String>, String, Span)>>,
}

impl BorrowedLocals {
    pub(super) fn require_place(&mut self, name: String) {
        self.explicit.insert(name);
    }
    fn receiver(&mut self, root: &str, path: Vec<String>, method: &str, span: Span) {
        self.receivers
            .entry(root.into())
            .or_default()
            .push((path, method.into(), span));
    }

    fn requires_place(&self, name: &str, target: &Type, plan: &FunctionPlan) -> bool {
        self.explicit.contains(name)
            || self.receivers.get(name).is_some_and(|calls| {
                calls.iter().any(|(path, name, span)| {
                    path.iter()
                        .try_fold(target, |ty, field| ty.inline_field(field).map(|(_, ty)| ty))
                        .is_some_and(|ty| {
                            let context = plan.generics.borrow();
                            if name == "$eq" {
                                if ty.is_numeric()
                                    || matches!(
                                        ty,
                                        Type::Bool | Type::Rune | Type::String | Type::Reference(_)
                                    )
                                {
                                    return false;
                                }
                                return context
                                    .members
                                    .traits
                                    .definitions
                                    .iter()
                                    .find(|d| d.is_eq)
                                    .is_some_and(|d| {
                                        context
                                            .members
                                            .trait_method(
                                                ty,
                                                "eq",
                                                Some(d.self_parameter.id),
                                                &plan.scope,
                                                *span,
                                            )
                                            .ok()
                                            .flatten()
                                            .is_some()
                                    });
                            }
                            let selected = context.members.trait_calls.borrow().get(span).copied();
                            let inherent = context
                                .members
                                .method(ty, name, plan.definition_scope.as_deref())
                                .filter(|_| selected.is_none() && plan.member_visible(ty, name));
                            let method = inherent.or_else(|| {
                                context
                                    .members
                                    .trait_method(ty, name, selected, &plan.scope, *span)
                                    .ok()
                                    .flatten()
                            });
                            method.is_some_and(|m| {
                                matches!(m.receiver, Some(members::Receiver::Borrowed { .. }))
                            })
                        })
                })
            })
    }
}

// Collect spellings conservatively before planning so a borrow in one branch
// never changes another branch's representation. Shadowed bindings get distinct
// places; matching an unused spelling can only materialize extra local storage.
pub(super) fn borrowed_names(body: &Expr) -> BorrowedLocals {
    let mut names = BorrowedLocals::default();
    let mut pending = vec![body];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::When(_) => {}
            Expr::AnonymousFunction(function) => {
                let mut captures = vec![&function.body];
                while let Some(expr) = captures.pop() {
                    match expr {
                        Expr::Ident { name, .. }
                        | Expr::Assign { name, .. }
                        | Expr::Call { callee: name, .. } => {
                            names
                                .captured
                                .insert(name.split('.').next().unwrap().to_owned());
                            names
                                .explicit
                                .insert(name.split('.').next().unwrap().to_owned());
                        }
                        _ => {}
                    }
                    captures.extend(closures::children(expr));
                }
                pending.push(&function.body);
            }
            Expr::Invoke { callee, args, .. } => {
                pending.push(callee);
                pending.extend(args);
            }
            Expr::Unary { op, operand, .. } => {
                if matches!(op, UnaryOp::Borrow | UnaryOp::BorrowMut) {
                    if let Some((name, _)) = local_path(operand) {
                        names.explicit.insert(name.to_owned());
                    }
                }
                pending.push(operand);
            }
            Expr::Index { base, index, .. } => {
                if let Some((name, _)) = local_path(base) {
                    names.explicit.insert(name.to_owned());
                }
                pending.extend([base.as_ref(), index.as_ref()]);
            }
            Expr::Field { base, .. } => pending.push(base),
            Expr::Let { value, .. }
            | Expr::Var { value, .. }
            | Expr::Assign { value, .. }
            | Expr::Destructure { value, .. }
            | Expr::Spread { value, .. } => pending.push(value),
            Expr::Return { value, .. } => pending.extend(value.iter().map(Box::as_ref)),
            Expr::BinaryChain { .. } | Expr::IfChain { .. } | Expr::LetElse(_) => {
                pending.extend(expr.children())
            }
            Expr::BinOp {
                lhs,
                op: BinOp::Eq | BinOp::Ne,
                rhs,
                span,
            } => {
                for operand in [lhs.as_ref(), rhs.as_ref()] {
                    if let Some((root, path)) = local_path(operand) {
                        names.receiver(root, path, "$eq", *span);
                    }
                }
                pending.extend([lhs.as_ref(), rhs.as_ref()]);
            }
            Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
                pending.extend([lhs.as_ref(), rhs.as_ref()])
            }
            Expr::FieldAssign { target, value, .. } => {
                pending.extend([target.as_ref(), value.as_ref()])
            }
            Expr::IndexedLoop(loop_) => pending.push(&loop_.body),
            Expr::While {
                condition, body, ..
            } => pending.extend([condition.as_ref(), body.as_ref()]),
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                pending.extend([condition.as_ref(), then_branch.as_ref()]);
                pending.extend(else_branch.iter().map(Box::as_ref));
            }
            Expr::Match { value, arms, .. } => {
                pending.push(value);
                pending.extend(
                    arms.iter()
                        .flat_map(|arm| arm.guard.iter().chain(std::iter::once(&arm.body))),
                );
            }
            Expr::Record { fields, base, .. } => {
                pending.extend(fields.iter().map(|field| &field.value));
                pending.extend(base.iter().map(Box::as_ref));
            }
            Expr::MethodCall(call) => {
                if let Some((root, path)) = local_path(&call.receiver) {
                    names.receiver(root, path, &call.name, call.span);
                }
                pending.push(&call.receiver);
                pending.extend(&call.args);
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                if let Some((receiver, method)) = callee.rsplit_once('.') {
                    let mut parts = receiver.split('.');
                    let root = parts.next().unwrap();
                    names.receiver(root, parts.map(str::to_owned).collect(), method, *span);
                }
                pending.extend(args);
            }
            Expr::Array { elements, .. } | Expr::Tuple { elements, .. } => pending.extend(elements),
            Expr::Concurrent { branches, .. } => pending.extend(branches),
            Expr::Unsafe { exprs, .. }
            | Expr::Block { exprs, .. }
            | Expr::Graph { exprs, .. }
            | Expr::Sequence { exprs, .. } => pending.extend(exprs),
            Expr::Isolate { body, .. }
            | Expr::TaskScope { body, .. }
            | Expr::Branch { body, .. } => pending.push(body),
            Expr::Literal { .. }
            | Expr::Ident { .. }
            | Expr::GenericName(_)
            | Expr::FunctionRef { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }
    names
}

fn names_capture_place(names: &BorrowedLocals, name: &str) -> bool {
    names.captured.contains(name)
}

impl FunctionPlan {
    /// Reserve backing bytes after reference checking and template removal.
    /// A frame's slot allocation is stable across polls; loop entries reuse it.
    pub(super) fn reserve_cps_locals(&mut self) -> Result<(), Error> {
        for (op, span) in self.ops.iter_spanned() {
            let local = match op {
                Op::Local { dst, target, .. } if !target.is_place() => Some((dst.index, target)),
                _ => None,
            };
            for (key, target) in local.into_iter() {
                let layout = target
                    .storage_layout()
                    .map_err(|e| type_error(span, ErrorCode::ResourceLimit, e.to_string()))?;
                if layout.align > std::mem::align_of::<u64>() {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidState,
                        "CPS local alignment exceeds frame alignment",
                    ));
                }
                let end = self
                    .slot_count
                    .checked_add(layout.size.max(1).div_ceil(8))
                    .filter(|&end| end < i32::MAX as usize / 8)
                    .ok_or_else(|| {
                        type_error(span, ErrorCode::ResourceLimit, "CPS frame is too large")
                    })?;
                self.cps_local_slots.insert(key, self.slot_count);
                self.slot_count = end;
            }
        }
        Ok(())
    }

    pub(super) fn local_binding(
        &mut self,
        name: &str,
        mut binding: Binding,
    ) -> Result<Binding, Error> {
        let target = slot_type(&binding.slot);
        if !name.starts_with('$') {
            if let Some(slot) = &binding.slot {
                for scalar in slot.scalars().filter(|s| s.ty.is_owner()) {
                    self.diagnostic_bindings.insert(
                        scalar.index,
                        (name.to_owned(), self.current_span, target.clone()),
                    );
                }
            }
        }

        if target.is_owner() {
            let owner = binding.slot.as_ref().unwrap();
            let scope = self.local_scopes.last_mut().unwrap();
            if !scope.iter().any(|slot| slot.index == owner.index) {
                scope.push(owner.clone());
            }
            if !target.is_inline_owner() {
                return Ok(binding);
            }
        }
        if target.is_closure_value() {
            if let Some(place) = binding
                .slot
                .as_ref()
                .and_then(|s| self.closure_places.get(&s.index))
                .cloned()
            {
                let scope = self.local_scopes.last_mut().unwrap();
                if !scope.iter().any(|local| local.index == place.index) {
                    scope.push(place.clone());
                }
                if target.is_owner() {
                    self.register_owner(binding.slot.clone().unwrap(), Some(place.clone()));
                }
                binding.place = Some(place);
                return Ok(binding);
            }
        }
        let needs_place = self.borrowed_names.requires_place(name, &target, self);
        if target.is_place()
            || target.is_closure_value()
            || names_capture_place(&self.borrowed_names, name)
            || target.is_inline_owner()
            || needs_place
                && !target.contains_references()
                && (target.is_copy_scalar() || target.is_aggregate())
        {
            let dst = self.slot(Type::reference(target.clone(), true))?;
            self.emit(Op::Local {
                dst: dst.clone(),
                value: binding.slot.clone(),
                target,
            });
            self.local_scopes.last_mut().unwrap().push(dst.clone());
            if let Some(owner) = binding.slot.as_ref().filter(|s| s.ty.is_owner()) {
                self.register_owner(owner.clone(), Some(dst.clone()));
            }
            binding.place = Some(dst);
        }
        Ok(binding)
    }

    pub(super) fn read_local(&mut self, reference: Slot) -> Result<Option<Slot>, Error> {
        let target = reference.ty.reference_type().unwrap().target.clone();
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
        Ok(dst)
    }

    pub(super) fn read_owner(
        &mut self,
        reference: Slot,
        owner: &Slot,
        path: Vec<String>,
        target: Type,
    ) -> Result<Option<Slot>, Error> {
        if target.is_owner() && self.captured_owners.contains(&owner.index) {
            return Err(type_error(
                &self.current_span,
                ErrorCode::InvalidAssignment,
                if self.consuming_captures {
                    "Move capture requires a consuming closure entry"
                } else {
                    "Borrowed closure captures cannot consume non-Copy values; use move fn"
                },
            ));
        }
        let capture = self.captured_fields.get(&owner.index).cloned();
        let (root, ownership_path, detach) = if let Some((root, prefix)) = capture {
            (
                root,
                prefix.into_iter().chain(path.iter().cloned()).collect(),
                true,
            )
        } else {
            (owner.clone(), path.clone(), false)
        };
        let dst = Some(self.slot(target.clone())?);
        if !target.is_owner() {
            self.emit(Op::ReadMemory {
                dst: dst.clone(),
                reference,
                path: path.clone(),
            });
        } else if !target.is_inline_owner() && detach {
            self.emit(Op::Buffer(BufferOp::Detach {
                dst: dst.clone().unwrap(),
                src: owner.field_path(&path),
                owner: root,
            }));
        } else {
            self.emit(Op::ReadOwner {
                dst: dst.clone(),
                reference,
                path: path.clone(),
                owner: root,
                ownership_path,
                detach,
            });
        }
        if target.is_closure_value() {
            let dst = dst.clone().unwrap();
            // Owning extraction is checked through its containing owner. Local
            // borrowing environments still need their separate affine move.
            let op = if target.is_owner() {
                closures::ClosureOp::Init(dst)
            } else {
                closures::ClosureOp::Move {
                    dst,
                    src: owner.field_path(&path),
                }
            };
            self.emit(Op::Closure(Box::new(op)));
        }
        Ok(dst)
    }

    pub(super) fn access_owner(&mut self, owner: &Slot, mutable: bool) {
        if self.captured_owners.contains(&owner.index) {
            return;
        }
        self.emit(Op::Buffer(BufferOp::Access {
            src: owner.clone(),
            mutable,
        }));
    }

    fn owner_borrow_target(&self, target: &Type, span: Span) -> Result<(), Error> {
        if !target.is_owner()
            && !target.is_callable_parameter()
            && !(target.is_copy_storage_value() && !target.contains_references())
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                "Only owning or reference-free Copy fields can be borrowed from owning records",
            ));
        }
        Ok(())
    }

    pub(super) fn borrow_local(
        &mut self,
        operand: &Expr,
        mutable: bool,
        vars: &HashMap<String, Binding>,
        suspends: bool,
        span: Span,
    ) -> Result<Flow, Error> {
        let Some((name, path)) = local_path(operand) else {
            return Err(type_error(&span, ErrorCode::InvalidAssignment, "Borrowing requires a local binding or its inline record field; temporary borrowing is not supported"));
        };
        let binding = vars.get(name).ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::UnknownName,
                format!("Undefined local binding '{name}'"),
            )
        })?;
        if let Some(src) = binding
            .slot
            .as_ref()
            .filter(|s| s.ty.reference_type().is_some())
        {
            let r = src.ty.reference_type().unwrap();
            let root = self.pointee(&r.target, &self.type_registry)?;
            if !path.is_empty()
                && (root.is_owner() || root.is_copy_storage_value() && !root.contains_references())
            {
                if mutable && !r.mutable {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidAssignment,
                        "Cannot borrow a field through a shared reference as mutable",
                    ));
                }
                let target = self.field_target(root.clone(), &path, span)?;
                self.owner_borrow_target(&target, span)?;
                let dst = self.slot(Type::reference(target, mutable))?;
                let mut src = src.clone();
                src.ty = Type::reference(root.clone(), r.mutable);
                if root.is_inline_owner() || root.is_copy_value() {
                    self.emit(Op::Project {
                        dst: dst.clone(),
                        src,
                        path,
                        reservation: false,
                    });
                } else {
                    self.emit(Op::Reference {
                        dst: dst.clone(),
                        src,
                        moving: false,
                    });
                }
                return Ok(Flow::Value(Some(dst)));
            }
        }
        let ty = slot_type(&binding.slot);
        if suspends
            && !(ty.is_copy_value()
                || path.is_empty() && ty.is_scoped_suspending_closure()
                || path.is_empty() && ty == Type::String
                || self
                    .pointee(
                        ty.reference_type().map(|r| &r.target).unwrap_or(&ty),
                        &self.type_registry,
                    )
                    .and_then(|target| self.field_target(target, &path, span))
                    .is_ok_and(|t| {
                        t.is_suspend_safe_local_owner()
                            // Copy fields share their owning root's stable CPS storage.
                            || ty.is_suspend_safe_owner()
                                && t.is_copy_storage_value()
                                && !t.contains_references()
                    }))
        {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Local borrowing is only supported in synchronous functions",
            ));
        }
        if mutable && !binding.mutable {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                "Exclusive local borrowing requires a var binding",
            ));
        }
        if binding
            .slot
            .as_ref()
            .is_some_and(|s| self.captured_owners.contains(&s.index) && s.ty.is_owner())
        {
            let target = self.field_target(slot_type(&binding.slot), &path, span)?;
            self.owner_borrow_target(&target, span)?;
            let dst = self.slot(Type::reference(target, mutable))?;
            if path.is_empty() {
                self.emit(Op::Reference {
                    dst: dst.clone(),
                    src: binding.place.clone().unwrap(),
                    moving: false,
                });
            } else {
                self.emit(Op::Project {
                    dst: dst.clone(),
                    src: binding.place.clone().unwrap(),
                    path,
                    reservation: false,
                });
            }
            return Ok(Flow::Value(Some(dst)));
        }
        if let Some(src) = binding.slot.as_ref().filter(|slot| slot.ty.is_owner()) {
            let target = self.field_target(src.ty.clone(), &path, span)?;
            self.owner_borrow_target(&target, span)?;
            let project = binding.place.is_some() && !path.is_empty();
            let dst = self.slot(Type::reference(
                if project {
                    src.ty.clone()
                } else {
                    target.clone()
                },
                mutable,
            ))?;
            self.emit(Op::Buffer(BufferOp::Borrow {
                reservation: false,
                dst: dst.clone(),
                src: src.clone(),
                place: binding.place.clone(),
            }));
            if project {
                let field = self.slot(Type::reference(target, mutable))?;
                self.emit(Op::Project {
                    dst: field.clone(),
                    src: dst,
                    path,
                    reservation: false,
                });
                return Ok(Flow::Value(Some(field)));
            }
            return Ok(Flow::Value(Some(dst)));
        }
        let src = binding.place.clone().ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Local borrowing requires Copy data without references, String or opaque handles",
            )
        })?;
        let target = self.field_target(slot_type(&binding.slot), &path, span)?;
        let dst = self.slot(Type::reference(target, mutable))?;
        if path.is_empty() {
            self.emit(Op::Reference {
                dst: dst.clone(),
                src,
                moving: false,
            });
        } else {
            self.emit(Op::Project {
                dst: dst.clone(),
                src,
                path,
                reservation: false,
            });
        }
        Ok(Flow::Value(Some(dst)))
    }

    pub(super) fn end_locals_from(&mut self, depth: usize) {
        let locals: Vec<_> = self.local_scopes[depth..]
            .iter()
            .flatten()
            .cloned()
            .collect();
        for local in locals.into_iter().rev() {
            if self.result.as_ref().is_some_and(|result| {
                result.index == local.index && (result.ty.is_owner() || result.ty.is_place())
            }) {
                continue;
            }
            if local.ty.is_closure_value() {
                self.emit(Op::Closure(Box::new(closures::ClosureOp::End(
                    local.clone(),
                ))));
            }
            self.emit(Op::EndLocal(local));
        }
    }

    pub(super) fn escape_owner(&mut self, flow: &Flow) {
        if let Flow::Value(Some(slot)) = flow {
            if slot.ty.is_owner() || slot.ty.is_closure_value() || slot.ty.is_place() {
                self.local_scopes
                    .last_mut()
                    .unwrap()
                    .retain(|local| local.index != slot.index);
                let parent = self.local_scopes.len() - 2;
                if let Some(place) = self.closure_places.get(&slot.index).cloned() {
                    self.local_scopes
                        .last_mut()
                        .unwrap()
                        .retain(|local| local.index != place.index);
                    if !self.local_scopes[parent]
                        .iter()
                        .any(|local| local.index == place.index)
                    {
                        self.local_scopes[parent].push(place);
                    }
                }
                self.local_scopes[parent].push(slot.clone());
            }
        }
    }

    pub(super) fn end_local_scope(&mut self) {
        let locals = self.local_scopes.pop().unwrap();
        for local in locals.into_iter().rev() {
            if self.result.as_ref().is_some_and(|result| {
                result.index == local.index && (result.ty.is_owner() || result.ty.is_place())
            }) {
                continue;
            }
            self.emit(Op::EndLocal(local));
        }
    }

    /// Erase analysis-only markers and lower checked storage in source order.
    pub(super) fn lower_checked_storage(&mut self) {
        self.ops.retain_mut(|op| {
            if matches!(
                op,
                Op::EndLocal(_) | Op::Closure(_) | Op::Buffer(BufferOp::Access { .. })
            ) {
                return false;
            }
            let next = match op {
                Op::Buffer(BufferOp::End(owner)) if owner.ty.is_place() => {
                    Some(PlaceOp::End(owner.clone()))
                }
                Op::Buffer(BufferOp::Replace {
                    src,
                    owner,
                    place,
                    path,
                    ..
                }) if owner.ty.is_place() => Some(PlaceOp::Write {
                    reference: place.clone(),
                    value: src.clone(),
                    offset: owner.ty.storage_field_path(path).unwrap().0,
                }),
                Op::Buffer(BufferOp::SwapReferences {
                    left,
                    right,
                    target,
                }) if target.is_place() => {
                    let temporary = self.slot_count;
                    self.slot_count += 1;
                    Some(PlaceOp::Swap {
                        left: left.clone(),
                        right: right.clone(),
                        target: target.clone(),
                        temporary,
                    })
                }
                Op::Local {
                    dst,
                    value: Some(src),
                    target,
                } if target.is_place() => Some(PlaceOp::Alias {
                    dst: dst.clone(),
                    src: src.clone(),
                }),
                Op::ReadMemory {
                    dst: Some(dst),
                    reference,
                    path,
                }
                | Op::ReadOwner {
                    dst: Some(dst),
                    reference,
                    path,
                    ..
                } if dst.ty.is_place()
                    || reference.ty.reference_type().unwrap().target.is_place() =>
                {
                    Some(PlaceOp::Read {
                        offset: reference
                            .ty
                            .reference_type()
                            .unwrap()
                            .target
                            .storage_field_path(path)
                            .unwrap()
                            .0,
                        dst: dst.clone(),
                        reference: reference.clone(),
                    })
                }
                Op::WriteMemory {
                    reference,
                    path,
                    value: Some(value),
                    target,
                }
                | Op::StoreOwner {
                    reference,
                    path,
                    value: Some(value),
                    target,
                } if target.is_place()
                    || reference.ty.reference_type().unwrap().target.is_place() =>
                {
                    Some(PlaceOp::Write {
                        offset: reference
                            .ty
                            .reference_type()
                            .unwrap()
                            .target
                            .storage_field_path(path)
                            .unwrap()
                            .0,
                        reference: reference.clone(),
                        value: value.clone(),
                    })
                }
                _ => None,
            };
            if let Some(next) = next {
                *op = Op::Place(next);
            }
            true
        });
    }

    pub(super) fn local_field_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Option<Flow>, Error> {
        let field = match expr {
            Expr::Field { .. } => expr,
            Expr::FieldAssign { target, .. } => target,
            _ => return Ok(None),
        };
        let Some((name, path)) = local_path(field) else {
            return Ok(None);
        };
        let Some(binding) = vars.get(name) else {
            return Ok(None);
        };
        // A captured reference lives in an environment field. Load it through
        // the ordinary reference projection path before accessing its pointee.
        if slot_type(&binding.slot).reference_type().is_some() {
            return Ok(None);
        }
        let Some(reference) = binding.place.clone() else {
            return Ok(None);
        };
        let target = self.field_target(slot_type(&binding.slot), &path, expr.span())?;
        let owner = binding
            .slot
            .as_ref()
            .filter(|slot| slot.ty.is_owner())
            .cloned();
        if let Some(owner) = &owner {
            self.access_owner(owner, matches!(expr, Expr::FieldAssign { .. }));
        }
        if let Expr::FieldAssign {
            value, op, span, ..
        } = expr
        {
            if !binding.mutable {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Cannot assign to an immutable local; declare it with var",
                ));
            }
            if op.is_some() && !target.is_numeric() {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Compound assignment requires a supported numeric type",
                ));
            }
            if target.is_owner() {
                if owner.as_ref().is_some_and(|s| {
                    self.captured_owners.contains(&s.index)
                        || self.captured_fields.contains_key(&s.index)
                }) {
                    let projected = self.slot(Type::reference(target.clone(), true))?;
                    self.emit(Op::Project {
                        dst: projected.clone(),
                        src: reference.clone(),
                        path: path.clone(),
                        reservation: true,
                    });
                    let Flow::Value(Some(value)) =
                        self.expr_expected(value, vars, functions, suspends, types, Some(&target))?
                    else {
                        return Ok(Some(Flow::Diverged));
                    };
                    if value.ty != target {
                        return Err(type_error(
                            span,
                            ErrorCode::TypeMismatch,
                            "Captured field assignment has the wrong type",
                        ));
                    }
                    self.replace_capture(projected, value)?;
                    return Ok(Some(Flow::Value(None)));
                }
                let (_, dst) = records::place(self, field, vars)?;
                let Flow::Value(value) =
                    self.expr_expected(value, vars, functions, suspends, types, Some(&target))?
                else {
                    return Ok(Some(Flow::Diverged));
                };
                if slot_type(&value) != target {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "Local field assignment has the wrong type",
                    ));
                }
                // A field write cannot resurrect a record consumed by its RHS.
                self.access_owner(owner.as_ref().unwrap(), true);
                self.emit(Op::Buffer(BufferOp::Replace {
                    owner: owner.clone().unwrap(),
                    dst: dst.unwrap(),
                    src: value.unwrap(),
                    place: reference.clone(),
                    path,
                }));
                self.register_owner(owner.unwrap(), Some(reference));
                return Ok(Some(Flow::Value(None)));
            }
            let dst = self.slot(Type::reference(target.clone(), true))?;
            self.emit(Op::Project {
                dst: dst.clone(),
                src: reference,
                path,
                reservation: true,
            });
            let old = self.compound_snapshot(
                op,
                target.clone(),
                None,
                Some((dst.clone(), vec![])),
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
                    "Local field assignment has the wrong type",
                ));
            }
            if let Some(owner) = &owner {
                self.access_owner(owner, true);
            }
            self.emit(Op::WriteMemory {
                reference: dst,
                path: vec![],
                value,
                target,
            });
            Ok(Some(Flow::Value(None)))
        } else {
            if target.is_owner() {
                return self
                    .read_owner(reference, owner.as_ref().unwrap(), path, target)
                    .map(|dst| Some(Flow::Value(dst)));
            }
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
