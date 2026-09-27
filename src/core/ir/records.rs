//! Record construction, projection and mutable places, lowered to scalar copies.
use super::*;
use std::collections::HashSet;

impl FunctionPlan {
    pub(super) fn record_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Binding>,
        functions: &HashMap<String, FunctionInfo>,
        suspends: bool,
        types: &TypeRegistry,
    ) -> Result<Flow, Error> {
        match expr {
            Expr::Record { name, fields, span } => {
                let ty = self.nominals.get(name).cloned().ok_or_else(|| type_error(span,
                    ErrorCode::UnknownType, format!("Unknown record constructor `{name}`; aliases do not introduce constructors")))?;
                let Type::Record(record) = &ty else {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        format!("`{name}` is an enum; construct a qualified variant"),
                    ));
                };
                let mut seen = HashSet::new();
                for field in fields {
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
                let result = self.slot(ty.clone())?;
                for (index, field) in fields.iter().enumerate() {
                    let Flow::Value(value) =
                        self.expr(&field.value, vars, functions, suspends, types)?
                    else {
                        if let Some(next) = fields.get(index + 1) {
                            return Err(unreachable_error(&next.value));
                        }
                        return Ok(Flow::Diverged);
                    };
                    let target = field_slot(&result, &field.name, &field.span)?;
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
                Ok(Flow::Value(Some(result)))
            }
            Expr::Field { base, name, span } => {
                let mut root = base.as_ref();
                while let Expr::Field { base, .. } = root {
                    root = base;
                }
                if matches!(root, Expr::Ident { name, .. } if vars.contains_key(name)) {
                    // Snapshot only the selected field, not the entire root record.
                    let (root, value) = place(expr, vars)?;
                    return if vars[root].mutable {
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
                Ok(Flow::Value(field_slot(&base, name, span)?))
            }
            Expr::FieldAssign {
                target,
                value,
                span,
            } => {
                let (root, target) = place(target, vars)?;
                let binding = &vars[root];
                if !binding.mutable {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        format!("Cannot assign to immutable binding `{root}`; declare it with var"),
                    ));
                }
                let Flow::Value(value) = self.expr(value, vars, functions, suspends, types)? else {
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

fn field_slot(base: &Slot, name: &str, span: &Span) -> Result<Option<Slot>, Error> {
    let Type::Record(record) = &base.ty else {
        return Err(type_error(
            span,
            ErrorCode::TypeMismatch,
            format!("Type {} has no fields", base.ty),
        ));
    };
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

fn place<'a>(
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
        Expr::Field { base, name, span } => {
            let (root, slot) = place(base, vars)?;
            let slot = slot.ok_or_else(|| {
                type_error(
                    span,
                    ErrorCode::TypeMismatch,
                    "Cannot access a field of Unit",
                )
            })?;
            Ok((root, field_slot(&slot, name, span)?))
        }
        _ => Err(type_error(
            &expr.span(),
            ErrorCode::InvalidAssignment,
            "Field assignment must be rooted in a binding",
        )),
    }
}
