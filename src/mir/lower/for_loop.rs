//! Bounded iteration uses a fixed set of logical workers, each reusing its
//! continuation while claiming items from a shared cursor.
use super::*;

impl Lowerer<'_> {
    pub(super) fn for_call(
        &mut self,
        ty: Type,
        intrinsic: RuntimeIntrinsic,
        values: Vec<MirValueId>,
    ) -> MirValueId {
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic,
            arguments: values
                .into_iter()
                .enumerate()
                .map(|(parameter, value)| MirCallArgument { parameter, value })
                .collect(),
        });
        destination
    }

    pub(super) fn for_bind(&mut self, name: &str, value: MirValueId) {
        let local = self.new_local(name, self.value_types[value.0]);
        self.bind_local(name, local);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            local,
            value: Some(value),
            destination,
        });
    }

    fn for_state(&mut self, state: &str) -> MirValueId {
        let local = self.resolve_local(state).expect("iteration state binding");
        let ty = self.locals[local.0].ty;
        let value = self.next_value(ty);
        self.push_statement(MirStatement::Read {
            destination: value,
            local,
        });
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::Dup { destination, value });
        destination
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for(
        &mut self,
        expression: &CoreExpr,
        index: Option<&str>,
        item: &str,
        iterable: &CoreExpr,
        body: &CoreExpr,
        limit: Option<&CoreExpr>,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(mut input) = self.lower_value(iterable, function_names, types)? else {
            return Ok(None);
        };
        let cursor_ty = types.cursor_type(iterable.id).unwrap_or(iterable.ty);
        if cursor_ty != iterable.ty {
            let intrinsic = match (iterable.ty, cursor_ty) {
                (Type::Map(id), Type::MapCursor(_)) => {
                    let info = types.map_info(id);
                    RuntimeIntrinsic::MapEntriesCursorNew(info.key, info.value)
                }
                (Type::MutMap(id), Type::MutMapCursor(_)) => {
                    let info = types.map_info(id);
                    RuntimeIntrinsic::MutMapIntoIter(info.key, info.value)
                }
                (Type::MutSet(id), Type::MutSetCursor(_)) => {
                    RuntimeIntrinsic::MutSetIntoIter(types.map_info(id).key)
                }
                (Type::MutList(id), Type::MutListCursor(_)) => {
                    RuntimeIntrinsic::MutListIntoIter(types.list_type(id))
                }
                _ => {
                    return Err(Diagnostic::codegen(
                        "IntoCursor conversion is only implemented for Map, MutMap, MutSet, and MutList",
                    ))
                }
            };
            input = self.for_call(cursor_ty, intrinsic, vec![input]);
        }
        let collecting = matches!(expression.ty, Type::List(_));
        let output_id = match expression.ty {
            Type::List(id) => Some(id),
            Type::Unit => None,
            other => unreachable!("for result is List or Unit, got {other:?}"),
        };
        let state = format!("<for-state-{:?}>", expression.id);
        let enter = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(enter);
        self.terminate(MirTerminator::Goto {
            target: enter,
            arguments: vec![],
        })?;
        self.switch_to(enter);
        self.bindings.push(HashMap::new());
        if limit.is_some() {
            // @parallel keeps the batch coordinator for bounded claiming; the
            // input is a List by the checked-for diagnostic.
            let bound = if let Some(limit) = limit {
                let Some(value) = self.lower_value(limit, function_names, types)? else {
                    return Ok(None);
                };
                value
            } else {
                unreachable!("parallel for always has a limit")
            };
            let Type::List(input_id) = iterable.ty else {
                unreachable!("checked parallel List input")
            };
            let element = types.list_type(input_id);
            let coordinator = self.for_call(
                Type::Batch,
                RuntimeIntrinsic::BatchNew(element),
                vec![input, bound],
            );
            self.for_bind(&state, coordinator);
            let worker = CoreExpr {
                id: body.id,
                ty: Type::Unit,
                kind: CoreExprKind::ForWorker {
                    index: index.map(str::to_owned),
                    item: item.to_owned(),
                    input_type: iterable.ty,
                    state: state.clone(),
                    body: Box::new(body.clone()),
                    collect: collecting,
                },
            };
            // Repeated workers cannot each consume the same unique capture.
            for (name, _) in crate::hir::task_captures(&worker) {
                if let Some(local) = self.resolve_local(&name) {
                    if matches!(
                        self.locals[local.0].ownership,
                        MirOwnership::Owned | MirOwnership::Borrowed
                    ) {
                        return Err(Diagnostic::codegen(format!("parallel for cannot capture owned or borrowed local '{name}'; create it inside the iteration or use a Cown")));
                    }
                }
            }
            let scope = self.new_task_scope();
            self.push_statement(MirStatement::ScopeEnter {
                scope,
                region: false,
            });
            self.task_scopes.push(scope);
            let head = self.new_block();
            let spawn = self.new_block();
            let drain = self.new_block();
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: vec![],
            })?;
            self.switch_to(head);
            self.lower_task_poll()?;
            let state_value = self.for_state(&state);
            let condition =
                self.for_call(Type::Bool, RuntimeIntrinsic::BatchWorker, vec![state_value]);
            self.terminate(MirTerminator::Branch {
                condition,
                then_block: spawn,
                else_block: drain,
            })?;
            self.switch_to(spawn);
            self.lower_task_create(body.id, &worker, scope, function_names, types)?;
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: vec![],
            })?;
            self.switch_to(drain);
            let complete = self.new_block();
            self.lower_task_failure_check(scope, complete, types)?;
            self.switch_to(complete);
            self.push_statement(MirStatement::ScopeExit { scope });
            self.task_scopes.pop();
        } else {
            if collecting {
                // Sequential loops collect through the dedicated builder; loop
                // state is cursor-local based and no coordinator claims input.
                let builder = self.for_call(Type::SeqBuilder, RuntimeIntrinsic::SeqNew, vec![]);
                self.for_bind(&state, builder);
            }
            self.for_bind("<for-cursor>", input);
            self.lower_cursor_iterations(
                index,
                item,
                cursor_ty,
                &state,
                body,
                collecting,
                function_names,
                types,
            )?;
        }
        if !self.is_open() {
            self.blocks[exit.0].terminator = Some(MirTerminator::Unreachable);
            self.bindings.pop();
            return Ok(None);
        }
        let result = if collecting {
            let output_id = output_id.expect("collecting for has a List type");
            let state_value = self.for_state(&state);
            let state_local = self.resolve_local(&state).expect("iteration state binding");
            self.for_call(
                expression.ty,
                match self.locals[state_local.0].ty {
                    Type::Batch => RuntimeIntrinsic::BatchFinish(types.list_type(output_id)),
                    Type::SeqBuilder => RuntimeIntrinsic::SeqFinish(types.list_type(output_id)),
                    other => unreachable!("unexpected iteration state {other:?}"),
                },
                vec![state_value],
            )
        } else if limit.is_some() {
            let state_value = self.for_state(&state);
            let finished = self.for_call(
                Type::List(types.list_id(Type::Unit).expect("parallel unit for")),
                RuntimeIntrinsic::BatchFinish(Type::Unit),
                vec![state_value],
            );
            self.discard_value(Some(finished));
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Unit { destination });
            destination
        } else {
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Unit { destination });
            destination
        };
        let from = self.current;
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![result],
        })?;
        self.bindings.pop();
        self.switch_to(exit);
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(from, result)],
        });
        Ok(Some(destination))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for_iterations(
        &mut self,
        index: Option<&str>,
        item: &str,
        input_type: Type,
        state: &str,
        body: &CoreExpr,
        collect: bool,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        let Type::List(id) = input_type else {
            return self.lower_cursor_iterations(
                index,
                item,
                input_type,
                state,
                body,
                collect,
                function_names,
                types,
            );
        };
        let element = types.list_type(id);
        let head = self.new_block();
        let claim = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(claim);
        self.terminate(MirTerminator::Goto {
            target: head,
            arguments: vec![],
        })?;
        self.switch_to(head);
        self.lower_task_poll()?;
        self.terminate(MirTerminator::Goto {
            target: claim,
            arguments: vec![],
        })?;
        self.switch_to(claim);
        self.bindings.push(HashMap::new());
        let state_value = self.for_state(state);
        let pair = self.for_call(
            Type::Tuple(
                types
                    .tuple_id(&[Type::U64, input_type])
                    .expect("iteration tuple"),
            ),
            RuntimeIntrinsic::BatchNext(element),
            vec![state_value],
        );
        let position = self.next_value(Type::U64);
        self.push_statement(MirStatement::Project {
            destination: position,
            base: pair,
            access: MirFieldAccess::Index(0),
        });
        let node = self.next_value(input_type);
        self.push_statement(MirStatement::Project {
            destination: node,
            base: pair,
            access: MirFieldAccess::Index(1),
        });
        let retained = self.next_value(input_type);
        self.push_statement(MirStatement::Dup {
            destination: retained,
            value: node,
        });
        let empty = self.for_call(Type::Bool, RuntimeIntrinsic::ListIsEmpty, vec![retained]);
        // Bind the owned pair so exhaustion and all control-flow exits release it.
        self.for_bind("<for-item-pair>", pair);
        let body_block = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: empty,
            then_block: exit,
            else_block: body_block,
        })?;
        self.switch_to(body_block);
        let retained = self.next_value(input_type);
        self.push_statement(MirStatement::Dup {
            destination: retained,
            value: node,
        });
        let option = self.for_call(
            Type::Option(types.option_id(element).expect("iteration Option")),
            RuntimeIntrinsic::ListHead(element),
            vec![retained],
        );
        let projected = self.next_value(element);
        self.push_statement(MirStatement::EnumProject {
            destination: projected,
            value: option,
            variant: 0,
            field: 0,
        });
        let value = if types.is_shared(element) {
            let destination = self.next_value(element);
            self.push_statement(MirStatement::Dup {
                destination,
                value: projected,
            });
            destination
        } else {
            projected
        };
        self.discard_value(Some(option));
        // Release the cursor node before user code can suspend; otherwise a
        // slow early item retains the whole unprocessed input suffix.
        let local = self.resolve_local("<for-item-pair>").unwrap();
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::DropLocal { destination, local });
        self.lower_for_body(
            index,
            item,
            position,
            value,
            head,
            exit,
            state,
            body,
            collect,
            function_names,
            types,
        )?;
        self.bindings.pop();
        self.switch_to(exit);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for_body(
        &mut self,
        index: Option<&str>,
        item: &str,
        position: MirValueId,
        value: MirValueId,
        head: MirBlockId,
        exit: MirBlockId,
        state: &str,
        body: &CoreExpr,
        collect: bool,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        self.for_bind(item, value);
        self.for_bind("<for-position>", position);
        if let Some(index) = index {
            self.for_bind(index, position);
        }
        let iteration_scope = if crate::hir::iteration_has_branches(body) {
            let scope = self.new_task_scope();
            self.push_statement(MirStatement::ScopeEnter {
                scope,
                region: false,
            });
            self.task_scopes.push(scope);
            Some(scope)
        } else {
            None
        };
        let (next_iteration, break_iteration) = if iteration_scope.is_some() {
            (self.new_block(), self.new_block())
        } else {
            (head, exit)
        };
        self.loops.push(LoopFrame {
            continue_block: next_iteration,
            break_block: break_iteration,
            breaks: vec![],
        });
        self.lower_task_poll()?;
        let value = self.lower_value(body, function_names, types)?;
        if self.is_open() {
            if collect {
                let value = value.unwrap_or_else(|| {
                    let destination = self.next_value(Type::Unit);
                    self.push_statement(MirStatement::Unit { destination });
                    destination
                });
                let result_type =
                    Type::List(types.list_id(body.ty).expect("iteration result List"));
                let tail = self.for_call(result_type, RuntimeIntrinsic::ListEmpty(body.ty), vec![]);
                let singleton = self.for_call(
                    result_type,
                    RuntimeIntrinsic::ListCons(body.ty),
                    vec![value, tail],
                );
                let state_value = self.for_state(state);
                let state_local = self.resolve_local(state).expect("iteration state binding");
                // The batch coordinator tags results with their input index for
                // the final sort; the sequential builder links nodes in order.
                match self.locals[state_local.0].ty {
                    Type::Batch => {
                        let local = self.resolve_local("<for-position>").unwrap();
                        let position = self.next_value(Type::U64);
                        self.push_statement(MirStatement::Read {
                            destination: position,
                            local,
                        });
                        self.for_call(
                            Type::Unit,
                            RuntimeIntrinsic::BatchPush(body.ty),
                            vec![state_value, position, singleton],
                        );
                    }
                    Type::SeqBuilder => {
                        self.for_call(
                            Type::Unit,
                            RuntimeIntrinsic::SeqPush(body.ty),
                            vec![state_value, singleton],
                        );
                    }
                    other => unreachable!("unexpected iteration state {other:?}"),
                }
            } else {
                self.discard_value(value);
            }
            self.terminate(MirTerminator::Goto {
                target: next_iteration,
                arguments: vec![],
            })?;
        }
        let loop_frame = self.loops.pop().expect("iteration loop frame");
        if let Some(scope) = iteration_scope {
            self.switch_to(next_iteration);
            let has_next = self.blocks.iter().any(|block| {
                matches!(block.terminator, Some(MirTerminator::Goto { target, .. }) if target == next_iteration)
            });
            if has_next {
                let complete = self.new_block();
                self.lower_task_failure_check(scope, complete, types)?;
                self.switch_to(complete);
                self.push_statement(MirStatement::ScopeExit { scope });
                self.terminate(MirTerminator::Goto {
                    target: head,
                    arguments: vec![],
                })?;
            } else {
                self.terminate(MirTerminator::Unreachable)?;
            }
            self.switch_to(break_iteration);
            if loop_frame.breaks.is_empty() {
                self.terminate(MirTerminator::Unreachable)?;
            } else {
                let complete = self.new_block();
                self.lower_task_failure_check(scope, complete, types)?;
                self.switch_to(complete);
                self.push_statement(MirStatement::ScopeExit { scope });
                self.terminate(MirTerminator::Goto {
                    target: exit,
                    arguments: vec![],
                })?;
            }
            self.task_scopes.pop();
        }
        Ok(())
    }
}
