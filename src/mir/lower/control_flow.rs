//! Control-flow lowering for typed MIR.

use super::*;
use crate::hir::CoreExpr;

impl Lowerer<'_> {
    pub(super) fn lower_task_poll(&mut self) -> Result<(), Diagnostic> {
        let cancelled = self.new_block();
        let continue_block = self.new_block();
        let condition = self.next_value(Type::Bool);
        self.push_statement(MirStatement::TaskPoll {
            destination: condition,
        });
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: cancelled,
            else_block: continue_block,
        })?;

        self.switch_to(cancelled);
        self.release_cown_leases_from(0);
        for scope in self.task_scopes.clone().into_iter().rev() {
            self.push_statement(MirStatement::ScopeExit { scope });
        }
        let result = self.next_value(self.return_type);
        self.push_statement(MirStatement::TaskCancelled {
            destination: result,
        });
        let block = &mut self.blocks[self.current.0];
        block.terminator = Some(MirTerminator::Return(Some(result)));

        self.switch_to(continue_block);
        Ok(())
    }

    /// Protect a newly returned value while checking cancellation. The
    /// cancelled edge drops initialized locals, including this result.
    pub(super) fn lower_task_poll_result(
        &mut self,
        value: MirValueId,
    ) -> Result<MirValueId, Diagnostic> {
        let ty = self.value_types[value.0];
        let ownership = self.value_ownership[value.0];
        if !matches!(ownership, MirOwnership::Owned | MirOwnership::Shared) {
            self.lower_task_poll()?;
            return Ok(value);
        }
        let local = self.new_local("<call-result>", ty);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            local,
            value: Some(value),
            destination,
        });
        self.lower_task_poll()?;
        let destination = self.next_value(ty);
        if ownership == MirOwnership::Owned {
            self.push_statement(MirStatement::TakeLocal { destination, local });
        } else {
            let read = self.next_value(ty);
            self.push_statement(MirStatement::Read {
                destination: read,
                local,
            });
            self.push_statement(MirStatement::Dup {
                destination,
                value: read,
            });
            let dropped = self.next_value(Type::Unit);
            self.push_statement(MirStatement::DropLocal {
                destination: dropped,
                local,
            });
        }
        Ok(destination)
    }

    pub(super) fn lower_logical_with_left(
        &mut self,
        operator: BinaryOp,
        left_value: MirValueId,
        right: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let rhs_block = self.new_block();
        let short_block = self.new_block();
        let merge_block = self.new_block();
        self.mark_scoped(rhs_block);
        self.mark_scoped(short_block);
        let (then_block, else_block, short_value) = match operator {
            BinaryOp::And => (rhs_block, short_block, false),
            BinaryOp::Or => (short_block, rhs_block, true),
            _ => return Err(Diagnostic::codegen("invalid logical MIR operation")),
        };
        self.terminate(MirTerminator::Branch {
            condition: left_value,
            then_block,
            else_block,
        })?;

        self.switch_to(short_block);
        let short_destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Const {
            destination: short_destination,
            value: MirConstant::Boolean(short_value),
        });
        let short_from = self.current;
        self.terminate(MirTerminator::Goto {
            target: merge_block,
            arguments: vec![short_destination],
        })?;

        let rhs_incoming =
            self.lower_branch(rhs_block, merge_block, right, function_names, types)?;
        self.switch_to(merge_block);
        let mut incoming = vec![(short_from, short_destination)];
        incoming.extend(rhs_incoming);
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_unwrap(
        &mut self,
        value: &CoreExpr,
        propagate: bool,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(wrapped) = self.lower_value(value, function_names, types)? else {
            return Ok(None);
        };
        let inner = match value.ty {
            Type::Option(id) => types.option_type(id),
            Type::Result(id) => types.result_types(id).0,
            _ => {
                return Err(Diagnostic::codegen(
                    "MIR unwrap input is not Option or Result",
                ))
            }
        };

        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value: wrapped,
        });
        let success_tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination: success_tag,
            value: MirConstant::Integer(0),
        });
        let condition = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: condition,
            op: BinaryOp::Equal,
            left: tag,
            right: success_tag,
        });

        let success_block = self.new_block();
        let failure_block = self.new_block();
        let merge_block = self.new_block();
        self.mark_scoped(success_block);
        self.mark_scoped(failure_block);
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: success_block,
            else_block: failure_block,
        })?;

        self.switch_to(success_block);
        let projected = self.next_value_with_ownership(inner, ownership_for_type(inner, types));
        self.push_statement(MirStatement::EnumProject {
            destination: projected,
            value: wrapped,
            variant: 0,
            field: 0,
        });
        let result = if types.is_owned(inner) {
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Deinit {
                destination,
                value: wrapped,
                variant: Some(0),
            });
            projected
        } else if types.is_shared(inner) {
            // EnumProject aliases the payload, so retain it before dropping the
            // wrapper and return the retained value.
            let duplicate = self.next_value_with_ownership(inner, MirOwnership::Shared);
            self.push_statement(MirStatement::Dup {
                destination: duplicate,
                value: projected,
            });
            self.discard_value(Some(wrapped));
            duplicate
        } else {
            self.discard_value(Some(wrapped));
            projected
        };
        let success_from = self.current;
        self.terminate(MirTerminator::Goto {
            target: merge_block,
            arguments: vec![result],
        })?;

        self.switch_to(failure_block);
        if propagate {
            let returned = if value.ty == self.return_type {
                wrapped
            } else {
                let (Type::Result(source_id), Type::Result(target_id)) =
                    (value.ty, self.return_type)
                else {
                    return Err(Diagnostic::codegen(
                        "MIR propagation requires compatible wrapper types",
                    ));
                };
                let error_type = types.result_types(source_id).1;
                let mut error = self
                    .next_value_with_ownership(error_type, ownership_for_type(error_type, types));
                self.push_statement(MirStatement::EnumProject {
                    destination: error,
                    value: wrapped,
                    variant: 1,
                    field: 0,
                });
                if types.is_owned(error_type) {
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Deinit {
                        destination,
                        value: wrapped,
                        variant: Some(1),
                    });
                } else if types.is_shared(error_type) {
                    let duplicate =
                        self.next_value_with_ownership(error_type, MirOwnership::Shared);
                    self.push_statement(MirStatement::Dup {
                        destination: duplicate,
                        value: error,
                    });
                    error = duplicate;
                    self.discard_value(Some(wrapped));
                } else {
                    self.discard_value(Some(wrapped));
                }
                let destination = self.next_value(self.return_type);
                self.push_statement(MirStatement::EnumConstruct {
                    destination,
                    enum_id: MirTypeId::Result(target_id),
                    variant: 1,
                    arguments: vec![MirCallArgument {
                        parameter: 0,
                        value: error,
                    }],
                });
                destination
            };
            self.terminate(MirTerminator::Return(Some(returned)))?;
        } else {
            self.discard_value(Some(wrapped));
            let message = self.next_value(Type::String);
            self.push_statement(MirStatement::Const {
                destination: message,
                value: MirConstant::String("unwrap failed".to_owned()),
            });
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: RuntimeIntrinsic::Panic,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: message,
                }],
            });
            for scope in self.task_scopes.clone().into_iter().rev() {
                self.push_statement(MirStatement::ScopeExit { scope });
            }
            self.terminate(MirTerminator::Unreachable)?;
        }

        self.switch_to(merge_block);
        let destination = self.next_value(inner);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(success_from, result)],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_match(
        &mut self,
        scrutinee: MirValueId,
        arms: &[crate::hir::CoreMatchArm],
        result_type: Type,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if arms.is_empty() {
            return Err(Diagnostic::codegen("match requires at least one arm"));
        }
        let arm_blocks = arms.iter().map(|_| self.new_block()).collect::<Vec<_>>();
        let merge = self.new_block();
        for block in &arm_blocks {
            self.mark_scoped(*block);
        }
        let fallback = self.new_block();
        let mut test_block = self.current;
        for (index, (arm, arm_block)) in arms.iter().zip(&arm_blocks).enumerate() {
            let failure = if index + 1 == arms.len() {
                fallback
            } else {
                self.new_block()
            };
            self.switch_to(test_block);
            self.lower_pattern_test(&arm.pattern, scrutinee, *arm_block, failure, types)?;
            test_block = failure;
        }
        self.switch_to(fallback);
        self.terminate(MirTerminator::Unreachable)?;

        let mut incoming = Vec::new();
        for (arm, block) in arms.iter().zip(arm_blocks) {
            self.switch_to(block);
            self.bindings.push(HashMap::new());
            self.lower_pattern_bindings(&arm.pattern, scrutinee, types)?;
            let value_result = self.lower_value(&arm.value, function_names, types);
            self.bindings.pop().expect("match arm binding scope");
            let value = value_result?;
            if !self.is_open() {
                continue;
            }
            let value = match value {
                Some(value) => value,
                None if result_type == Type::Unit => {
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Unit { destination });
                    destination
                }
                None => {
                    return Err(Diagnostic::codegen(
                        "MIR match arm does not produce its declared value",
                    ));
                }
            };
            let from = self.current;
            self.terminate(MirTerminator::Goto {
                target: merge,
                arguments: vec![value],
            })?;
            incoming.push((from, value));
        }
        self.switch_to(merge);
        if incoming.is_empty() {
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        }
        let destination = self.next_value(result_type);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }
    pub(super) fn lower_if(
        &mut self,
        condition: &CoreExpr,
        then_branch: &CoreExpr,
        else_branch: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(condition) = self.lower_value(condition, function_names, types)? else {
            return Ok(None);
        };
        let then_block = self.new_block();
        let else_block = self.new_block();
        let merge_block = self.new_block();
        self.mark_scoped(then_block);
        self.mark_scoped(else_block);
        self.terminate(MirTerminator::Branch {
            condition,
            then_block,
            else_block,
        })?;

        let then_incoming =
            self.lower_branch(then_block, merge_block, then_branch, function_names, types)?;
        let else_incoming =
            self.lower_branch(else_block, merge_block, else_branch, function_names, types)?;
        self.switch_to(merge_block);

        let incoming = then_incoming
            .into_iter()
            .chain(else_incoming)
            .collect::<Vec<_>>();
        if incoming.is_empty() {
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        }
        let ty = incoming
            .first()
            .and_then(|(_, value)| self.value_types.get(value.0))
            .copied()
            .unwrap_or(Type::Unit);
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_branch(
        &mut self,
        branch: MirBlockId,
        merge: MirBlockId,
        expression: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Vec<(MirBlockId, MirValueId)>, Diagnostic> {
        self.switch_to(branch);
        let value = self.lower_value(expression, function_names, types)?;
        if !self.is_open() {
            return Ok(Vec::new());
        }
        let value = match value {
            Some(value) => value,
            None if expression.ty == Type::Unit => {
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Unit { destination });
                destination
            }
            None => {
                return Err(Diagnostic::codegen(
                    "MIR branch does not produce its declared value",
                ));
            }
        };
        let from = self.current;
        self.terminate(MirTerminator::Goto {
            target: merge,
            arguments: vec![value],
        })?;
        Ok(vec![(from, value)])
    }

    pub(super) fn lower_while(
        &mut self,
        condition: &CoreExpr,
        body: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        let head = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(body_block);
        self.terminate(MirTerminator::Goto {
            target: head,
            arguments: Vec::new(),
        })?;
        self.switch_to(head);
        self.lower_task_poll()?;
        let Some(condition) = self.lower_value(condition, function_names, types)? else {
            return Ok(());
        };
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: body_block,
            else_block: exit,
        })?;
        self.loops.push(LoopFrame {
            continue_block: head,
            break_block: exit,
            breaks: Vec::new(),
        });
        self.switch_to(body_block);
        let body_value = self.lower_value(body, function_names, types)?;
        if self.is_open() {
            self.discard_value(body_value);
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: Vec::new(),
            })?;
        }
        self.loops.pop();
        self.switch_to(exit);
        Ok(())
    }

    pub(super) fn lower_loop(
        &mut self,
        body: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let head = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(body_block);
        self.terminate(MirTerminator::Goto {
            target: head,
            arguments: Vec::new(),
        })?;
        self.switch_to(head);
        self.lower_task_poll()?;
        self.terminate(MirTerminator::Goto {
            target: body_block,
            arguments: Vec::new(),
        })?;
        self.loops.push(LoopFrame {
            continue_block: head,
            break_block: exit,
            breaks: Vec::new(),
        });
        self.switch_to(body_block);
        let body_value = self.lower_value(body, function_names, types)?;
        if self.is_open() {
            self.discard_value(body_value);
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: Vec::new(),
            })?;
        }
        let breaks = self.loops.pop().expect("loop frame pushed").breaks;
        self.switch_to(exit);
        if breaks.is_empty() {
            // A loop without a break cannot fall through. Close its unused
            // exit so enclosing blocks do not lower dead cleanup or suspend
            // points (including the function's final structured task wait).
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        }
        let incoming = breaks
            .into_iter()
            .filter_map(|(block, value)| value.map(|value| (block, value)))
            .collect::<Vec<_>>();
        if incoming.is_empty() {
            return Ok(None);
        }
        let ty = incoming
            .first()
            .and_then(|(_, value)| self.value_types.get(value.0))
            .copied()
            .unwrap_or(Type::Unit);
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_break(
        &mut self,
        value: Option<&CoreExpr>,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        let value = value
            .map(|value| self.lower_value(value, function_names, types))
            .transpose()?
            .flatten();
        let target = self
            .loops
            .last()
            .ok_or_else(|| Diagnostic::codegen("break is only valid inside a loop"))?
            .break_block;
        let (active_scopes, active_leases) = self.close_loop_exit_regions(target, types)?;
        let from = self.current;
        self.loops.last_mut().unwrap().breaks.push((from, value));
        let result = self.terminate(MirTerminator::Goto {
            target,
            arguments: value.into_iter().collect(),
        });
        self.task_scopes = active_scopes;
        self.cown_leases = active_leases;
        result
    }

    pub(super) fn lower_continue(&mut self, types: &CheckedTypes) -> Result<(), Diagnostic> {
        let target = self
            .loops
            .last()
            .map(|frame| frame.continue_block)
            .ok_or_else(|| Diagnostic::codegen("continue is only valid inside a loop"))?;
        let (active_scopes, active_leases) = self.close_loop_exit_regions(target, types)?;
        let result = self.terminate(MirTerminator::Goto {
            target,
            arguments: Vec::new(),
        });
        self.task_scopes = active_scopes;
        self.cown_leases = active_leases;
        result
    }

    /// A loop transfer must observe branch failure before destroying a region.
    /// Pop closed scopes while generating subsequent failure paths, so an outer
    /// failure never tries to close an already freed inner group.
    fn close_loop_exit_regions(
        &mut self,
        target: MirBlockId,
        types: &CheckedTypes,
    ) -> Result<(Vec<MirScopeId>, Vec<MirValueId>), Diagnostic> {
        let active = self.task_scopes.clone();
        let depth = self.block_task_depths[target.0];
        let lease_depth = self.block_cown_depths[target.0];
        let leases = self.cown_leases.clone();
        self.release_cown_leases_from(lease_depth);
        self.cown_leases.truncate(lease_depth);
        while self.task_scopes.len() > depth {
            let scope = *self.task_scopes.last().unwrap();
            if self.branch_regions.contains(&scope) {
                let complete = self.new_block();
                self.lower_task_failure_check(scope, complete, types)?;
                self.switch_to(complete);
            }
            self.push_statement(MirStatement::ScopeExit { scope });
            self.task_scopes.pop();
        }
        Ok((active, leases))
    }
}
