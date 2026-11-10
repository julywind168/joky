//! Record construction, projection and mutable places, lowered to scalar copies.
use super::*;
use std::collections::HashSet;

impl FunctionPlan {
    /// Whether a field name is accessible from this plan's definition scope.
    pub(super) fn field_visible(
        &self,
        record: &crate::core::types::RecordType,
        name: &str,
    ) -> bool {
        record
            .fields
            .iter()
            .find(|f| f.name == name)
            .is_none_or(|f| !f.private)
            || self.definition_scope.as_ref().map_or_else(
                || {
                    self.generics
                        .borrow()
                        .members
                        .local_records
                        .contains(&record.id)
                },
                |scope| scope.records.contains(&record.id),
            )
    }

    pub(super) fn check_field(
        &self,
        record: &crate::core::types::RecordType,
        name: &str,
        span: &Span,
    ) -> Result<(), Error> {
        if !self.field_visible(record, name) {
            return Err(type_error(
                span,
                ErrorCode::UnknownName,
                format!("Field '{}.{name}' is private", record.name),
            ));
        }
        Ok(())
    }

    pub(super) fn field_target(
        &self,
        mut target: Type,
        path: &[String],
        span: Span,
    ) -> Result<Type, Error> {
        for name in path {
            target = self.field_member(&target, name, &span)?.1;
        }
        Ok(target)
    }

    pub(super) fn field_member(
        &self,
        target: &Type,
        name: &str,
        span: &Span,
    ) -> Result<(usize, Type), Error> {
        if let Type::Record(record) = target {
            self.check_field(record, name, span)?;
        }
        target
            .inline_field(name)
            .map(|(offset, ty)| (offset, ty.clone()))
            .ok_or_else(|| {
                type_error(
                    span,
                    if matches!(target, Type::Record(_) | Type::Tuple(_)) {
                        ErrorCode::UnknownName
                    } else {
                        ErrorCode::TypeMismatch
                    },
                    match target {
                        Type::Tuple(_) if name.parse::<u64>().is_err() => {
                            format!("Tuple member `{name}` requires a decimal index")
                        }
                        Type::Tuple(tuple) => format!(
                            "Tuple member index `{name}` is out of range for {} elements",
                            tuple.elements.len()
                        ),
                        Type::Record(record) => {
                            format!("Unknown field `{name}` in record `{}`", record.name)
                        }
                        _ => format!("Type {target} has no fields"),
                    },
                )
            })
    }

    pub(super) fn record_type(
        &self,
        expr: &Expr,
        vars: &HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        types: &TypeRegistry,
        context: generics::ConstructorContext<'_>,
    ) -> Result<Type, Error> {
        let Expr::Record {
            name,
            type_args,
            fields,
            base,
            span,
        } = expr
        else {
            unreachable!()
        };
        let Some(ty) = self.named_type(name) else {
            return Err(type_error(
                span,
                ErrorCode::UnknownType,
                format!(
                    "Unknown record constructor `{name}`; aliases do not introduce constructors"
                ),
            ));
        };
        let Type::Record(record) = ty else {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!("`{name}` is an enum; construct a qualified variant"),
            ));
        };
        let mut seen = HashSet::new();
        for field in fields {
            self.check_field(record, &field.name, &field.span)?;
            if !seen.insert(&field.name) {
                return Err(type_error(
                    &field.span,
                    ErrorCode::DuplicateDefinition,
                    format!("Duplicate initializer for field `{}`", field.name),
                ));
            }
            if !record.fields.iter().any(|f| f.name == field.name) {
                return Err(type_error(
                    &field.span,
                    ErrorCode::UnknownName,
                    format!("Record `{name}` has no field `{}`", field.name),
                ));
            }
        }
        for field in &record.fields {
            if base.is_some() {
                self.check_field(record, &field.name, span)?;
            } else if !seen.contains(&field.name) {
                return Err(type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    format!("Missing field `{}` in record `{name}`", field.name),
                ));
            }
        }
        if record.parameters.is_empty() {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidCall,
                    "Non-generic record constructors do not accept type arguments",
                ));
            }
            return Ok(Type::Record(record.clone()));
        }
        if !type_args.is_empty() && type_args.len() != record.parameters.len() {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                format!(
                    "Type argument count mismatch for record `{name}`: expected {}",
                    record.parameters.len()
                ),
            ));
        }
        let mut inference = generics::seed_inference(&record.parameters, type_args, *span)?;
        if let Some(Type::Record(target)) = context.expected {
            if target.id == record.id {
                let constants = inference.substitutions.constants.clone();
                generics::unify(
                    &Type::Record(record.clone()),
                    &Type::Record(target.clone()),
                    &record.parameters,
                    &mut inference.substitutions,
                    *span,
                )?;
                // A result expectation may refine type parameters, but cannot guess lengths.
                inference.substitutions.constants = constants;
            }
        }
        // Resolve field types once, preserving initializer order for every pass.
        let inputs: Vec<_> = fields
            .iter()
            .map(|field| {
                let formal = &record
                    .fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .unwrap()
                    .ty;
                (&field.value, formal)
            })
            .collect();
        generics::inference_passes(&mut inference, context.default, |inference| {
            let mut local = vars.clone();
            for &(value, formal) in &inputs {
                self.infer_generic_argument(formal, value, &local, functions, types, inference)?;
                self.hint_bindings(value, &mut local, functions, types);
            }
            if let Some(base) = base {
                self.infer_generic_argument(
                    &Type::Record(record.clone()),
                    base,
                    &local,
                    functions,
                    types,
                    inference,
                )?;
            }
            Ok(())
        })?;
        self.materialize_inference(&mut inference, types, *span)?;
        if context.default
            && record
                .parameters
                .iter()
                .any(|p| inference.substitutions.argument(p).is_none())
        {
            self.diagnose_argument_hints(
                inputs.iter().copied(),
                &record.parameters,
                &inference.substitutions,
                vars,
                functions,
                types,
            )?;
        }
        let arguments = record.parameters.iter().map(|p| inference.substitutions.argument(p)
            .ok_or_else(|| type_error(span, ErrorCode::TypeMismatch,
                format!("Cannot infer type parameter `{}` for `{name}`; add a type annotation or explicit type arguments", p.name))))
            .collect::<Result<Vec<_>, _>>()?;
        let ty = types
            .instantiate_record(record, arguments)
            .map_err(|e| e.with_span(*span))?;
        validate_nominal_type(&ty, &self.nominals, span)?;
        Ok(ty)
    }

    pub(super) fn record_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
        expected: Option<&Type>,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::Record {
                fields, base, span, ..
            } => {
                let ty = self.record_type(
                    expr,
                    vars,
                    functions,
                    types,
                    generics::ConstructorContext {
                        expected,
                        default: true,
                    },
                )?;
                if base.is_some() && !ty.is_copy_value() && !ty.is_owner() {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "Record update requires a Copy or owning struct",
                    ));
                }

                let result = self.slot(ty.clone())?;
                if ty.is_place() {
                    self.emit(Op::Place(PlaceOp::Allocate(result.clone())));
                }
                for (index, field) in fields.iter().enumerate() {
                    let target = if ty.is_place() {
                        let (_, ty) = self.field_member(&ty, &field.name, &field.span)?;
                        (ty != Type::Unit).then_some(Slot { index: 0, ty })
                    } else {
                        field_slot(self, &result, &field.name, &field.span)?
                    };
                    let Flow::Value(value) = self.expr_expected(
                        &field.value,
                        vars,
                        functions,
                        suspends,
                        types,
                        Some(&slot_type(&target)),
                    )?
                    else {
                        if let Some(next) = fields.get(index + 1) {
                            return Err(unreachable_error(&next.value));
                        }
                        if let Some(base) = base {
                            return Err(unreachable_error(base));
                        }
                        return Ok(Flow::Diverged);
                    };
                    if slot_type(&target) != slot_type(&value) {
                        return Err(type_error(
                            &field.value.span(),
                            ErrorCode::TypeMismatch,
                            format!(
                                "Field `{}` expects {}, got {}",
                                field.name,
                                slot_type(&target),
                                slot_type(&value)
                            ),
                        ));
                    }
                    if ty.is_place() {
                        let offset = ty
                            .storage_field_path(std::slice::from_ref(&field.name))
                            .unwrap()
                            .0;
                        self.place_write(&result, offset, value);
                    } else if let (Some(dst), Some(src)) = (target, value) {
                        self.emit(Op::Copy(dst, src));
                    }
                }
                if let Some(base) = base {
                    let Flow::Value(value) =
                        self.expr_expected(base, vars, functions, suspends, types, Some(&ty))?
                    else {
                        return Ok(Flow::Diverged);
                    };
                    if slot_type(&value) != ty {
                        return Err(type_error(
                            &base.span(),
                            ErrorCode::TypeMismatch,
                            format!("Record update base expects {ty}, got {}", slot_type(&value)),
                        ));
                    }
                    let value = value.expect("record value");
                    let Type::Record(record) = &ty else {
                        unreachable!()
                    };
                    for field in &record.fields {
                        if fields.iter().any(|f| f.name == field.name) {
                            continue;
                        }
                        if ty.is_place() {
                            let (offset, field_ty) = ty
                                .storage_field_path(std::slice::from_ref(&field.name))
                                .unwrap();
                            let field_value = self.place_read(&value, offset, field_ty)?;
                            self.place_write(&result, offset, field_value);
                        } else if let (Some(dst), Some(src)) = (
                            field_slot(self, &result, &field.name, span)?,
                            field_slot(self, &value, &field.name, span)?,
                        ) {
                            if src.ty.is_owner() {
                                // Detach to its own cleanup item before releasing the
                                // overwritten remainder, just like record destructuring.
                                let detached = self.slot(src.ty.clone())?;
                                self.emit(Op::Buffer(BufferOp::Detach {
                                    dst: detached.clone(),
                                    src,
                                    owner: value.clone(),
                                }));
                                self.emit(Op::Copy(dst, detached));
                            } else {
                                self.emit(Op::Copy(dst, src));
                            }
                        }
                    }
                    if ty.is_owner() {
                        self.emit(Op::Buffer(BufferOp::End(value)));
                    }
                }

                if result.ty.is_owner() {
                    self.register_owner(result.clone(), None);
                }
                Ok(Flow::Value(Some(result)))
            }
            Expr::Field {
                base, name, span, ..
            } => {
                let mut root = base.as_ref();
                while let Expr::Field { base, .. } = root {
                    root = base;
                }
                if matches!(root, Expr::Ident { name, .. } if vars.contains_key(name)) {
                    // Snapshot only the selected field, not the entire root record.
                    let (root, value) = place(self, expr, vars)?;
                    return if vars[root].mutable || slot_type(&value).is_owner() {
                        self.snapshot(&value).map(Flow::Value)
                    } else {
                        Ok(Flow::Value(value))
                    };
                }
                let Flow::Value(value) = self.expr(base, vars, functions, suspends, types)? else {
                    return Ok(Flow::Diverged);
                };
                let base = value.ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        "Cannot access a field of Unit",
                    )
                })?;
                if base.ty.is_place() {
                    self.field_member(&base.ty, name, span)?;
                    let (offset, ty) = base
                        .ty
                        .storage_field_path(std::slice::from_ref(name))
                        .unwrap();
                    return self.place_read(&base, offset, ty).map(Flow::Value);
                }
                let field = field_slot(self, &base, name, span)?;
                if let Some(field) = field.as_ref().filter(|field| field.ty.is_owner()) {
                    let dst = self.slot(field.ty.clone())?;
                    self.emit(Op::Buffer(BufferOp::Extract {
                        dst: dst.clone(),
                        src: field.clone(),
                        owner: base,
                    }));
                    return Ok(Flow::Value(Some(dst)));
                }
                Ok(Flow::Value(field))
            }
            Expr::FieldAssign {
                op,
                target,
                value,
                span,
            } => {
                let (root, target) = place(self, target, vars)?;
                let binding = &vars[root];
                if !binding.mutable {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        self.immutable_binding_message(root, binding),
                    ));
                }
                let owner = binding
                    .slot
                    .as_ref()
                    .filter(|slot| slot.ty.is_owner())
                    .cloned();
                if let Some(owner) = &owner {
                    self.access_owner(owner, true);
                }
                let old =
                    self.compound_snapshot(op, slot_type(&target), target.clone(), None, span)?;
                let Flow::Value(value) = self.expr_expected(
                    value,
                    vars,
                    functions,
                    suspends,
                    types,
                    Some(&slot_type(&target)),
                )?
                else {
                    return Ok(Flow::Diverged);
                };
                let value = self.compound_value(op, old, value, span)?;
                if slot_type(&target) != slot_type(&value) {
                    return Err(type_error(
                        span,
                        ErrorCode::TypeMismatch,
                        format!(
                            "Field assignment expects {}, got {}",
                            slot_type(&target),
                            slot_type(&value)
                        ),
                    ));
                }
                if let Some(owner) = &owner {
                    self.access_owner(owner, true);
                }
                if let (Some(dst), Some(src)) = (target, value) {
                    self.emit(Op::Copy(dst, src));
                }
                Ok(Flow::Value(None))
            }
            _ => unreachable!(),
        }
    }
}

fn field_slot(
    plan: &FunctionPlan,
    base: &Slot,
    name: &str,
    span: &Span,
) -> Result<Option<Slot>, Error> {
    let (offset, ty) = plan.field_member(&base.ty, name, span)?;
    Ok((ty != Type::Unit).then(|| Slot {
        index: base.index + offset,
        ty,
    }))
}

pub(super) fn place<'a>(
    plan: &FunctionPlan,
    expr: &'a Expr,
    vars: &HashMap<String, Binding>,
) -> Result<(&'a str, Option<Slot>), Error> {
    match expr {
        Expr::Ident { name, span } => vars
            .get(name)
            .map(|binding| (name.as_str(), binding.slot.clone()))
            .ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::UnknownName,
                    format!("Undefined variable `{name}`"),
                )
            }),
        Expr::Field {
            base,
            name,
            span,
            type_args,
        } => {
            if !type_args.is_empty() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Field assignment does not accept type arguments",
                ));
            }
            let (root, slot) = place(plan, base, vars)?;
            let slot = slot.ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Cannot access a field of Unit",
                )
            })?;
            Ok((root, field_slot(plan, &slot, name, span)?))
        }
        _ => Err(type_error(
            &expr.span(),
            ErrorCode::InvalidAssignment,
            "Field assignment must be rooted in a binding",
        )),
    }
}
