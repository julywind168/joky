use super::*;
use crate::sema::symbol_table::ClassFieldInfo;

impl Checker {
    pub(super) fn check_regular_field(
        &self,
        value_type: Type,
        access: &FieldAccess,
        value: &Expr,
        expression: &Expr,
    ) -> Result<Type, SemanticError> {
        Ok(match (value_type, access) {
            (Type::Tuple(tuple_id), FieldAccess::Index(index)) => self
                .tuple_types
                .get(tuple_id)
                .and_then(|fields| fields.get(*index))
                .copied()
                .ok_or_else(|| SemanticError::UnknownValue {
                    name: format!("tuple field {index}"),
                    span: expression.span,
                })?,
            (Type::Struct(struct_id), FieldAccess::Name(name)) => self
                .struct_field_type(struct_id, name)
                .ok_or_else(|| SemanticError::UnknownStructField {
                    struct_name: self.struct_name(struct_id).to_owned(),
                    field: name.clone(),
                    span: expression.span,
                })?,
            (Type::Class(class_id), FieldAccess::Name(name)) => self
                .class_field(class_id, name)
                .map(|field| field.ty)
                .ok_or_else(|| SemanticError::UnknownClassField {
                    class_name: self.class_name(class_id).to_owned(),
                    field: name.clone(),
                    span: expression.span,
                })?,
            (Type::Tuple(_), FieldAccess::Name(_)) => {
                return Err(SemanticError::TypeMismatch {
                    expected: "tuple field index".to_owned(),
                    actual: "field name".to_owned(),
                    span: expression.span,
                });
            }
            (Type::Struct(_), FieldAccess::Index(_)) => {
                return Err(SemanticError::TypeMismatch {
                    expected: "struct field name".to_owned(),
                    actual: "field index".to_owned(),
                    span: expression.span,
                });
            }
            (Type::Class(_), FieldAccess::Index(_)) => {
                return Err(SemanticError::TypeMismatch {
                    expected: "class field name".to_owned(),
                    actual: "field index".to_owned(),
                    span: expression.span,
                });
            }
            (Type::Enum(_), _) => {
                return Err(SemanticError::TypeMismatch {
                    expected: "match expression".to_owned(),
                    actual: "enum field access".to_owned(),
                    span: expression.span,
                });
            }
            (actual, _) => {
                return Err(SemanticError::TypeMismatch {
                    expected: "tuple or struct".to_owned(),
                    actual: type_name(actual),
                    span: value.span,
                });
            }
        })
    }

    pub(super) fn check_struct_init(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        expression: &Expr,
    ) -> Result<Type, SemanticError> {
        let definition =
            self.structs
                .get(name)
                .cloned()
                .ok_or_else(|| SemanticError::UnknownType {
                    name: name.to_owned(),
                    span: expression.span,
                })?;
        for (field_name, value) in fields {
            let field_type = definition
                .fields
                .iter()
                .find(|(name, _)| name == field_name)
                .map(|(_, ty)| *ty)
                .ok_or_else(|| SemanticError::UnknownStructField {
                    struct_name: name.to_owned(),
                    field: field_name.clone(),
                    span: value.span,
                })?;
            if fields
                .iter()
                .filter(|(candidate, _)| candidate == field_name)
                .count()
                > 1
            {
                return Err(SemanticError::DuplicateStructField {
                    name: field_name.clone(),
                    span: value.span,
                });
            }
            self.check_expression(value, TypeExpectation::require(field_type))?;
        }
        for (field_index, (field_name, _)) in definition.fields.iter().enumerate() {
            if !fields.iter().any(|(candidate, _)| candidate == field_name)
                && definition.defaults[field_index].is_none()
            {
                return Err(SemanticError::MissingStructField {
                    struct_name: name.to_owned(),
                    field: field_name.clone(),
                    span: expression.span,
                });
            }
        }
        Ok(Type::Struct(definition.id))
    }

    pub(super) fn struct_field_type(&self, id: usize, name: &str) -> Option<Type> {
        self.structs
            .values()
            .find(|definition| definition.id == id)
            .and_then(|definition| {
                definition
                    .fields
                    .iter()
                    .find(|(field_name, _)| field_name == name)
                    .map(|(_, ty)| *ty)
            })
    }

    pub(super) fn struct_name(&self, id: usize) -> &str {
        self.structs
            .iter()
            .find(|(_, definition)| definition.id == id)
            .map(|(name, _)| name.as_str())
            .expect("each struct type has a definition")
    }

    pub(super) fn check_class_init(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        expression: &Expr,
    ) -> Result<Type, SemanticError> {
        let definition =
            self.classes
                .get(name)
                .cloned()
                .ok_or_else(|| SemanticError::UnknownType {
                    name: name.to_owned(),
                    span: expression.span,
                })?;
        for (field_name, value) in fields {
            let field = definition
                .fields
                .iter()
                .find(|(name, _, _)| name == field_name)
                .map(|(_, field, _)| field)
                .ok_or_else(|| SemanticError::UnknownClassField {
                    class_name: name.to_owned(),
                    field: field_name.clone(),
                    span: value.span,
                })?;
            if fields
                .iter()
                .filter(|(candidate, _)| candidate == field_name)
                .count()
                > 1
            {
                return Err(SemanticError::DuplicateClassField {
                    name: field_name.clone(),
                    span: value.span,
                });
            }
            self.check_expression(value, TypeExpectation::require(field.ty))?;
        }
        for (field_name, _, default) in &definition.fields {
            if !fields.iter().any(|(candidate, _)| candidate == field_name) && default.is_none() {
                return Err(SemanticError::MissingClassField {
                    class_name: name.to_owned(),
                    field: field_name.clone(),
                    span: expression.span,
                });
            }
        }
        Ok(Type::Class(definition.id))
    }

    pub(super) fn class_field(&self, id: usize, name: &str) -> Option<&ClassFieldInfo> {
        self.classes
            .values()
            .find(|definition| definition.id == id)
            .and_then(|definition| {
                definition
                    .fields
                    .iter()
                    .find(|(field_name, _, _)| field_name == name)
                    .map(|(_, field, _)| field)
            })
    }

    pub(super) fn class_name(&self, id: usize) -> &str {
        self.classes
            .iter()
            .find(|(_, definition)| definition.id == id)
            .map(|(name, _)| name.as_str())
            .expect("each class type has a definition")
    }

    pub(super) fn implicit_self_field_type(&self, name: &str) -> Option<Type> {
        match self.lookup("self")?.value_type {
            Type::Struct(id) => self.struct_field_type(id, name),
            Type::Class(id) => self.class_field(id, name).map(|field| field.ty),
            _ => None,
        }
    }

    pub(super) fn check_assignment(
        &mut self,
        target: &Expr,
        value: &Expr,
    ) -> Result<Type, SemanticError> {
        let place = self.mutable_place(target)?;
        self.check_expression(value, TypeExpectation::require(place))?;
        if matches!(target.kind, ExprKind::Name(_)) {
            self.record_drop_effects(place, target.span)?;
        }
        Ok(Type::Unit)
    }

    pub(super) fn check_compound_assignment(
        &mut self,
        target: &Expr,
        operator: crate::syntax::BinaryOp,
        value: &Expr,
    ) -> Result<Type, SemanticError> {
        let place = self.mutable_place(target)?;
        if let crate::syntax::BinaryOp::Arithmetic(_, mode) = operator {
            if mode == crate::syntax::ArithmeticMode::Checked {
                return Err(SemanticError::CheckedCompoundAssignment {
                    span: target.span.merge(value.span),
                });
            }
            if !place.is_integer() {
                return Err(SemanticError::ArithmeticModeRequiresInteger {
                    span: target.span.merge(value.span),
                });
            }
        }
        let allowed = if operator.is_bitwise() {
            place.is_integer()
        } else {
            place.is_numeric()
        };
        if !allowed {
            return Err(if operator.is_bitwise() {
                SemanticError::BitwiseOperatorRequiresInteger {
                    span: target.span.merge(value.span),
                }
            } else {
                SemanticError::NumericOperatorRequiresNumeric {
                    span: target.span.merge(value.span),
                }
            });
        }
        if matches!(
            operator.arithmetic(),
            Some((
                crate::syntax::ArithmeticOp::Divide | crate::syntax::ArithmeticOp::Remainder,
                _
            ))
        ) && place.is_integer()
            && crate::sema::validation::constant_integer(value) == Some(0)
        {
            return Err(SemanticError::DivisionByZero { span: value.span });
        }
        self.check_expression(value, TypeExpectation::require(place))?;
        if matches!(target.kind, ExprKind::Name(_)) {
            self.record_drop_effects(place, target.span)?;
        }
        Ok(Type::Unit)
    }

    fn mutable_place(&mut self, target: &Expr) -> Result<Type, SemanticError> {
        if let ExprKind::Name(name) = &target.kind {
            let binding = self
                .lookup(name)
                .ok_or_else(|| SemanticError::UnknownValue {
                    name: name.clone(),
                    span: target.span,
                })?;
            if !binding.mutable || binding.type_value.is_some() {
                return Err(SemanticError::ImmutableBinding {
                    name: name.clone(),
                    span: target.span,
                });
            }
            if let Some(declaration) = binding.declaration {
                self.local_bindings.insert(target.id, declaration);
            }
            self.types.insert(target.id, binding.value_type);
            return Ok(binding.value_type);
        }
        let ExprKind::Field {
            value: receiver,
            access: FieldAccess::Name(field_name),
        } = &target.kind
        else {
            return Err(SemanticError::InvalidAssignmentTarget { span: target.span });
        };
        let ExprKind::Name(name) = &receiver.kind else {
            return Err(SemanticError::InvalidAssignmentTarget { span: target.span });
        };
        if name != "self" {
            return Err(SemanticError::InvalidAssignmentTarget { span: target.span });
        }
        let Type::Class(class_id) = self.check_expression(receiver, TypeExpectation::none())?
        else {
            return Err(SemanticError::InvalidAssignmentTarget { span: target.span });
        };
        let field = self
            .class_field(class_id, field_name)
            .cloned()
            .ok_or_else(|| SemanticError::UnknownClassField {
                class_name: self.class_name(class_id).to_owned(),
                field: field_name.clone(),
                span: target.span,
            })?;
        if !field.mutable {
            return Err(SemanticError::ImmutableClassField {
                name: field_name.clone(),
                span: target.span,
            });
        }
        self.types.insert(target.id, field.ty);
        Ok(field.ty)
    }
}
