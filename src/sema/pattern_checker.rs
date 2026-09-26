use std::collections::{HashMap, HashSet};

use crate::diagnostic::SemanticError;
use crate::syntax::{Expr, MatchArm, Pattern};
use crate::Span;

use super::checker::Checker;
use super::expectation::TypeExpectation;
use super::pattern::{self, CheckedPattern};
use super::symbol_table::EnumVariantInfo;
use super::types::{type_name, Type};

impl Checker {
    pub(super) fn check_match(
        &mut self,
        value: &Expr,
        arms: &[MatchArm],
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        let value_type = self.check_expression(value, TypeExpectation::none())?;
        let pattern_id = match value_type {
            Type::Enum(enum_id) => enum_id,
            Type::Option(option_id) => pattern::option_pattern_id(option_id),
            Type::Result(result_id) => pattern::result_pattern_id(result_id),
            Type::Tuple(_) | Type::Unit => 0,
            _ => {
                return Err(SemanticError::TypeMismatch {
                    expected: "enum, Option, Result, or tuple".to_owned(),
                    actual: type_name(self.types[&value.id]),
                    span: value.span,
                })
            }
        };
        let enum_name = if matches!(value_type, Type::Option(_)) {
            "Option".to_owned()
        } else if matches!(value_type, Type::Result(_)) {
            "Result".to_owned()
        } else if matches!(value_type, Type::Tuple(_) | Type::Unit) {
            type_name(value_type)
        } else {
            self.enums
                .iter()
                .find(|(_, enumeration)| enumeration.id == pattern_id)
                .map(|(name, _)| name.clone())
                .expect("checked enum type")
        };
        let mut matrix: Vec<Vec<CheckedPattern>> = Vec::new();
        let mut result_type = expectation.ty();
        for arm in arms {
            let mut bindings = Vec::new();
            let mut binding_names = HashSet::new();
            let checked =
                self.check_pattern(&arm.pattern, value_type, &mut bindings, &mut binding_names)?;
            if matrix.iter().any(|row| row.first() == Some(&checked)) {
                if let Pattern::EnumVariant { variant, .. } = &arm.pattern {
                    return Err(SemanticError::DuplicateMatchArm {
                        variant: variant.clone(),
                        span: arm.span,
                    });
                }
            }
            if !pattern::is_useful(self, &matrix, std::slice::from_ref(&checked), &[value_type]) {
                return Err(SemanticError::UnreachableMatchArm { span: arm.span });
            }
            self.scopes.push(HashMap::new());
            for (name, ty) in bindings {
                self.bind(name, ty);
            }
            let arm_type = self.check_expression(
                &arm.value,
                result_type
                    .map(TypeExpectation::require)
                    .unwrap_or_else(TypeExpectation::none),
            )?;
            self.scopes.pop();
            if result_type.is_none() && !super::control_flow::does_not_complete(&arm.value) {
                result_type = Some(arm_type);
            }
            matrix.push(vec![checked]);
        }
        if pattern::is_useful(self, &matrix, &[CheckedPattern::Wildcard], &[value_type]) {
            return Err(SemanticError::NonExhaustiveMatch {
                enum_name,
                span: value.span,
            });
        }
        Ok(result_type.unwrap_or(Type::Unit))
    }

    pub(super) fn check_pattern(
        &mut self,
        pattern: &Pattern,
        expected: Type,
        bindings: &mut Vec<(String, Type)>,
        binding_names: &mut HashSet<String>,
    ) -> Result<CheckedPattern, SemanticError> {
        match pattern {
            Pattern::Wildcard { .. } => Ok(CheckedPattern::Wildcard),
            Pattern::Binding { name, span } => {
                if !binding_names.insert(name.clone()) {
                    return Err(SemanticError::DuplicatePatternBinding {
                        name: name.clone(),
                        span: *span,
                    });
                }
                bindings.push((name.clone(), expected));
                Ok(CheckedPattern::Wildcard)
            }
            Pattern::EnumVariant {
                enum_name,
                variant,
                fields,
                span,
                rest,
            } => {
                if let Type::Option(option_id) = expected {
                    let option_alias = enum_name == "Option"
                        || self.lookup(enum_name).is_some_and(|binding| {
                            matches!(binding.type_value, Some(Type::Option(_)))
                        });
                    if !option_alias || !matches!(variant.as_str(), "Some" | "None") {
                        return Err(SemanticError::TypeMismatch {
                            expected: "Option".to_owned(),
                            actual: enum_name.clone(),
                            span: *span,
                        });
                    }
                    let variant_index = usize::from(variant == "None");
                    let field_types = (variant_index == 0)
                        .then_some(self.option_types[option_id])
                        .into_iter()
                        .collect::<Vec<_>>();
                    if fields.len() > field_types.len()
                        || (!rest && fields.len() != field_types.len())
                    {
                        return Err(SemanticError::WrongArgumentCount {
                            function: format!("Option.{variant}"),
                            expected: field_types.len(),
                            span: *span,
                        });
                    }
                    let mut checked_fields = fields
                        .iter()
                        .zip(&field_types)
                        .map(|(field, field_type)| {
                            self.check_pattern(&field.pattern, *field_type, bindings, binding_names)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    checked_fields.extend(std::iter::repeat_n(
                        CheckedPattern::Wildcard,
                        field_types.len() - fields.len(),
                    ));
                    return Ok(CheckedPattern::EnumVariant {
                        enum_id: pattern::option_pattern_id(option_id),
                        variant_index,
                        fields: checked_fields,
                    });
                }
                if let Type::Result(result_id) = expected {
                    let result_alias = enum_name == "Result"
                        || self.lookup(enum_name).is_some_and(|binding| {
                            matches!(binding.type_value, Some(Type::Result(_)))
                        });
                    if !result_alias || !matches!(variant.as_str(), "Ok" | "Err") {
                        return Err(SemanticError::TypeMismatch {
                            expected: "Result".to_owned(),
                            actual: enum_name.clone(),
                            span: *span,
                        });
                    }
                    if fields.len() > 1 || (!rest && fields.len() != 1) {
                        return Err(SemanticError::WrongArgumentCount {
                            function: format!("Result.{variant}"),
                            expected: 1,
                            span: *span,
                        });
                    }
                    let (ok, err) = self.result_types[result_id];
                    let field_type = if variant == "Ok" { ok } else { err };
                    let checked = fields
                        .first()
                        .map(|field| {
                            self.check_pattern(&field.pattern, field_type, bindings, binding_names)
                        })
                        .transpose()?
                        .unwrap_or(CheckedPattern::Wildcard);
                    return Ok(CheckedPattern::EnumVariant {
                        enum_id: pattern::result_pattern_id(result_id),
                        variant_index: usize::from(variant == "Err"),
                        fields: vec![checked],
                    });
                }
                let Type::Enum(expected_id) = expected else {
                    return Err(SemanticError::TypeMismatch {
                        expected: type_name(expected),
                        actual: "enum pattern".to_owned(),
                        span: *span,
                    });
                };
                let enum_id = self
                    .enums
                    .get(enum_name)
                    .map(|enumeration| enumeration.id)
                    .or_else(|| {
                        self.lookup(enum_name)
                            .and_then(|binding| match binding.type_value {
                                Some(Type::Enum(id)) => Some(id),
                                _ => None,
                            })
                    })
                    .ok_or_else(|| SemanticError::UnknownType {
                        name: enum_name.clone(),
                        span: *span,
                    })?;
                let enumeration = self
                    .enums
                    .values()
                    .find(|enumeration| enumeration.id == enum_id)
                    .cloned()
                    .ok_or_else(|| SemanticError::UnknownType {
                        name: enum_name.clone(),
                        span: *span,
                    })?;
                if enumeration.id != expected_id {
                    let expected_name = self
                        .enums
                        .iter()
                        .find(|(_, enumeration)| enumeration.id == expected_id)
                        .map(|(name, _)| name.clone())
                        .expect("checked enum type");
                    return Err(SemanticError::TypeMismatch {
                        expected: expected_name,
                        actual: enum_name.clone(),
                        span: *span,
                    });
                }
                let variant_index = enumeration
                    .variants
                    .iter()
                    .position(|candidate| candidate.name == *variant)
                    .ok_or_else(|| SemanticError::UnknownEnumVariant {
                        enum_name: enum_name.clone(),
                        variant: variant.clone(),
                        span: *span,
                    })?;
                let variant_info = enumeration.variants[variant_index].clone();
                let ordered =
                    self.order_pattern_fields(fields, *rest, &variant_info, enum_name, *span)?;
                let checked_fields = ordered
                    .into_iter()
                    .zip(variant_info.fields)
                    .map(|(field, (_, field_type))| {
                        field
                            .map(|field| {
                                self.check_pattern(
                                    &field.pattern,
                                    field_type,
                                    bindings,
                                    binding_names,
                                )
                            })
                            .transpose()
                            .map(|value| value.unwrap_or(CheckedPattern::Wildcard))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(CheckedPattern::EnumVariant {
                    enum_id: expected_id,
                    variant_index,
                    fields: checked_fields,
                })
            }
            Pattern::Tuple { elements, span } => {
                if elements.is_empty() {
                    if expected != Type::Unit {
                        return Err(SemanticError::TypeMismatch {
                            expected: type_name(expected),
                            actual: "()".to_owned(),
                            span: *span,
                        });
                    }
                    return Ok(CheckedPattern::Wildcard);
                }
                let Type::Tuple(tuple_id) = expected else {
                    return Err(SemanticError::TypeMismatch {
                        expected: type_name(expected),
                        actual: "tuple pattern".to_owned(),
                        span: *span,
                    });
                };
                let field_types = self.tuple_types[tuple_id].clone();
                if elements.len() != field_types.len() {
                    return Err(SemanticError::WrongArgumentCount {
                        function: "tuple".to_owned(),
                        expected: field_types.len(),
                        span: *span,
                    });
                }
                let checked_fields = elements
                    .iter()
                    .zip(field_types)
                    .map(|(element, field_type)| {
                        self.check_pattern(element, field_type, bindings, binding_names)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(CheckedPattern::Tuple {
                    elements: checked_fields,
                })
            }
        }
    }

    fn order_pattern_fields<'a>(
        &self,
        fields: &'a [crate::syntax::PatternField],
        rest: bool,
        variant: &EnumVariantInfo,
        enum_name: &str,
        span: Span,
    ) -> Result<Vec<Option<&'a crate::syntax::PatternField>>, SemanticError> {
        if fields.len() > variant.fields.len() || (!rest && fields.len() != variant.fields.len()) {
            return Err(SemanticError::WrongArgumentCount {
                function: format!("{enum_name}.{}", variant.name),
                expected: variant.fields.len(),
                span,
            });
        }
        let mut ordered = vec![None; variant.fields.len()];
        let mut next_positional = 0;
        for field in fields {
            let shorthand = match &field.pattern {
                Pattern::Binding { name, .. } if field.label.is_none() => Some(name),
                _ => None,
            };
            let index = if let Some(label) = field.label.as_ref().or(shorthand) {
                variant
                    .fields
                    .iter()
                    .position(|(name, _)| name == label)
                    .ok_or_else(|| SemanticError::UnknownValue {
                        name: label.clone(),
                        span: field.span,
                    })?
            } else {
                while next_positional < ordered.len() && ordered[next_positional].is_some() {
                    next_positional += 1;
                }
                let index = next_positional;
                next_positional += 1;
                index
            };
            if index >= ordered.len() || ordered[index].is_some() {
                return Err(SemanticError::DuplicateEnumField {
                    name: field.label.clone().unwrap_or_else(|| {
                        variant.fields[index.min(variant.fields.len() - 1)]
                            .0
                            .clone()
                    }),
                    span: field.span,
                });
            }
            ordered[index] = Some(field);
        }
        Ok(ordered)
    }
}
