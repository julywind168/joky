use super::*;
use std::collections::HashSet;
impl Checker {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::sema) fn check_closure(
        &mut self,
        move_capture: bool,
        parameters: &[crate::syntax::Parameter],
        return_annotation: &TypeAnnotation,
        body: &Expr,
        span: crate::Span,
        closure_id: crate::syntax::NodeId,
        expectation: TypeExpectation,
        infer_return: bool,
    ) -> Result<Type, SemanticError> {
        let expected_signature = expectation.ty().and_then(|ty| match ty {
            Type::Function(id) => self.function_types.get(id).cloned(),
            _ => None,
        });
        if expected_signature
            .as_ref()
            .is_some_and(|signature| signature.parameters.len() != parameters.len())
        {
            return Err(SemanticError::WrongArgumentCount {
                function: "closure".to_owned(),
                expected: expected_signature.expect("checked").parameters.len(),
                span,
            });
        }
        let parameter_types = parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                if parameter.ty.is_name("_") {
                    expected_signature
                        .as_ref()
                        .and_then(|signature| signature.parameters.get(index).copied())
                        .ok_or_else(|| SemanticError::FunctionNotSupported {
                            name: "closure parameter types require a function type context"
                                .to_owned(),
                            span: parameter.span,
                        })
                } else {
                    self.resolve_type(&parameter.ty)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parameter_names = parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let return_type = if return_annotation.is_name("_") {
            if infer_return {
                None
            } else {
                Some(
                    expected_signature
                        .as_ref()
                        .map(|signature| signature.return_type)
                        .ok_or_else(|| SemanticError::FunctionNotSupported {
                            name: "closure return type requires a function type context".to_owned(),
                            span: return_annotation.span,
                        })?,
                )
            }
        } else {
            Some(self.resolve_type(return_annotation)?)
        };
        let mut captures = Vec::new();
        let mut capture_bindings = Vec::new();
        for (name, capture_span) in
            crate::sema::free_names::free_names(body, parameter_names.iter().cloned())
        {
            if let Some(binding) = self.lookup(&name) {
                if binding.type_value.is_some() {
                    continue;
                }
                if binding.mutable {
                    if !move_capture {
                        return Err(SemanticError::MutableCapture {
                            name,
                            boundary: "closure".to_owned(),
                            span: capture_span,
                        });
                    }
                    if binding.declaration.is_some_and(|declaration| {
                        self.borrowed_mutable_captures.contains(&declaration)
                    }) {
                        return Err(SemanticError::FunctionNotSupported {
                            name: format!(
                                "cannot move mutable capture '{name}' out of a borrowed closure environment; initialize a local 'var' first"
                            ),
                            span: capture_span,
                        });
                    }
                }
                if self.drop_contract(binding.value_type).0 {
                    return Err(SemanticError::FunctionNotSupported {
                        name: "Drop values cannot be captured in shared closure environments yet"
                            .into(),
                        span,
                    });
                }
                captures.push((name.clone(), binding.value_type));
                capture_bindings.push(super::super::ClosureCaptureBinding {
                    name: name.clone(),
                    declaration: binding.declaration,
                    mutable: binding.mutable,
                });
                if self.is_owned_capture_type(binding.value_type) && !move_capture {
                    return Err(SemanticError::FunctionNotSupported {
                        name: format!(
                            "closure captures owned value '{name}'; use 'move fn' to move it"
                        ),
                        span,
                    });
                }
            }
        }
        captures.sort_by(|left, right| left.0.cmp(&right.0));
        capture_bindings.sort_by(|left, right| left.name.cmp(&right.name));
        let saved_borrowed_captures = self.borrowed_mutable_captures.clone();
        self.borrowed_mutable_captures.extend(
            capture_bindings
                .iter()
                .filter(|capture| capture.mutable)
                .filter_map(|capture| capture.declaration),
        );
        self.closure_captures.insert(closure_id, captures);
        self.closure_capture_bindings
            .insert(closure_id, capture_bindings);
        let saved_effects = std::mem::take(&mut self.current_effects);
        self.scopes.push(std::collections::HashMap::new());
        for (parameter, ty) in parameters.iter().zip(&parameter_types) {
            self.bind(parameter.name.clone(), *ty);
        }
        if let Some(return_type) = return_type {
            self.return_types.push(return_type);
        }
        let outer_loops = std::mem::take(&mut self.loop_break_types);
        let body_result = self.check_expression(
            body,
            return_type.map_or_else(TypeExpectation::none, TypeExpectation::require),
        );
        self.loop_break_types = outer_loops;
        self.borrowed_mutable_captures = saved_borrowed_captures;
        if return_type.is_some() {
            self.return_types.pop();
        }
        self.scopes.pop();
        let effects = std::mem::replace(&mut self.current_effects, saved_effects);
        let body_type = body_result?;
        let return_type = return_type.unwrap_or(body_type);
        if body_type != return_type {
            return Err(type_mismatch(return_type, body_type, span));
        }
        let suspends = effects.may_suspend(&self.effects);
        let stored_names = if infer_return {
            expected_signature
                .as_ref()
                .map(|signature| signature.parameter_names.clone())
                .unwrap_or_else(|| parameter_names.clone())
        } else {
            parameter_names.clone()
        };
        let id = if infer_return {
            self.function_types
                .iter()
                .position(|info| {
                    info.parameter_names == stored_names
                        && info.parameters == parameter_types
                        && info.return_type == return_type
                        && info.suspends == suspends
                        && info.effects == effects
                })
                .unwrap_or_else(|| {
                    let id = self.function_types.len();
                    self.function_types
                        .push(crate::sema::symbol_table::FunctionTypeInfo {
                            parameter_names: stored_names,
                            parameters: parameter_types,
                            return_type,
                            effects,
                            suspends,
                        });
                    id
                })
        } else if let Some(Type::Function(expected)) = expectation.ty() {
            let signature = &self.function_types[expected];
            if signature.parameters != parameter_types
                || signature.return_type != return_type
                || signature.suspends != suspends
                || signature.effects != effects
            {
                if signature.parameters == parameter_types && signature.return_type == return_type {
                    if signature.suspends != suspends {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "closures assigned to this function type must not suspend".into(),
                            span,
                        });
                    }
                    if signature.effects != effects {
                        return Err(SemanticError::FunctionNotSupported {
                            name: "closures assigned to this function type must not use effects"
                                .into(),
                            span,
                        });
                    }
                }
                return Err(type_mismatch(
                    Type::Function(expected),
                    Type::Function(expected),
                    span,
                ));
            }
            expected
        } else if let Some((id, _)) = self.function_types.iter().enumerate().find(|(_, info)| {
            info.parameters == parameter_types
                && info.return_type == return_type
                && info.suspends == suspends
                && info.effects == effects
        }) {
            id
        } else {
            let id = self.function_types.len();
            self.function_types
                .push(crate::sema::symbol_table::FunctionTypeInfo {
                    parameter_names,
                    parameters: parameter_types,
                    return_type,
                    effects,
                    suspends,
                });
            id
        };
        Ok(Type::Function(id))
    }

    pub(in crate::sema) fn is_callback_capture(&self, ty: Type, seen: &mut HashSet<Type>) -> bool {
        if !seen.insert(ty) {
            return true;
        }
        match ty {
            ty if ty.is_c_abi_compatible() => true,
            Type::Bool | Type::Duration | Type::Unit | Type::String | Type::Bytes => true,
            Type::Tuple(id) => self.tuple_types[id]
                .iter()
                .all(|ty| self.is_callback_capture(*ty, seen)),
            Type::Option(id) => self.is_callback_capture(self.option_types[id], seen),
            Type::Result(id) => {
                let (ok, error) = self.result_types[id];
                self.is_callback_capture(ok, seen) && self.is_callback_capture(error, seen)
            }
            Type::List(id) => self.is_callback_capture(self.list_types[id], seen),
            Type::Map(id) => {
                self.is_callback_capture(self.maps[id].key, seen)
                    && self.is_callback_capture(self.maps[id].value, seen)
            }
            Type::Struct(id) => self.structs.values().find(|s| s.id == id).is_some_and(|s| {
                s.fields
                    .iter()
                    .all(|(_, ty)| self.is_callback_capture(*ty, seen))
            }),
            Type::Enum(id) => self.enums.values().find(|e| e.id == id).is_some_and(|e| {
                e.variants.iter().all(|v| {
                    v.fields
                        .iter()
                        .all(|(_, ty)| self.is_callback_capture(*ty, seen))
                })
            }),
            _ => false,
        }
    }

    pub(in crate::sema) fn is_owned_capture_type(&self, ty: Type) -> bool {
        match ty {
            Type::Function(_)
            | Type::Class(_)
            | Type::Dyn(_)
            | Type::Native(_)
            | Type::CCallback
            | Type::MutBytes
            | Type::MutList(_)
            | Type::MutMap(_)
            | Type::MutSet(_) => true,
            Type::Tuple(id) => self.tuple_types[id]
                .iter()
                .any(|field| self.is_owned_capture_type(*field)),
            Type::Struct(id) => self
                .structs
                .values()
                .find(|structure| structure.id == id)
                .is_some_and(|structure| {
                    structure
                        .fields
                        .iter()
                        .any(|(_, field)| self.is_owned_capture_type(*field))
                }),
            Type::Enum(id) => self
                .enums
                .values()
                .find(|enumeration| enumeration.id == id)
                .is_some_and(|enumeration| {
                    enumeration.variants.iter().any(|variant| {
                        variant
                            .fields
                            .iter()
                            .any(|(_, field)| self.is_owned_capture_type(*field))
                    })
                }),
            Type::Option(id) => self.is_owned_capture_type(self.option_types[id]),
            Type::Result(id) => {
                let (ok, err) = self.result_types[id];
                self.is_owned_capture_type(ok) || self.is_owned_capture_type(err)
            }
            _ => false,
        }
    }
}
