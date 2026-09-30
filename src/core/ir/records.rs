//! Record construction, projection and mutable places, lowered to scalar copies.
use super::*;
use std::collections::HashSet;

impl FunctionPlan {
    pub(super) fn check_field(
        &self,
        record: &crate::parser::ast::RecordType,
        name: &str,
        span: &Span,
    ) -> Result<(), Error> {
        if record.fields.iter().any(|f| f.name == name && f.private)
            && !self.definition_scope.as_ref().map_or_else(
                || {
                    self.generics
                        .borrow()
                        .members
                        .local_records
                        .contains(&record.id)
                },
                |scope| scope.records.contains(&record.id),
            )
        {
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
            let Type::Record(record) = target else {
                return Err(type_error(
                    &span,
                    ErrorCode::TypeMismatch,
                    "Field access requires an inline record",
                ));
            };
            self.check_field(&record, name, &span)?;
            target = record
                .fields
                .iter()
                .find(|f| f.name == *name)
                .map(|f| f.ty.clone())
                .ok_or_else(|| {
                    type_error(
                        &span,
                        ErrorCode::UnknownName,
                        format!("Unknown field '{name}'"),
                    )
                })?;
        }
        Ok(target)
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
            if !seen.contains(&field.name) {
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
        let mut inference = generics::seed_inference(&record.parameters, type_args);
        if let Some(Type::Record(target)) = context.expected {
            if target.id == record.id {
                generics::unify(
                    &Type::Record(record.clone()),
                    &Type::Record(target.clone()),
                    &record.parameters,
                    &mut inference.substitutions,
                    *span,
                )?;
            }
        }
        generics::inference_passes(&mut inference, context.default, |inference| {
            let mut local = vars.clone();
            for field in fields {
                let formal = &record
                    .fields
                    .iter()
                    .find(|f| f.name == field.name)
                    .unwrap()
                    .ty;
                self.infer_generic_argument(
                    formal,
                    &field.value,
                    &local,
                    functions,
                    types,
                    inference,
                )?;
                self.hint_bindings(&field.value, &mut local, functions, types);
            }
            Ok(())
        })?;
        self.materialize_inference(&mut inference, types, *span)?;
        let arguments = record.parameters.iter().map(|p| inference.substitutions.get(&p.id).cloned()
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
            Expr::Record { fields, .. } => {
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
                let result = self.slot(ty.clone())?;
                for (index, field) in fields.iter().enumerate() {
                    let target = field_slot(self, &result, &field.name, &field.span)?;
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
                    if let (Some(dst), Some(src)) = (target, value) {
                        self.emit(Op::Copy(dst, src));
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
                    if slot_type(&value).is_owner() {
                        let mut path = Vec::new();
                        let mut field = expr;
                        while let Expr::Field { base, name, .. } = field {
                            path.push(name.clone());
                            field = base;
                        }
                        path.reverse();
                        self.check_owner_projection(slot_type(&vars[root].slot), &path, *span)?;
                    }
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
                let field = field_slot(self, &base, name, span)?;
                if slot_type(&field).is_owner() {
                    self.check_owner_projection(
                        base.ty.clone(),
                        std::slice::from_ref(name),
                        *span,
                    )?;
                }
                Ok(Flow::Value(field))
            }
            Expr::FieldAssign {
                target,
                value,
                span,
            } => {
                let mut path = Vec::new();
                let mut field = target.as_ref();
                while let Expr::Field { base, name, .. } = field {
                    path.push(name.clone());
                    field = base;
                }
                path.reverse();
                let (root, target) = place(self, target, vars)?;
                let binding = &vars[root];
                if slot_type(&target).is_owner() {
                    self.check_owner_projection(slot_type(&binding.slot), &path, *span)?;
                }
                if !binding.mutable {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        format!("Cannot assign to immutable binding `{root}`; declare it with var"),
                    ));
                }
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
    let Type::Record(record) = &base.ty else {
        return Err(type_error(
            span,
            ErrorCode::TypeMismatch,
            format!("Type {} has no fields", base.ty),
        ));
    };
    plan.check_field(record, name, span)?;
    let field = record
        .fields
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| {
            type_error(
                span,
                ErrorCode::UnknownName,
                format!("Record `{}` has no field `{name}`", record.name),
            )
        })?;
    Ok((field.ty != Type::Unit).then(|| Slot {
        index: base.index + field.offset,
        ty: field.ty.clone(),
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
