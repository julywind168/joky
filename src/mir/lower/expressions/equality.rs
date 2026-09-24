//! Conditional equality for immutable containers and product/sum types.
use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind};
use crate::syntax::{BinaryOp, NodeId};

pub(super) fn is_option_none(expression: &CoreExpr) -> bool {
    matches!(&expression.kind, CoreExprKind::Name(name) if name == "None")
}

impl Lowerer<'_> {
    /// `== None` / `!= None` is a tag test. The payload is not read.
    pub(super) fn lower_option_none_equality(
        &mut self,
        left: &CoreExpr,
        right: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let probe = if is_option_none(left) { right } else { left };
        let enter = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(enter);
        self.terminate(MirTerminator::Goto {
            target: enter,
            arguments: vec![],
        })?;
        self.switch_to(enter);
        self.bindings.push(HashMap::new());
        let result = self.lower_option_none_tag(probe, functions, types);
        self.bindings.pop();
        let from = self.current;
        if let Some(value) = result? {
            self.terminate(MirTerminator::Goto {
                target: exit,
                arguments: vec![value],
            })?;
            self.switch_to(exit);
            let destination = self.next_value(Type::Bool);
            self.push_statement(MirStatement::Phi {
                destination,
                incoming: vec![(from, value)],
            });
            Ok(Some(destination))
        } else {
            self.switch_to(exit);
            self.terminate(MirTerminator::Unreachable)?;
            Ok(None)
        }
    }

    fn lower_option_none_tag(
        &mut self,
        probe: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(value) = self.lower_borrowed_value(probe, functions, types)? else {
            return Ok(None);
        };
        // Reading the tag does not consume the operand. Keep any retained
        // shared value in this comparison's scope so its payload is released.
        let local = self.comparison_bind(value);
        let value = self.comparison_borrow(local, types);
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value,
        });
        // Option layouts use variant 0 for Some and 1 for None.
        let none_tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination: none_tag,
            value: MirConstant::Integer(1),
        });
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination,
            op: BinaryOp::Equal,
            left: tag,
            right: none_tag,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_aggregate_equality(
        &mut self,
        left: &CoreExpr,
        right: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let enter = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(enter);
        self.terminate(MirTerminator::Goto {
            target: enter,
            arguments: vec![],
        })?;
        self.switch_to(enter);
        self.bindings.push(HashMap::new());
        let result = self.lower_equality_operands(left, right, functions, types)?;
        self.bindings.pop();
        let from = self.current;
        if let Some(value) = result {
            self.terminate(MirTerminator::Goto {
                target: exit,
                arguments: vec![value],
            })?;
            self.switch_to(exit);
            let destination = self.next_value(Type::Bool);
            self.push_statement(MirStatement::Phi {
                destination,
                incoming: vec![(from, value)],
            });
            Ok(Some(destination))
        } else {
            self.switch_to(exit);
            self.terminate(MirTerminator::Unreachable)?;
            Ok(None)
        }
    }

    fn lower_equality_operands(
        &mut self,
        left: &CoreExpr,
        right: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(value) = self.lower_borrowed_value(left, functions, types)? else {
            return Ok(None);
        };
        let lhs = self.comparison_bind(value);
        let Some(value) = self.lower_borrowed_value(right, functions, types)? else {
            return Ok(None);
        };
        let rhs = self.comparison_bind(value);
        match left.ty {
            Type::List(id) => {
                self.lower_list_equality([lhs, rhs], types.list_type(id), left.id, functions, types)
            }
            Type::Tuple(id) => {
                let unequal = self.new_block();
                let done = self.new_block();
                for (index, ty) in types.tuple_elements(id).iter().copied().enumerate() {
                    let a = self.comparison_project(lhs, ty, None, index, types);
                    let b = self.comparison_project(rhs, ty, None, index, types);
                    let Some(equal) = self.equality_compare(a, b, left.id, functions, types)?
                    else {
                        return Ok(None);
                    };
                    let next = self.new_block();
                    self.terminate(MirTerminator::Branch {
                        condition: equal,
                        then_block: next,
                        else_block: unequal,
                    })?;
                    self.switch_to(next);
                }
                let equal = self.comparison_bool(true);
                let from = self.current;
                self.terminate(MirTerminator::Goto {
                    target: done,
                    arguments: vec![equal],
                })?;
                self.switch_to(unequal);
                let unequal_value = self.comparison_bool(false);
                self.terminate(MirTerminator::Goto {
                    target: done,
                    arguments: vec![unequal_value],
                })?;
                self.switch_to(done);
                let destination = self.next_value(Type::Bool);
                self.push_statement(MirStatement::Phi {
                    destination,
                    incoming: vec![(from, equal), (unequal, unequal_value)],
                });
                Ok(Some(destination))
            }
            Type::Option(_) | Type::Result(_) => {
                self.lower_variant_equality([lhs, rhs], left.id, functions, types)
            }
            _ => unreachable!("aggregate equality type checked"),
        }
    }

    fn lower_variant_equality(
        &mut self,
        locals: [MirLocalId; 2],
        source: NodeId,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let [left, right] = locals.map(|local| {
            let value = self.comparison_borrow(local, types);
            let destination = self.next_value(Type::I32);
            self.push_statement(MirStatement::EnumTag { destination, value });
            destination
        });
        let condition = self.comparison_scalar(left, right);
        let same = self.new_block();
        let different = self.new_block();
        let done = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: same,
            else_block: different,
        })?;
        self.switch_to(different);
        let value = self.comparison_bool(false);
        let mut incoming = vec![(different, value)];
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![value],
        })?;
        self.switch_to(same);
        let zero = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination: zero,
            value: MirConstant::Integer(0),
        });
        let condition = self.comparison_scalar(left, zero);
        let first = self.new_block();
        let second = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: first,
            else_block: second,
        })?;
        let payloads = match self.locals[locals[0].0].ty {
            Type::Option(id) => [Some(types.option_type(id)), None],
            Type::Result(id) => {
                let (ok, err) = types.result_types(id);
                [Some(ok), Some(err)]
            }
            _ => unreachable!(),
        };
        for (variant, (block, payload)) in [first, second].into_iter().zip(payloads).enumerate() {
            self.switch_to(block);
            let equal = if let Some(ty) = payload {
                let left = self.comparison_project(locals[0], ty, Some(variant), 0, types);
                let right = self.comparison_project(locals[1], ty, Some(variant), 0, types);
                let Some(value) = self.equality_compare(left, right, source, functions, types)?
                else {
                    continue;
                };
                value
            } else {
                self.comparison_bool(true)
            };
            incoming.push((self.current, equal));
            self.terminate(MirTerminator::Goto {
                target: done,
                arguments: vec![equal],
            })?;
        }
        self.switch_to(done);
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    fn lower_list_equality(
        &mut self,
        originals: [MirLocalId; 2],
        element: Type,
        source: NodeId,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let list = self.locals[originals[0].0].ty;
        let initial = originals.map(|local| self.comparison_copy(local));
        let start = self.current;
        let head = self.new_block();
        let advance = self.new_block();
        let done = self.new_block();
        self.terminate(MirTerminator::Goto {
            target: head,
            arguments: initial.to_vec(),
        })?;
        self.switch_to(head);
        let cursors = [self.next_value(list), self.next_value(list)];
        // Incoming tails are appended after the loop body is lowered.
        for (cursor, value) in cursors.into_iter().zip(initial) {
            self.push_statement(MirStatement::Phi {
                destination: cursor,
                incoming: vec![(start, value)],
            });
        }
        let locals = cursors.map(|value| self.comparison_bind(value));
        let empty = locals.map(|local| {
            let value = self.comparison_copy(local);
            self.comparison_intrinsic(RuntimeIntrinsic::ListIsEmpty, Type::Bool, value)
        });
        let left_empty = self.new_block();
        let left_present = self.new_block();
        let right_empty = self.new_block();
        let present = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: empty[0],
            then_block: left_empty,
            else_block: left_present,
        })?;
        self.switch_to(left_empty);
        let mut incoming = vec![(left_empty, empty[1])];
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![empty[1]],
        })?;
        self.switch_to(left_present);
        self.terminate(MirTerminator::Branch {
            condition: empty[1],
            then_block: right_empty,
            else_block: present,
        })?;
        self.switch_to(right_empty);
        let value = self.comparison_bool(false);
        incoming.push((right_empty, value));
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![value],
        })?;
        self.switch_to(present);
        let option = Type::Option(types.option_id(element).expect("equality head type"));
        let heads = locals.map(|local| {
            let value = self.comparison_copy(local);
            let value =
                self.comparison_intrinsic(RuntimeIntrinsic::ListHead(element), option, value);
            self.comparison_bind(value)
        });
        let a = self.comparison_project(heads[0], element, Some(0), 0, types);
        let b = self.comparison_project(heads[1], element, Some(0), 0, types);
        if let Some(equal) = self.equality_compare(a, b, source, functions, types)? {
            for local in heads {
                self.comparison_drop_local(local);
            }
            let unequal = self.new_block();
            self.terminate(MirTerminator::Branch {
                condition: equal,
                then_block: advance,
                else_block: unequal,
            })?;
            self.switch_to(unequal);
            let value = self.comparison_bool(false);
            incoming.push((unequal, value));
            self.terminate(MirTerminator::Goto {
                target: done,
                arguments: vec![value],
            })?;
            self.switch_to(advance);
            let option = Type::Option(types.option_id(list).expect("equality tail type"));
            let tails = locals.map(|local| {
                let value = self.comparison_copy(local);
                let value =
                    self.comparison_intrinsic(RuntimeIntrinsic::ListTail(element), option, value);
                let wrapper = self.comparison_bind(value);
                let value = self.comparison_project(wrapper, list, Some(0), 0, types);
                let tail = self.comparison_copy(value);
                self.comparison_drop_local(wrapper);
                self.comparison_drop_local(local);
                tail
            });
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: tails.to_vec(),
            })?;
            for statement in &mut self.blocks[head.0].statements {
                if let MirStatement::Phi {
                    destination,
                    incoming,
                } = statement
                {
                    if let Some(index) = cursors.iter().position(|value| value == destination) {
                        incoming.push((advance, tails[index]));
                    }
                }
            }
        } else {
            self.switch_to(advance);
            self.terminate(MirTerminator::Unreachable)?;
        }
        self.switch_to(done);
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        for local in locals {
            self.comparison_drop_local(local);
        }
        Ok(Some(destination))
    }

    fn equality_compare(
        &mut self,
        left: MirLocalId,
        right: MirLocalId,
        source: NodeId,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let [left, right] = [left, right].map(|local| CoreExpr {
            id: source,
            ty: self.locals[local.0].ty,
            kind: CoreExprKind::Name(self.locals[local.0].name.clone()),
        });
        let kind = crate::hir::lower_comparison(source, BinaryOp::Equal, left, right, types);
        self.lower_value(
            &CoreExpr {
                id: source,
                ty: Type::Bool,
                kind,
            },
            functions,
            types,
        )
    }
}
