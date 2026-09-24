//! Pattern-test and pattern-binding lowering for typed MIR.

use super::*;
use crate::hir::CorePattern;

impl Lowerer<'_> {
    pub(super) fn lower_pattern_test(
        &mut self,
        pattern: &CorePattern,
        value: MirValueId,
        success: MirBlockId,
        failure: MirBlockId,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        match pattern {
            CorePattern::Wildcard { .. } | CorePattern::Binding { .. } => {
                self.terminate(MirTerminator::Goto {
                    target: success,
                    arguments: Vec::new(),
                })
            }
            CorePattern::EnumVariant {
                enum_name,
                variant,
                fields,
                ..
            } => {
                let (_, variant_index, declared) =
                    resolved_pattern_variant(enum_name, variant, self.value_types[value.0], types)?;
                let tag = self.next_value(Type::I32);
                self.push_statement(MirStatement::EnumTag {
                    destination: tag,
                    value,
                });
                let expected = self.next_value(Type::I32);
                self.push_statement(MirStatement::Const {
                    destination: expected,
                    value: MirConstant::Integer(variant_index as u64),
                });
                let condition = self.next_value(Type::Bool);
                self.push_statement(MirStatement::Binary {
                    destination: condition,
                    op: BinaryOp::Equal,
                    left: tag,
                    right: expected,
                });
                let field_test = (!fields.is_empty()).then(|| self.new_block());
                self.terminate(MirTerminator::Branch {
                    condition,
                    then_block: field_test.unwrap_or(success),
                    else_block: failure,
                })?;
                if let Some(mut test_block) = field_test {
                    let fields = if matches!(enum_name.as_str(), "Option" | "Result") {
                        fields.iter().map(|field| &field.pattern).collect()
                    } else {
                        ordered_pattern_fields(fields, &declared)?
                    };
                    for (index, pattern) in fields.into_iter().enumerate() {
                        self.switch_to(test_block);
                        let field_type = declared[index].1;
                        let payload = if types.is_owned(field_type) {
                            self.next_value_with_ownership(field_type, MirOwnership::Borrowed)
                        } else {
                            self.next_value(field_type)
                        };
                        self.push_statement(MirStatement::EnumProject {
                            destination: payload,
                            value,
                            variant: variant_index,
                            field: index,
                        });
                        let next = (index + 1 < declared.len()).then(|| self.new_block());
                        self.lower_pattern_test(
                            pattern,
                            payload,
                            next.unwrap_or(success),
                            failure,
                            types,
                        )?;
                        if let Some(next) = next {
                            test_block = next;
                        }
                    }
                }
                Ok(())
            }
            CorePattern::Tuple { elements, .. } => {
                if elements.is_empty() {
                    return self.terminate(MirTerminator::Goto {
                        target: success,
                        arguments: Vec::new(),
                    });
                }
                let Type::Tuple(tuple_id) = self.value_types[value.0] else {
                    return Err(Diagnostic::codegen("tuple pattern value was not resolved"));
                };
                let field_types = types.tuple_elements(tuple_id).to_vec();
                if elements.len() != field_types.len() {
                    return Err(Diagnostic::codegen("tuple pattern arity was not resolved"));
                }
                let mut test_block = self.current;
                for (index, (pattern, field_type)) in elements.iter().zip(field_types).enumerate() {
                    self.switch_to(test_block);
                    let payload = if types.is_owned(field_type) {
                        self.next_value_with_ownership(field_type, MirOwnership::Borrowed)
                    } else {
                        self.next_value(field_type)
                    };
                    self.push_statement(MirStatement::Project {
                        destination: payload,
                        base: value,
                        access: MirFieldAccess::Index(index),
                    });
                    let next = (index + 1 < elements.len()).then(|| self.new_block());
                    self.lower_pattern_test(
                        pattern,
                        payload,
                        next.unwrap_or(success),
                        failure,
                        types,
                    )?;
                    if let Some(next) = next {
                        test_block = next;
                    }
                }
                Ok(())
            }
        }
    }

    pub(super) fn lower_pattern_bindings(
        &mut self,
        pattern: &CorePattern,
        value: MirValueId,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        self.lower_pattern_bindings_with_mutable(pattern, value, types, false)
    }

    pub(super) fn lower_pattern_bindings_with_mutable(
        &mut self,
        pattern: &CorePattern,
        value: MirValueId,
        types: &CheckedTypes,
        mutable: bool,
    ) -> Result<(), Diagnostic> {
        match pattern {
            CorePattern::Wildcard { .. } => {
                self.discard_value(Some(value));
                Ok(())
            }
            CorePattern::Binding { name, .. } => {
                let local = self.new_local_with_ownership(
                    name,
                    self.value_types[value.0],
                    self.value_ownership[value.0],
                );
                self.bind_local(name, local);
                if mutable {
                    self.mutable_locals
                        .insert(local, self.current_span.unwrap_or(crate::Span::new(0, 0)));
                }
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(value),
                    destination,
                });
                Ok(())
            }
            CorePattern::EnumVariant {
                enum_name,
                variant,
                fields,
                ..
            } => {
                let (_, variant_index, declared) =
                    resolved_pattern_variant(enum_name, variant, self.value_types[value.0], types)?;
                let fields = if matches!(enum_name.as_str(), "Option" | "Result") {
                    fields.iter().map(|field| &field.pattern).collect()
                } else {
                    ordered_pattern_fields(fields, &declared)?
                };
                for (index, pattern) in fields.into_iter().enumerate() {
                    let field_type = declared[index].1;
                    let payload = if self.value_ownership[value.0] == MirOwnership::Borrowed
                        && types.is_owned(field_type)
                    {
                        self.next_value_with_ownership(field_type, MirOwnership::Borrowed)
                    } else {
                        self.next_value(field_type)
                    };
                    self.push_statement(MirStatement::EnumProject {
                        destination: payload,
                        value,
                        variant: variant_index,
                        field: index,
                    });
                    let payload = self.retain_pattern_payload(value, payload, types);
                    self.lower_pattern_bindings_with_mutable(pattern, payload, types, mutable)?;
                }
                if self.value_ownership[value.0] == MirOwnership::Owned {
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Deinit {
                        destination,
                        value,
                        variant: Some(variant_index),
                    });
                }
                Ok(())
            }
            CorePattern::Tuple { elements, .. } => {
                if elements.is_empty() {
                    self.discard_value(Some(value));
                    return Ok(());
                }
                let Type::Tuple(tuple_id) = self.value_types[value.0] else {
                    return Err(Diagnostic::codegen("tuple pattern value was not resolved"));
                };
                let field_types = types.tuple_elements(tuple_id).to_vec();
                if elements.len() != field_types.len() {
                    return Err(Diagnostic::codegen("tuple pattern arity was not resolved"));
                }
                for (index, (pattern, field_type)) in elements.iter().zip(field_types).enumerate() {
                    let payload = if self.value_ownership[value.0] == MirOwnership::Borrowed
                        && types.is_owned(field_type)
                    {
                        self.next_value_with_ownership(field_type, MirOwnership::Borrowed)
                    } else {
                        self.next_value(field_type)
                    };
                    self.push_statement(MirStatement::Project {
                        destination: payload,
                        base: value,
                        access: MirFieldAccess::Index(index),
                    });
                    let payload = self.retain_pattern_payload(value, payload, types);
                    self.lower_pattern_bindings_with_mutable(pattern, payload, types, mutable)?;
                }
                if self.value_ownership[value.0] == MirOwnership::Owned {
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Deinit {
                        destination,
                        value,
                        variant: None,
                    });
                }
                Ok(())
            }
        }
    }

    fn retain_pattern_payload(
        &mut self,
        aggregate: MirValueId,
        payload: MirValueId,
        types: &CheckedTypes,
    ) -> MirValueId {
        // Borrowed aggregates keep their reference. Owned pattern matching
        // transfers it to the binding instead.
        if self.value_ownership[aggregate.0] == MirOwnership::Borrowed
            && types.is_shared(self.value_types[payload.0])
        {
            let destination = self.next_value(self.value_types[payload.0]);
            self.push_statement(MirStatement::Dup {
                destination,
                value: payload,
            });
            destination
        } else {
            payload
        }
    }
}
