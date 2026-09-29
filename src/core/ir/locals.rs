//! Stable function storage and lexical lifetimes for borrowed Copy locals.
use super::*;
use std::collections::HashSet;

// Collect spellings conservatively before planning so a borrow in one branch
// never changes another branch's representation. Shadowed bindings get distinct
// places; matching an unused spelling can only materialize extra local storage.
pub(super) fn borrowed_names(body: &Expr) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut pending = vec![body];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Unary { op, operand, .. } => {
                if matches!(op, UnaryOp::Borrow | UnaryOp::BorrowMut) {
                    if let Expr::Ident { name, .. } = operand.as_ref() {
                        names.insert(name.clone());
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
                pending.push(&call.receiver);
                pending.extend(&call.args);
            }
            Expr::Call { args, .. } => pending.extend(args),
            Expr::Tuple { elements, .. } => pending.extend(elements),
            Expr::Scope { branches, .. } => pending.extend(branches),
            Expr::Block { exprs, .. }
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
        if self.borrowed_names.contains(name)
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
        let Expr::Ident { name, .. } = operand else {
            return Err(type_error(&span, ErrorCode::InvalidAssignment, "Borrowing requires a local binding; field and temporary borrowing are not yet supported"));
        };
        let binding = vars.get(name).ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::UnknownName,
                format!("Undefined local binding '{name}'"),
            )
        })?;
        if mutable && !binding.mutable {
            return Err(type_error(
                &span,
                ErrorCode::InvalidAssignment,
                "Exclusive local borrowing requires a var binding",
            ));
        }
        let src = binding.place.clone().ok_or_else(|| {
            type_error(
                &span,
                ErrorCode::TypeMismatch,
                "Local borrowing requires Copy data without references, String or opaque handles",
            )
        })?;
        let dst = self.slot(Type::reference(slot_type(&binding.slot), mutable))?;
        self.emit(Op::Reference {
            dst: dst.clone(),
            src,
            moving: false,
        });
        Ok(Flow::Value(Some(dst)))
    }

    pub(super) fn end_locals_from(&mut self, depth: usize) {
        let locals: Vec<_> = self.local_scopes[depth..]
            .iter()
            .flatten()
            .cloned()
            .collect();
        for local in locals {
            self.emit(Op::EndLocal(local));
        }
    }

    pub(super) fn end_local_scope(&mut self) {
        let locals = self.local_scopes.pop().unwrap();
        for local in locals {
            self.emit(Op::EndLocal(local));
        }
    }

    pub(super) fn erase_local_ends(&mut self) {
        let mut spans = Vec::with_capacity(self.op_spans.len());
        let mut index = 0;
        self.ops.retain(|op| {
            let keep = !matches!(op, Op::EndLocal(_));
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
        let mut root = match expr {
            Expr::Field { .. } => expr,
            Expr::FieldAssign { target, .. } => target,
            _ => return Ok(None),
        };
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
        let Expr::Ident { name, .. } = root else {
            return Ok(None);
        };
        let Some(binding) = vars.get(name) else {
            return Ok(None);
        };
        let Some(mut reference) = binding.place.clone() else {
            return Ok(None);
        };
        path.reverse();
        let mut target = slot_type(&binding.slot);
        for name in &path {
            let Type::Record(record) = target else {
                return Err(type_error(
                    &expr.span(),
                    ErrorCode::TypeMismatch,
                    "Local field access requires a record",
                ));
            };
            target = record
                .fields
                .iter()
                .find(|f| &f.name == name)
                .map(|f| f.ty.clone())
                .ok_or_else(|| {
                    type_error(
                        &expr.span(),
                        ErrorCode::UnknownName,
                        format!("Unknown field '{name}'"),
                    )
                })?;
        }
        if let Expr::FieldAssign { value, span, .. } = expr {
            if !binding.mutable {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Cannot assign to an immutable local; declare it with var",
                ));
            }
            reference = self.reserve_write(reference)?;
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
