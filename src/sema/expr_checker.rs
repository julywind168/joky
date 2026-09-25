use crate::diagnostic::SemanticError;
use crate::syntax::{
    CollectionLiteral, Expr, ExprKind, FieldAccess, Pattern, TypeAnnotation, UnaryOp,
};
use crate::Span;

use super::checker::{intern_list, intern_map, intern_result, intern_tuple, Checker};
use super::expectation::TypeExpectation;
use super::types::{type_name, Type};
use super::validation::{check_float, check_positive_integer, integer_suffix_type, type_mismatch};

mod aggregates;
mod closures;
mod effects;
mod operators;

fn pattern_has_enum(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::EnumVariant { .. } => true,
        Pattern::Tuple { elements, .. } => elements.iter().any(pattern_has_enum),
        Pattern::Binding { .. } | Pattern::Wildcard { .. } => false,
    }
}

impl Checker {
    /// Checks the type of an expression
    ///
    /// # Arguments
    /// - `expression`: the expression to check
    /// - `expectation`: the type inference expectation
    ///
    /// # Returns
    /// The actual type of the expression
    pub(super) fn check_expression(
        &mut self,
        expression: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        if self.expr_depth >= crate::syntax::MAX_EXPRESSION_NESTING {
            return Err(SemanticError::ExpressionTooDeep {
                limit: crate::syntax::MAX_EXPRESSION_NESTING,
                span: expression.span,
            });
        }
        self.expr_depth += 1;
        let result = self.check_expression_inner(expression, expectation);
        self.expr_depth -= 1;
        result
    }

    fn check_expression_inner(
        &mut self,
        expression: &Expr,
        expectation: TypeExpectation,
    ) -> Result<Type, SemanticError> {
        if let Some(value) = self.imported_constants.get(&expression.id) {
            if expectation.is_required() {
                if let Some(expected) = expectation.ty().filter(|ty| *ty != value.ty) {
                    return Err(type_mismatch(expected, value.ty, expression.span));
                }
            }
            self.types.insert(expression.id, value.ty);
            return Ok(value.ty);
        }
        let actual = match &expression.kind {
            ExprKind::Integer(value) => {
                // For integer literals, use the expected type if there is one and it is an integer type
                // Otherwise default to Int32
                let value_type = expectation
                    .ty()
                    .filter(|ty| ty.is_integer())
                    .unwrap_or(Type::I32);
                check_positive_integer(*value, value_type, expression.span)?;
                value_type
            }
            ExprKind::TypedInteger(value, suffix) => {
                let value_type = integer_suffix_type(*suffix);
                check_positive_integer(*value, value_type, expression.span)?;
                value_type
            }
            ExprKind::Float(value) => {
                // For float literals, use the expected type if there is one and it is a float type
                // Otherwise default to Float64
                let value_type = expectation
                    .ty()
                    .filter(|ty| ty.is_float())
                    .unwrap_or(Type::F64);
                check_float(*value, value_type, expression.span)?;
                value_type
            }
            ExprKind::Duration(_) => Type::Duration,
            ExprKind::String(_) => Type::String,
            ExprKind::Bytes(_) => Type::Bytes,
            ExprKind::InterpolatedString(parts) => {
                for (_, expression) in parts {
                    if let Some(expression) = expression {
                        let ty = self.check_expression(expression, TypeExpectation::none())?;
                        if !self.implements_show(ty) {
                            return Err(SemanticError::TraitBoundNotSatisfied {
                                trait_name: "Show".to_owned(),
                                actual: type_name(ty),
                                span: expression.span,
                            });
                        }
                    }
                }
                Type::String
            }
            ExprKind::Boolean(_) => Type::Bool,
            ExprKind::AnonymousStruct { .. } => {
                return Err(SemanticError::FunctionNotSupported {
                    name: "anonymous struct types are only valid in type functions".to_owned(),
                    span: expression.span,
                });
            }
            ExprKind::AnonymousEnum { .. } => {
                return Err(SemanticError::FunctionNotSupported {
                    name: "anonymous enum types are only valid in type functions".to_owned(),
                    span: expression.span,
                });
            }
            ExprKind::Name(name) if name == "None" => expectation
                .ty()
                .filter(|ty| matches!(ty, Type::Option(_)))
                .ok_or_else(|| SemanticError::UnknownValue {
                    name: "None requires an Option(T) type context".to_owned(),
                    span: expression.span,
                })?,
            ExprKind::Field { .. } => {
                if let Some(value_type) = self.try_resolve_import_value(expression)? {
                    if expectation.is_required() {
                        if let Some(expected) = expectation.ty().filter(|ty| *ty != value_type) {
                            return Err(type_mismatch(expected, value_type, expression.span));
                        }
                    }
                    self.types.insert(expression.id, value_type);
                    return Ok(value_type);
                }
                let ExprKind::Field { value, access } = &expression.kind else {
                    unreachable!();
                };
                if let FieldAccess::Name(variant_name) = access {
                    if let Some(enum_type) = self.resolve_builtin_enum_type_value(value)? {
                        let field_count = match enum_type {
                            Type::Option(_) => match variant_name.as_str() {
                                "Some" => 1,
                                "None" => 0,
                                _ => {
                                    return Err(SemanticError::UnknownEnumVariant {
                                        enum_name: "Option".to_owned(),
                                        variant: variant_name.clone(),
                                        span: expression.span,
                                    })
                                }
                            },
                            Type::Result(_) => match variant_name.as_str() {
                                "Ok" | "Err" => 1,
                                _ => {
                                    return Err(SemanticError::UnknownEnumVariant {
                                        enum_name: "Result".to_owned(),
                                        variant: variant_name.clone(),
                                        span: expression.span,
                                    })
                                }
                            },
                            Type::Enum(enum_id) => {
                                let enumeration = self
                                    .enums
                                    .values()
                                    .find(|enumeration| enumeration.id == enum_id)
                                    .expect("resolved enum type");
                                let variant = enumeration
                                    .variants
                                    .iter()
                                    .find(|variant| variant.name == *variant_name)
                                    .ok_or_else(|| SemanticError::UnknownEnumVariant {
                                        enum_name: format!("enum_{enum_id}"),
                                        variant: variant_name.clone(),
                                        span: expression.span,
                                    })?;
                                variant.fields.len()
                            }
                            _ => unreachable!("resolved enum type"),
                        };
                        if field_count != 0 {
                            return Err(SemanticError::WrongArgumentCount {
                                function: format!("{}.{}", type_name(enum_type), variant_name),
                                expected: field_count,
                                span: expression.span,
                            });
                        }
                        enum_type
                    } else {
                        let value_type = self.check_expression(value, TypeExpectation::none())?;
                        self.check_regular_field(value_type, access, value, expression)?
                    }
                } else {
                    let value_type = self.check_expression(value, TypeExpectation::none())?;
                    self.check_regular_field(value_type, access, value, expression)?
                }
            }
            ExprKind::Name(name) => {
                if let Some(binding) = self.lookup(name) {
                    if let Some(declaration) = binding.declaration {
                        self.local_bindings.insert(expression.id, declaration);
                    }
                    if binding.type_value.is_some() {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "compile-time types cannot be used as runtime values".to_owned(),
                            span: expression.span,
                        });
                    }
                    binding.value_type
                } else if let Some(field_type) = self.implicit_self_field_type(name) {
                    field_type
                } else if let Some(constant_name) = self.resolve_constant_name(name) {
                    let constant_type = self.check_constant(&constant_name)?;
                    self.constant_references
                        .insert(expression.id, constant_name);
                    constant_type
                } else {
                    return Err(SemanticError::UnknownValue {
                        name: name.clone(),
                        span: expression.span,
                    });
                }
            }
            ExprKind::Let {
                name,
                annotation,
                value,
                mutable,
            } => {
                if annotation.as_ref().is_none_or(|ty| ty.is_name("type")) {
                    if let Some(type_value) = self.resolve_type_value_expr(value)? {
                        if *mutable {
                            return Err(SemanticError::MutableTypeBinding {
                                name: name.clone(),
                                span: expression.span,
                            });
                        }
                        self.bind_type(name.clone(), type_value);
                        self.compile_time_bindings.insert(expression.id);
                        self.types.insert(expression.id, Type::Unit);
                        if expectation.is_required() && expectation.ty() != Some(Type::Unit) {
                            return Err(type_mismatch(
                                expectation.ty().unwrap(),
                                Type::Unit,
                                expression.span,
                            ));
                        }
                        return Ok(Type::Unit);
                    }
                }
                let value_type = self.check_binding(value, annotation.as_ref())?;
                self.bind_local_value(name.clone(), value_type, expression.id, *mutable);
                Type::Unit
            }
            ExprKind::LetPattern {
                pattern,
                binding_ids,
                mutable,
                value,
            } => {
                if pattern_has_enum(pattern) {
                    return Err(SemanticError::TypeMismatch {
                        expected: "tuple pattern".to_owned(),
                        actual: "enum pattern".to_owned(),
                        span: pattern.span(),
                    });
                }
                let value_type = self.check_expression(value, TypeExpectation::none())?;
                let mut bindings = Vec::new();
                let mut binding_names = std::collections::HashSet::new();
                self.check_pattern(pattern, value_type, &mut bindings, &mut binding_names)?;
                for (name, ty) in bindings {
                    let id = binding_ids.iter().find(|(n, _)| n == &name).unwrap().1;
                    self.bind_local_value(name, ty, id, *mutable);
                }
                Type::Unit
            }
            ExprKind::Unary {
                op: UnaryOp::Negate,
                expression: inner,
            } => self.check_negation(inner, expectation)?,
            ExprKind::Unary {
                op: UnaryOp::BitNot,
                expression: inner,
            } => self.check_bit_not(inner, expectation)?,
            ExprKind::Unary {
                op: UnaryOp::Not,
                expression: inner,
            } => {
                self.check_expression(inner, TypeExpectation::require(Type::Bool))?;
                Type::Bool
            }
            ExprKind::Unwrap { value, propagate } => {
                let wrapped = self.check_expression(value, TypeExpectation::none())?;
                let inner = match wrapped {
                    Type::Option(id) => self.option_types[id],
                    Type::Result(id) => self.result_types[id].0,
                    _ => {
                        return Err(SemanticError::TypeMismatch {
                            expected: "Option or Result".to_owned(),
                            actual: type_name(wrapped),
                            span: value.span,
                        })
                    }
                };
                if *propagate {
                    let return_type = self.return_types.last().copied().unwrap_or(Type::Unit);
                    let compatible = match (wrapped, return_type) {
                        (Type::Option(source), Type::Option(target)) => source == target,
                        (Type::Result(source), Type::Result(target)) => {
                            self.result_types[source].1 == self.result_types[target].1
                        }
                        _ => false,
                    };
                    if !compatible {
                        return Err(SemanticError::TypeMismatch {
                            expected: type_name(return_type),
                            actual: type_name(wrapped),
                            span: expression.span,
                        });
                    }
                }
                inner
            }
            ExprKind::Cast {
                value,
                mode,
                target,
            } => {
                let source = self.check_expression(value, TypeExpectation::none())?;
                let target_ty = self.resolve_type(target)?;
                self.check_cast_modes(*mode, source, target_ty, expression.span)?
            }
            ExprKind::Range {
                start, end, step, ..
            } => self.check_range(start, end, step.as_deref(), expectation)?,
            ExprKind::Binary { .. } => self.check_binary_tree(expression, expectation)?,
            ExprKind::Call { callee, arguments } => self.check_call(
                callee,
                arguments,
                expression.span,
                expression.id,
                expectation,
            )?,
            ExprKind::Closure {
                move_capture,
                parameters,
                return_type,
                body,
            } => self.check_closure(
                *move_capture,
                parameters,
                return_type,
                body,
                expression.span,
                expression.id,
                expectation,
                false,
            )?,
            ExprKind::Tuple(elements) => {
                if elements.is_empty() {
                    Type::Unit
                } else {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::Tuple(id) => Some(self.tuple_types[id].clone()),
                        _ => None,
                    });
                    let fields = elements
                        .iter()
                        .enumerate()
                        .map(|(index, element)| {
                            let hint = expected
                                .as_ref()
                                .and_then(|fields| fields.get(index))
                                .copied()
                                .map(TypeExpectation::require)
                                .unwrap_or_else(TypeExpectation::none);
                            self.check_expression(element, hint)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Type::Tuple(intern_tuple(&mut self.tuple_types, fields))
                }
            }
            ExprKind::CollectionLiteral(literal) => match literal {
                CollectionLiteral::List(elements) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::List(id) => Some(self.list_types[id]),
                        _ => None,
                    });
                    if let Some(first) = elements.first() {
                        let element = self.check_expression(
                            first,
                            expected
                                .map(TypeExpectation::require)
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_list_element(element, first.span)?;
                        for item in &elements[1..] {
                            self.check_expression(item, TypeExpectation::require(element))?;
                        }
                        Type::List(intern_list(&mut self.list_types, element))
                    } else {
                        expected
                            .map(|element| Type::List(intern_list(&mut self.list_types, element)))
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty List literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
                CollectionLiteral::MutList(elements) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::MutList(id) => Some(self.list_types[id]),
                        _ => None,
                    });
                    if let Some(first) = elements.first() {
                        let element = self.check_expression(
                            first,
                            expected
                                .map(TypeExpectation::require)
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_mut_list_element(element, first.span)?;
                        for item in &elements[1..] {
                            self.check_expression(item, TypeExpectation::require(element))?;
                        }
                        Type::MutList(intern_list(&mut self.list_types, element))
                    } else {
                        expected
                            .map(|element| {
                                Type::MutList(intern_list(&mut self.list_types, element))
                            })
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty MutList literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
                CollectionLiteral::Set(elements) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::Map(id) | Type::MutMap(id) => Some(self.maps[id].key),
                        _ => None,
                    });
                    if let Some(first) = elements.first() {
                        let element = self.check_expression(
                            first,
                            expected
                                .map(TypeExpectation::require)
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_map_key(element, first.span)?;
                        for item in &elements[1..] {
                            self.check_expression(item, TypeExpectation::require(element))?;
                        }
                        Type::Map(intern_map(&mut self.maps, element, Type::Bool))
                    } else {
                        expected
                            .map(|key| Type::Map(intern_map(&mut self.maps, key, Type::Bool)))
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty Set literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
                CollectionLiteral::MutSet(elements) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::MutSet(id) => Some(self.maps[id].key),
                        _ => None,
                    });
                    if let Some(first) = elements.first() {
                        let element = self.check_expression(
                            first,
                            expected
                                .map(TypeExpectation::require)
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_map_key(element, first.span)?;
                        for item in &elements[1..] {
                            self.check_expression(item, TypeExpectation::require(element))?;
                        }
                        Type::MutSet(intern_map(&mut self.maps, element, Type::Bool))
                    } else {
                        expected
                            .map(|key| Type::MutSet(intern_map(&mut self.maps, key, Type::Bool)))
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty MutSet literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
                CollectionLiteral::Map(entries) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::Map(id) | Type::MutMap(id) => Some(self.maps[id]),
                        _ => None,
                    });
                    if let Some(first) = entries.first() {
                        let key = self.check_expression(
                            &first.key,
                            expected
                                .map(|info| TypeExpectation::require(info.key))
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_map_key(key, first.key.span)?;
                        let value = self.check_expression(
                            &first.value,
                            expected
                                .map(|info| TypeExpectation::require(info.value))
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_list_element(value, first.value.span)?;
                        for entry in &entries[1..] {
                            self.check_expression(&entry.key, TypeExpectation::require(key))?;
                            self.check_expression(&entry.value, TypeExpectation::require(value))?;
                        }
                        Type::Map(intern_map(&mut self.maps, key, value))
                    } else {
                        expected
                            .map(|info| Type::Map(intern_map(&mut self.maps, info.key, info.value)))
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty Map literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
                CollectionLiteral::MutMap(entries) => {
                    let expected = expectation.ty().and_then(|ty| match ty {
                        Type::MutMap(id) => Some(self.maps[id]),
                        _ => None,
                    });
                    if let Some(first) = entries.first() {
                        let key = self.check_expression(
                            &first.key,
                            expected
                                .map(|info| TypeExpectation::require(info.key))
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_map_key(key, first.key.span)?;
                        let value = self.check_expression(
                            &first.value,
                            expected
                                .map(|info| TypeExpectation::require(info.value))
                                .unwrap_or_else(TypeExpectation::none),
                        )?;
                        self.check_mut_list_element(value, first.value.span)?;
                        for entry in &entries[1..] {
                            self.check_expression(&entry.key, TypeExpectation::require(key))?;
                            self.check_expression(&entry.value, TypeExpectation::require(value))?;
                        }
                        Type::MutMap(intern_map(&mut self.maps, key, value))
                    } else {
                        expected
                            .map(|info| {
                                Type::MutMap(intern_map(&mut self.maps, info.key, info.value))
                            })
                            .ok_or_else(|| SemanticError::UnknownValue {
                                name: "empty MutMap literal requires an explicit type".to_owned(),
                                span: expression.span,
                            })?
                    }
                }
            },
            ExprKind::StructInit { name, fields } => {
                if self.classes.contains_key(name) {
                    self.check_class_init(name, fields, expression)?
                } else {
                    self.check_struct_init(name, fields, expression)?
                }
            }
            ExprKind::Assign { target, value } => self.check_assignment(target, value)?,
            ExprKind::CompoundAssign {
                target,
                operator,
                value,
            } => self.check_compound_assignment(target, *operator, value)?,
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.check_if(condition, then_branch, else_branch, expectation)?,
            ExprKind::Match { value, arms } => self.check_match(value, arms, expectation)?,
            ExprKind::For {
                index,
                item,
                iterable,
                body,
                limit,
            } => self.check_for(
                index.as_deref(),
                item,
                iterable,
                body,
                limit.as_deref(),
                expectation,
                expression.span,
            )?,
            ExprKind::While { condition, body } => {
                self.check_expression(condition, TypeExpectation::require(Type::Bool))?;
                self.loop_break_types.push(Some(Type::Unit));
                self.check_expression(body, TypeExpectation::none())?;
                self.loop_break_types.pop();
                Type::Unit
            }
            ExprKind::Loop { body } => self.check_loop(body, expectation)?,
            ExprKind::Break { value } => {
                if self.loop_break_types.is_empty() {
                    return Err(SemanticError::BreakOutsideLoop {
                        span: expression.span,
                    });
                }
                let value_type = value
                    .as_deref()
                    .map(|value| {
                        self.check_expression(
                            value,
                            TypeExpectation::from_option(self.current_break_type()),
                        )
                    })
                    .transpose()?
                    .unwrap_or(Type::Unit);
                let slot = self
                    .loop_break_types
                    .last_mut()
                    .expect("loop context checked above");
                if let Some(expected) = *slot {
                    if expected != value_type {
                        return Err(type_mismatch(expected, value_type, expression.span));
                    }
                } else {
                    *slot = Some(value_type);
                }
                Type::Unit
            }
            ExprKind::Continue => {
                if self.loop_break_types.is_empty() {
                    return Err(SemanticError::ContinueOutsideLoop {
                        span: expression.span,
                    });
                }
                Type::Unit
            }
            ExprKind::Abort { value } => {
                let expected = self.handler_abort_types.last().copied().ok_or_else(|| {
                    SemanticError::FunctionNotSupported {
                        name: "abort is only valid inside an effect handler".to_owned(),
                        span: expression.span,
                    }
                })?;
                self.check_expression(value, TypeExpectation::require(expected))?;
                expected
            }
            ExprKind::When {
                cowns,
                bindings,
                until,
                body,
            } => self.check_when(
                cowns,
                bindings.as_deref(),
                until.as_deref(),
                body,
                expression.span,
            )?,
            ExprKind::Do { body, handlers } => self.check_do(body, handlers, expectation)?,
            ExprKind::Parallel(arms) => {
                for arm in arms {
                    self.check_mutable_captures(arm, [], "parallel")?;
                }
                let outer_loops = std::mem::take(&mut self.loop_break_types);
                let checked = arms
                    .iter()
                    .map(|arm| self.check_block(std::slice::from_ref(arm), TypeExpectation::none()))
                    .collect::<Result<Vec<_>, _>>();
                self.loop_break_types = outer_loops;
                let values = checked?;
                Type::Tuple(intern_tuple(&mut self.tuple_types, values))
            }
            ExprKind::Race(arms) => {
                for arm in arms {
                    self.check_mutable_captures(arm, [], "race")?;
                }
                let Some((first, rest)) = arms.split_first() else {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "race requires at least one arm".to_owned(),
                        span: expression.span,
                    });
                };
                let outer_loops = std::mem::take(&mut self.loop_break_types);
                let result = (|| {
                    let ty =
                        self.check_block(std::slice::from_ref(first), TypeExpectation::none())?;
                    for arm in rest {
                        self.check_block(std::slice::from_ref(arm), TypeExpectation::require(ty))?;
                    }
                    Ok::<_, SemanticError>(ty)
                })();
                self.loop_break_types = outer_loops;
                result?
            }
            ExprKind::Branch(body) => {
                self.check_mutable_captures(body, [], "branch")?;
                let outer_loops = std::mem::take(&mut self.loop_break_types);
                let result = self.check_expression(body, TypeExpectation::none());
                self.loop_break_types = outer_loops;
                result?;
                Type::Unit
            }
            ExprKind::Region(body) => {
                if self.when_depth != 0 {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "region cannot be entered while holding a when lease".into(),
                        span: expression.span,
                    });
                }
                self.check_expression(body, expectation)?
            }
            ExprKind::Block(expressions) => self.check_block(expressions, expectation)?,
        };

        let actual = if super::control_flow::does_not_complete(expression) {
            expectation.ty().unwrap_or(actual)
        } else {
            actual
        };
        if expectation.is_required() {
            if let Some(expected) = expectation.ty() {
                if actual != expected {
                    return Err(type_mismatch(expected, actual, expression.span));
                }
            }
        }

        let transparent_echo = matches!(&expression.kind, ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Name(name) if name == "echo"));
        if !transparent_echo
            && !matches!(expression.kind, ExprKind::Name(_) | ExprKind::Field { .. })
        {
            self.record_drop_effects(actual, expression.span)?;
        }
        self.types.insert(expression.id, actual);
        Ok(actual)
    }

    fn resolve_constant_name(&self, name: &str) -> Option<String> {
        if let Some(prefix) = &self.current_module_prefix {
            let qualified = format!("{prefix}{name}");
            if self.constants.contains_key(&qualified) {
                return Some(qualified);
            }
        }
        self.constants.contains_key(name).then(|| name.to_owned())
    }

    /// Returns the break type of the current loop
    fn current_break_type(&self) -> Option<Type> {
        self.loop_break_types.last().copied().flatten()
    }

    /// Checks the value of a let binding
    fn check_binding(
        &mut self,
        value: &Expr,
        annotation: Option<&TypeAnnotation>,
    ) -> Result<Type, SemanticError> {
        let expected = annotation
            .map(|annotation| self.resolve_type(annotation))
            .transpose()?;
        // With a type annotation, the value is required to be that type
        let expectation = match expected {
            Some(ty) => TypeExpectation::require(ty),
            None => TypeExpectation::none(),
        };
        self.check_expression(value, expectation)
    }
}
