//! Stable function storage and lexical lifetimes for borrowed locals and mixed owners.
use super::*;
use std::collections::HashSet;

// A projection stays within one local object; it never follows a reference.
fn local_path(mut expr: &Expr) -> Option<(&str, Vec<String>)> {
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
    receivers: HashMap<String, Vec<(Vec<String>, String, Span)>>,
}

impl BorrowedLocals {
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
                        .try_fold(target, |ty, field| match ty {
                            Type::Record(record) => record
                                .fields
                                .iter()
                                .find(|f| &f.name == field)
                                .map(|f| &f.ty),
                            _ => None,
                        })
                        .is_some_and(|ty| {
                            let context = plan.generics.borrow();
                            let selected = context.members.trait_calls.borrow().get(span).copied();
                            let inherent = context
                                .members
                                .method(ty, name)
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
            Expr::Field { base, .. } => pending.push(base),
            Expr::Let { value, .. }
            | Expr::Var { value, .. }
            | Expr::Assign { value, .. }
            | Expr::Destructure { value, .. } => pending.push(value),
            Expr::Return { value, .. } => pending.extend(value.iter().map(Box::as_ref)),
            Expr::BinOp { lhs, rhs, .. } | Expr::Logical { lhs, rhs, .. } => {
                pending.extend([lhs.as_ref(), rhs.as_ref()])
            }
            Expr::FieldAssign { target, value, .. } => {
                pending.extend([target.as_ref(), value.as_ref()])
            }
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
                pending.extend(arms.iter().map(|arm| &arm.body));
            }
            Expr::Record { fields, .. } => pending.extend(fields.iter().map(|field| &field.value)),
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
            Expr::Tuple { elements, .. } => pending.extend(elements),
            Expr::Scope { branches, .. } => pending.extend(branches),
            Expr::Unsafe { exprs, .. }
            | Expr::Block { exprs, .. }
            | Expr::Graph { exprs, .. }
            | Expr::Sequence { exprs, .. } => pending.extend(exprs),
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

impl FunctionPlan {
    pub(super) fn local_binding(
        &mut self,
        name: &str,
        mut binding: Binding,
    ) -> Result<Binding, Error> {
        let target = slot_type(&binding.slot);
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
        let needs_place = self.borrowed_names.requires_place(name, &target, self);
        if target.is_inline_owner()
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
        self.check_owner_projection(owner.ty.clone(), &path, self.current_span)?;
        let dst = Some(self.slot(target)?);
        self.emit(Op::ReadOwner {
            dst: dst.clone(),
            reference,
            path,
            owner: owner.clone(),
        });
        Ok(dst)
    }

    fn access_owner(&mut self, owner: &Slot, mutable: bool) {
        self.emit(Op::Buffer(BufferOp::Access {
            src: owner.clone(),
            mutable,
        }));
    }

    fn owner_borrow_target(&self, target: &Type, span: Span) -> Result<(), Error> {
        if !target.is_owner() {
            return Err(type_error(&span, ErrorCode::InvalidAssignment,
                "Only owning fields can be borrowed from owning records; read or write Copy fields directly"));
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
        if suspends {
            return Err(type_error(
                &span,
                ErrorCode::InvalidCall,
                "Local borrowing is only supported in synchronous functions",
            ));
        }
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
        if let Some(src) = binding.slot.as_ref().filter(|s| s.ty.is_buffer_view()) {
            if !path.is_empty() && src.ty.reference_type().unwrap().target.is_owner() {
                let r = src.ty.reference_type().unwrap();
                if mutable && !r.mutable {
                    return Err(type_error(
                        &span,
                        ErrorCode::InvalidAssignment,
                        "Cannot borrow an owning field through a shared reference as mutable",
                    ));
                }
                let root = self.pointee(&r.target, &self.type_registry)?;
                let target = self.field_target(root.clone(), &path, span)?;
                self.owner_borrow_target(&target, span)?;
                let dst = self.slot(Type::reference(target, mutable))?;
                let mut src = src.clone();
                src.ty = Type::reference(root.clone(), r.mutable);
                if root.is_inline_owner() {
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
        if mutable && !binding.mutable {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                "Exclusive local borrowing requires a var binding",
            ));
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
            if self
                .result
                .as_ref()
                .is_some_and(|result| result.index == local.index && result.ty.is_owner())
            {
                continue;
            }
            self.emit(Op::EndLocal(local));
        }
    }

    pub(super) fn escape_owner(&mut self, flow: &Flow) {
        if let Flow::Value(Some(slot)) = flow {
            if slot.ty.is_owner() {
                self.local_scopes
                    .last_mut()
                    .unwrap()
                    .retain(|local| local.index != slot.index);
                let parent = self.local_scopes.len() - 2;
                self.local_scopes[parent].push(slot.clone());
            }
        }
    }

    pub(super) fn end_local_scope(&mut self) {
        let locals = self.local_scopes.pop().unwrap();
        for local in locals.into_iter().rev() {
            if self
                .result
                .as_ref()
                .is_some_and(|result| result.index == local.index && result.ty.is_owner())
            {
                continue;
            }
            self.emit(Op::EndLocal(local));
        }
    }

    pub(super) fn erase_local_ends(&mut self) {
        let mut spans = Vec::with_capacity(self.op_spans.len());
        let mut index = 0;
        self.ops.retain(|op| {
            let keep = !matches!(op, Op::EndLocal(_) | Op::Buffer(BufferOp::Access { .. }));
            if keep {
                spans.push(self.op_spans[index]);
            }
            index += 1;
            keep
        });
        self.op_spans = spans;
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
        if let Expr::FieldAssign { value, span, .. } = expr {
            if !binding.mutable {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Cannot assign to an immutable local; declare it with var",
                ));
            }
            if target.is_owner() {
                self.check_owner_projection(slot_type(&binding.slot), &path, *span)?;
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
                self.emit(Op::Copy(dst.clone().unwrap(), value.unwrap()));
                self.emit(Op::StoreOwner {
                    reference: reference.clone(),
                    path,
                    value: dst,
                    target,
                });
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
