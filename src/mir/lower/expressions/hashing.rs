//! Tuple and enum hashing use ordinary member calls to preserve custom trait dispatch.
use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    /// Builtin hash implementation for enums with fields: hash the
    /// discriminant first, then branch on variant to hash each field
    pub(super) fn lower_enum_hash(
        &mut self,
        receiver: &CoreExpr,
        state: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Type::Enum(enum_id) = receiver.ty else {
            unreachable!()
        };
        let Some(value) = self.lower_borrowed_value(receiver, functions, types)? else {
            return Ok(None);
        };
        let enum_local = self.comparison_bind(value);
        let Some(value) = self.lower_borrowed_value(state, functions, types)? else {
            return Ok(None);
        };
        let state_local = self.comparison_bind(value);

        // Hash the discriminant tag
        let enum_val = self.comparison_borrow(enum_local, types);
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value: enum_val,
        });
        let state_for_tag = self.comparison_borrow(state_local, types);
        let tag_unit = self.next_value(Type::Unit);
        self.push_statement(MirStatement::RuntimeCall {
            destination: tag_unit,
            intrinsic: RuntimeIntrinsic::Hash(Type::I32),
            arguments: vec![
                MirCallArgument {
                    parameter: 0,
                    value: tag,
                },
                MirCallArgument {
                    parameter: 1,
                    value: state_for_tag,
                },
            ],
        });

        let variants = types.enum_variants(enum_id).to_vec();
        // No branching needed if all variants are fieldless
        if variants.iter().all(|v| v.fields.is_empty()) {
            self.comparison_drop_local(enum_local);
            self.comparison_drop_local(state_local);
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Unit { destination });
            return Ok(Some(destination));
        }

        // Branch on each tag value in turn, converging on the done block
        let done = self.new_block();
        for (variant_index, variant) in variants.iter().enumerate() {
            if variant.fields.is_empty() {
                // Fieldless variant: jump straight to done
                let is_this = self.next_value(Type::Bool);
                let idx_const = self.next_value(Type::I32);
                self.push_statement(MirStatement::Const {
                    destination: idx_const,
                    value: MirConstant::Integer(variant_index as u64),
                });
                self.push_statement(MirStatement::Binary {
                    destination: is_this,
                    op: BinaryOp::Equal,
                    left: tag,
                    right: idx_const,
                });
                let this_block = self.new_block();
                let next_block = self.new_block();
                self.terminate(MirTerminator::Branch {
                    condition: is_this,
                    then_block: this_block,
                    else_block: next_block,
                })?;
                self.switch_to(this_block);
                self.terminate(MirTerminator::Goto {
                    target: done,
                    arguments: vec![],
                })?;
                self.switch_to(next_block);
            } else {
                // Variant with fields: project each field and call hash on it in turn
                let is_this = self.next_value(Type::Bool);
                let idx_const = self.next_value(Type::I32);
                self.push_statement(MirStatement::Const {
                    destination: idx_const,
                    value: MirConstant::Integer(variant_index as u64),
                });
                self.push_statement(MirStatement::Binary {
                    destination: is_this,
                    op: BinaryOp::Equal,
                    left: tag,
                    right: idx_const,
                });
                let this_block = self.new_block();
                let next_block = self.new_block();
                self.terminate(MirTerminator::Branch {
                    condition: is_this,
                    then_block: this_block,
                    else_block: next_block,
                })?;
                self.switch_to(this_block);
                for (field_index, (_, field_ty)) in variant.fields.iter().enumerate() {
                    let field_ty = *field_ty;
                    let field_local = self.comparison_project(
                        enum_local,
                        field_ty,
                        Some(variant_index),
                        field_index,
                        types,
                    );
                    let expression = |local: MirLocalId, lowerer: &Self| CoreExpr {
                        id: receiver.id,
                        ty: lowerer.locals[local.0].ty,
                        kind: CoreExprKind::Name(lowerer.locals[local.0].name.clone()),
                    };
                    let mut arguments = vec![CoreCallArgument {
                        label: None,
                        value: expression(state_local, self),
                    }];
                    let callee = if let Some(symbol) = types
                        .interface
                        .method_symbols
                        .get(&(field_ty, crate::sema::HASH_METHOD.into()))
                    {
                        arguments.insert(
                            0,
                            CoreCallArgument {
                                label: Some("self".into()),
                                value: expression(field_local, self),
                            },
                        );
                        CoreExprKind::ExternalSymbol(symbol.clone())
                    } else {
                        CoreExprKind::Field {
                            value: Box::new(expression(field_local, self)),
                            access: FieldAccess::Name(crate::sema::HASH_METHOD.into()),
                        }
                    };
                    let call = CoreExpr {
                        id: receiver.id,
                        ty: Type::Unit,
                        kind: CoreExprKind::Call {
                            type_arguments: vec![],
                            effect_operation: None,
                            callee: Box::new(CoreExpr {
                                id: receiver.id,
                                ty: Type::Unit,
                                kind: callee,
                            }),
                            arguments,
                        },
                    };
                    if self.lower_value(&call, functions, types)?.is_none() {
                        return Ok(None);
                    }
                    self.comparison_drop_local(field_local);
                }
                self.terminate(MirTerminator::Goto {
                    target: done,
                    arguments: vec![],
                })?;
                self.switch_to(next_block);
            }
        }
        // The final next_block is unreachable (every variant is covered)
        self.terminate(MirTerminator::Unreachable)?;
        self.switch_to(done);

        self.comparison_drop_local(enum_local);
        self.comparison_drop_local(state_local);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Unit { destination });
        Ok(Some(destination))
    }

    pub(super) fn lower_tuple_hash(
        &mut self,
        receiver: &CoreExpr,
        state: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Type::Tuple(id) = receiver.ty else {
            unreachable!()
        };
        let Some(value) = self.lower_borrowed_value(receiver, functions, types)? else {
            return Ok(None);
        };
        let tuple = self.comparison_bind(value);
        let Some(value) = self.lower_borrowed_value(state, functions, types)? else {
            return Ok(None);
        };
        let state = self.comparison_bind(value);
        for (index, ty) in types.tuple_elements(id).iter().copied().enumerate() {
            let member = self.comparison_project(tuple, ty, None, index, types);
            let expression = |local: MirLocalId| CoreExpr {
                id: receiver.id,
                ty: self.locals[local.0].ty,
                kind: CoreExprKind::Name(self.locals[local.0].name.clone()),
            };
            let mut arguments = vec![CoreCallArgument {
                label: None,
                value: expression(state),
            }];
            let callee = if let Some(symbol) = types
                .interface
                .method_symbols
                .get(&(ty, crate::sema::HASH_METHOD.into()))
            {
                arguments.insert(
                    0,
                    CoreCallArgument {
                        label: Some("self".into()),
                        value: expression(member),
                    },
                );
                CoreExprKind::ExternalSymbol(symbol.clone())
            } else {
                CoreExprKind::Field {
                    value: Box::new(expression(member)),
                    access: FieldAccess::Name(crate::sema::HASH_METHOD.into()),
                }
            };
            let call = CoreExpr {
                id: receiver.id,
                ty: Type::Unit,
                kind: CoreExprKind::Call {
                    type_arguments: vec![],
                    effect_operation: None,
                    callee: Box::new(CoreExpr {
                        id: receiver.id,
                        ty: Type::Unit,
                        kind: callee,
                    }),
                    arguments,
                },
            };
            if self.lower_value(&call, functions, types)?.is_none() {
                return Ok(None);
            }
        }
        self.comparison_drop_local(tuple);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Unit { destination });
        Ok(Some(destination))
    }
}
