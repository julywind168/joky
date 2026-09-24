use super::*;
use crate::hir::{task_captures, task_function_name, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_task_failure_check(
        &mut self,
        scope: MirScopeId,
        continuation: MirBlockId,
        types: &CheckedTypes,
    ) -> Result<bool, Diagnostic> {
        let mut seen = HashSet::new();
        let mut handlers = self
            .handlers
            .iter()
            .rev()
            .flat_map(|frame| frame.values())
            .filter(|target| {
                // Runtime handler arms consume matching requests through the
                // continuation ABI. They do not have a CFG target carrying
                // task-failure payloads, so never build a failure edge to
                // their marker block.
                !target.runtime_dispatch
                    && types.effects().operation_mode(target.arm.operation)
                        == Some(crate::sema::EffectMode::Aborts)
                    && seen.insert(target.arm.operation)
            })
            .cloned()
            .collect::<Vec<_>>();
        handlers.sort_by_key(|target| target.arm.operation);
        let operation = self.next_value(Type::U64);
        self.push_statement(MirStatement::TaskFailureOperation {
            destination: operation,
            scope,
        });
        let no_failure = self.next_value(Type::U64);
        self.push_statement(MirStatement::Const {
            destination: no_failure,
            value: MirConstant::Integer(u64::MAX),
        });
        let completed = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: completed,
            op: crate::syntax::BinaryOp::Equal,
            left: operation,
            right: no_failure,
        });
        let mut check_block = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: completed,
            then_block: continuation,
            else_block: check_block,
        })?;

        for target in &handlers {
            self.switch_to(check_block);
            let expected = self.next_value(Type::U64);
            self.push_statement(MirStatement::Const {
                destination: expected,
                value: MirConstant::EffectOperation(target.arm.operation),
            });
            let matches = self.next_value(Type::Bool);
            self.push_statement(MirStatement::Binary {
                destination: matches,
                op: crate::syntax::BinaryOp::Equal,
                left: operation,
                right: expected,
            });
            let matched_block = self.new_block();
            let next_block = self.new_block();
            self.terminate(MirTerminator::Branch {
                condition: matches,
                then_block: matched_block,
                else_block: next_block,
            })?;

            self.switch_to(matched_block);
            let operation_info = types
                .effects()
                .operation_info(target.arm.operation)
                .expect("task failure handler operation");
            let arguments = operation_info
                .parameters
                .iter()
                .enumerate()
                .map(|(parameter, ty)| {
                    let destination =
                        self.next_value_with_ownership(*ty, ownership_for_type(*ty, types));
                    self.push_statement(MirStatement::TaskFailurePayload {
                        destination,
                        scope,
                        operation: target.arm.operation,
                        parameter,
                    });
                    destination
                })
                .collect::<Vec<_>>();
            self.push_statement(MirStatement::TaskFailureClaim { scope });
            self.push_statement(MirStatement::ScopeExit { scope });
            self.terminate(MirTerminator::Goto {
                target: target.arm.target,
                arguments,
            })?;
            check_block = next_block;
        }
        self.switch_to(check_block);
        self.release_cown_leases_from(0);
        let result = self.next_value(self.return_type);
        self.push_statement(MirStatement::TaskFailureRethrow {
            destination: result,
            scope,
        });
        for active_scope in self.task_scopes.clone().into_iter().rev() {
            self.push_statement(MirStatement::ScopeExit {
                scope: active_scope,
            });
        }
        self.blocks[self.current.0].terminator = Some(MirTerminator::Return(Some(result)));
        Ok(true)
    }

    pub(super) fn lower_task_create(
        &mut self,
        task_key: crate::syntax::NodeId,
        body: &CoreExpr,
        scope: MirScopeId,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<MirTaskId, Diagnostic> {
        let name = task_function_name(&self.function_name, task_key);
        let function = *self
            .function_ids
            .get(&name)
            .ok_or_else(|| Diagnostic::codegen("MIR task function was not resolved"))?;
        let parameter_names = self
            .function_parameters
            .get(&function)
            .map(Vec::as_slice)
            .ok_or_else(|| Diagnostic::codegen("MIR task parameters were not resolved"))?;
        let mut arguments = Vec::new();
        for (name, ty) in task_captures(body) {
            let Some(local) = self.resolve_local(&name) else {
                continue;
            };
            if self.locals[local.0].ownership == MirOwnership::Borrowed {
                return Err(Diagnostic::codegen(format!(
                    "cannot capture borrowed local '{name}' into task"
                )));
            }
            let parameter = parameter_names
                .iter()
                .position(|parameter| parameter == &name)
                .ok_or_else(|| Diagnostic::codegen("MIR task capture was not resolved"))?;
            let value = self
                .lower_value(
                    &CoreExpr {
                        id: body.id,
                        ty,
                        kind: CoreExprKind::Name(name),
                    },
                    function_names,
                    types,
                )?
                .ok_or_else(|| Diagnostic::codegen("MIR task capture does not produce a value"))?;
            arguments.push(MirCallArgument { parameter, value });
        }
        arguments.sort_by_key(|argument| argument.parameter);
        let task = self.new_task();
        self.push_statement(MirStatement::TaskCreate {
            scope,
            task,
            function,
            arguments,
        });
        Ok(task)
    }

    pub(super) fn lower_parallel(
        &mut self,
        arms: &[CoreExpr],
        expression_type: Type,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let scope = self.new_task_scope();
        self.push_statement(MirStatement::ScopeEnter {
            scope,
            region: false,
        });
        self.task_scopes.push(scope);
        let tasks = arms
            .iter()
            .map(|arm| self.lower_task_create(arm.id, arm, scope, function_names, types))
            .collect::<Result<Vec<_>, _>>()?;
        let mut values = Vec::with_capacity(tasks.len());
        for (task, arm) in tasks.iter().copied().zip(arms) {
            let destination = self.next_value(arm.ty);
            self.push_statement(MirStatement::TaskJoin {
                destination,
                scope,
                task,
            });
            values.push(destination);
            let continuation = self.new_block();
            if self.lower_task_failure_check(scope, continuation, types)? {
                self.switch_to(continuation);
            }
        }
        for task in tasks {
            self.push_statement(MirStatement::TaskClaimResult { scope, task });
        }
        self.push_statement(MirStatement::ScopeExit { scope });
        self.task_scopes.pop();
        let destination = self.next_value(expression_type);
        self.push_statement(MirStatement::Tuple {
            destination,
            elements: values,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_race(
        &mut self,
        arms: &[CoreExpr],
        result_type: Type,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let scope = self.new_task_scope();
        self.push_statement(MirStatement::ScopeEnter {
            scope,
            region: false,
        });
        self.task_scopes.push(scope);
        let tasks = arms
            .iter()
            .map(|arm| self.lower_task_create(arm.id, arm, scope, function_names, types))
            .collect::<Result<Vec<_>, _>>()?;
        self.push_statement(MirStatement::RaceStart {
            scope,
            tasks: tasks.clone(),
        });
        let destination = self.next_value(result_type);
        self.push_statement(MirStatement::RaceSelect {
            destination,
            scope,
            tasks,
        });
        let continuation = self.new_block();
        if self.lower_task_failure_check(scope, continuation, types)? {
            self.switch_to(continuation);
        }
        self.push_statement(MirStatement::ScopeExit { scope });
        self.task_scopes.pop();
        Ok(Some(destination))
    }
}
