use super::*;
use crate::hir::{CoreExpr, CoreExprKind};
use crate::syntax::FieldAccess;

impl Lowerer<'_> {
    pub(super) fn lower_constructor_fields<'b>(
        &mut self,
        destination_type: Type,
        arguments: impl IntoIterator<Item = (Option<&'b str>, &'b CoreExpr)>,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Vec<MirValueId>, Diagnostic> {
        let (declared, defaults) = match destination_type {
            Type::Struct(id) => (
                types.struct_fields(id),
                self.struct_defaults.get(id).cloned().unwrap_or_default(),
            ),
            Type::Class(id) => (
                types.class_fields(id),
                self.class_defaults.get(id).cloned().unwrap_or_default(),
            ),
            _ => {
                return Err(Diagnostic::codegen(
                    "MIR constructor has an invalid destination type",
                ))
            }
        };
        let mut ordered = vec![None; declared.len()];
        let mut next_positional = 0;
        for (label, value) in arguments {
            let index = if let Some(label) = label {
                declared
                    .iter()
                    .position(|(name, _)| name == label)
                    .ok_or_else(|| Diagnostic::codegen("constructor field was not resolved"))?
            } else {
                while next_positional < ordered.len() && ordered[next_positional].is_some() {
                    next_positional += 1;
                }
                let index = next_positional;
                next_positional += 1;
                index
            };
            if index >= ordered.len() || ordered[index].is_some() {
                return Err(Diagnostic::codegen("constructor field was not resolved"));
            }
            ordered[index] = Some(value);
        }
        let mut fields = Vec::with_capacity(declared.len());
        for (index, argument) in ordered.into_iter().enumerate() {
            let expression = argument
                .cloned()
                .or_else(|| defaults.get(index).and_then(Option::as_ref).cloned())
                .ok_or_else(|| Diagnostic::codegen("missing constructor argument"))?;
            let value = self
                .lower_value(&expression, function_names, types)?
                .ok_or_else(|| Diagnostic::codegen("constructor field does not produce a value"))?;
            fields.push(value);
        }
        Ok(fields)
    }

    pub(super) fn lower_borrowed_value(
        &mut self,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if let CoreExprKind::Name(name) = &expression.kind {
            if let Some((local, index)) = self.expression_capture(expression, name) {
                return self
                    .read_capture(local, index, expression.ty, true)
                    .map(Some);
            }
        }
        if types.is_owned(expression.ty) {
            if let CoreExprKind::Field { value, access } = &expression.kind {
                if matches!(value.ty, Type::Tuple(_) | Type::Struct(_) | Type::Class(_)) {
                    let Some(base) = self.lower_borrowed_value(value, function_names, types)?
                    else {
                        return Ok(None);
                    };
                    let destination =
                        self.next_value_with_ownership(expression.ty, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::Project {
                        destination,
                        base,
                        access: resolve_field_access(access, value.ty, types)?,
                    });
                    return Ok(Some(destination));
                }
            }
        }
        if types.is_owned(expression.ty) {
            if let CoreExprKind::Name(name) = &expression.kind {
                if let Some(local) = self.resolve_expression_local(expression, name) {
                    let destination =
                        self.next_value_with_ownership(expression.ty, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::BorrowLocal { destination, local });
                    return Ok(Some(destination));
                }
            }
        }
        let value = self.lower_value(expression, function_names, types)?;
        let Some(value) = value else {
            return Ok(None);
        };
        if types.is_owned(expression.ty) && self.value_ownership[value.0] == MirOwnership::Owned {
            let local = self.new_local("<temporary>", expression.ty);
            let bound = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Bind {
                local,
                value: Some(value),
                destination: bound,
            });
            let destination = self.next_value_with_ownership(expression.ty, MirOwnership::Borrowed);
            self.push_statement(MirStatement::BorrowLocal { destination, local });
            return Ok(Some(destination));
        }
        Ok(Some(value))
    }

    pub(super) fn lower_owned_field(
        &mut self,
        value: &CoreExpr,
        access: &FieldAccess,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(base) = self.lower_value(value, function_names, types)? else {
            return Ok(None);
        };
        if self.value_ownership[base.0] != MirOwnership::Owned {
            return Err(Diagnostic::codegen(
                "cannot move a field out of a borrowed aggregate",
            ));
        }
        let MirFieldAccess::Index(selected_index) = resolve_field_access(access, value.ty, types)?;
        let field_types = match value.ty {
            Type::Tuple(id) => types.tuple_elements(id).to_vec(),
            Type::Struct(id) => types
                .struct_fields(id)
                .iter()
                .map(|(_, field_type)| *field_type)
                .collect(),
            _ => {
                return Err(Diagnostic::codegen(
                    "owned field extraction requires a tuple or struct",
                ))
            }
        };

        let mut selected = None;
        for (index, field_type) in field_types.into_iter().enumerate() {
            if !types.needs_drop(field_type) {
                continue;
            }
            // Deinit consumes the entire aggregate, including shared fields
            // whose references must be released when another field is moved.
            let field = self.next_value(field_type);
            self.push_statement(MirStatement::Project {
                destination: field,
                base,
                access: MirFieldAccess::Index(index),
            });
            if index == selected_index {
                selected = Some(field);
            } else {
                self.discard_value(Some(field));
            }
        }
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Deinit {
            destination,
            value: base,
            variant: None,
        });
        selected
            .map(Some)
            .ok_or_else(|| Diagnostic::codegen("selected field is not owned"))
    }
}
