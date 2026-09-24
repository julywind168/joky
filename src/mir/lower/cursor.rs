//! Consumptive Cursor calls and their iteration state.
use super::*;

impl Lowerer<'_> {
    fn take_cursor_local(&mut self, local: MirLocalId) -> MirValueId {
        let ty = self.locals[local.0].ty;
        let value = self.next_value(ty);
        // Shared and Owned slots transfer wholesale; codegen moves the slot
        // without touching the reference count. Copy and borrowed cursors are
        // read out, leaving the slot live for later exits.
        match self.locals[local.0].ownership {
            MirOwnership::Owned | MirOwnership::Shared => {
                self.push_statement(MirStatement::TakeLocal {
                    destination: value,
                    local,
                });
            }
            _ => {
                self.push_statement(MirStatement::Read {
                    destination: value,
                    local,
                });
            }
        }
        value
    }

    fn cursor_deinit(&mut self, value: MirValueId, variant: Option<usize>) {
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Deinit {
            destination,
            value,
            variant,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_cursor_iterations(
        &mut self,
        index: Option<&str>,
        item: &str,
        input: Type,
        state: &str,
        body: &CoreExpr,
        collect: bool,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<(), Diagnostic> {
        let element = types.cursor_item_type(input).expect("Cursor Item");
        let pair_type = Type::Tuple(types.tuple_id(&[element, input]).expect("Cursor pair"));
        let option_type = Type::Option(types.option_id(pair_type).expect("Cursor result"));
        let cursor = self.resolve_local("<for-cursor>").expect("Cursor local");
        // Loop state lives in ordinary locals; local_ssa::normalize synthesizes
        // the loop-carried phis and edge transfers once the CFG is complete.
        let position = self.new_local("<for-cursor-position>", Type::U64);
        let initial_position = self.next_value(Type::U64);
        self.push_statement(MirStatement::Const {
            destination: initial_position,
            value: MirConstant::Integer(0),
        });
        let bound = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination: bound,
            local: position,
            value: Some(initial_position),
        });
        let head = self.new_block();
        let claim = self.new_block();
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
        let receiver = self.take_cursor_local(cursor);
        let destination = if let Some(symbol) = types
            .interface
            .method_symbols
            .get(&(input, crate::sema::CURSOR_METHOD.into()))
        {
            let destination = self.next_value(option_type);
            let function = self.function_ids[&crate::module::symbol_key(symbol)];
            self.push_statement(MirStatement::Call {
                destination,
                function,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }],
                continuation: None,
            });
            destination
        } else if input == Type::BytesCursor {
            // The builtin byte cursor advances through a runtime call instead
            // of a user method; the receiver reference is adopted by the callee.
            let destination = self.next_value(option_type);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: RuntimeIntrinsic::BytesCursorAdvance,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }],
            });
            destination
        } else if matches!(
            input,
            Type::MapCursor(_) | Type::MapKeyCursor(_) | Type::MapValueCursor(_)
        ) {
            // Builtin map cursors advance through a runtime call; the receiver
            // reference is adopted by the callee.
            let map_id = match input {
                Type::MapCursor(id) | Type::MapKeyCursor(id) | Type::MapValueCursor(id) => id,
                _ => unreachable!(),
            };
            let info = types.map_info(map_id);
            let destination = self.next_value(option_type);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: RuntimeIntrinsic::MapCursorAdvance(info.key, info.value),
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }],
            });
            destination
        } else if let Type::MutListCursor(id) = input {
            let destination = self.next_value(option_type);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: RuntimeIntrinsic::MutListCursorAdvance(types.list_type(id)),
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }],
            });
            destination
        } else if matches!(input, Type::MutMapCursor(_) | Type::MutSetCursor(_)) {
            let map_id = match input {
                Type::MutMapCursor(id) | Type::MutSetCursor(id) => id,
                _ => unreachable!(),
            };
            let info = types.map_info(map_id);
            let destination = self.next_value(option_type);
            self.push_statement(MirStatement::RuntimeCall {
                destination,
                intrinsic: if matches!(input, Type::MutMapCursor(_)) {
                    RuntimeIntrinsic::MutMapCursorAdvance(info.key, info.value)
                } else {
                    RuntimeIntrinsic::MutSetCursorAdvance(info.key)
                },
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: receiver,
                }],
            });
            destination
        } else if types.range_item(input).is_some() {
            self.range_advance_from_value(receiver, input, element, pair_type, option_type)?
        } else if let Type::List(list_id) = input {
            // Lists are their own cursor: advance inlines the head/tail split
            // on the taken receiver value.
            let element_type = types.list_type(list_id);
            self.list_advance_from_value(
                receiver,
                input,
                element_type,
                pair_type,
                option_type,
                types,
            )?
        } else {
            let Some(value) = self.lower_method_call_values(
                receiver,
                crate::sema::CURSOR_METHOD,
                option_type,
                vec![],
                function_names,
                types,
            )?
            else {
                self.bindings.pop();
                return Ok(());
            };
            value
        };
        let exhausted = self.new_block();
        let unpack = self.new_block();
        let break_cleanup = self.new_block();
        let advance = self.new_block();
        let exit = self.new_block();
        self.blocks[unpack.0].scope_depth = self.blocks[claim.0].scope_depth;
        self.blocks[exhausted.0].scope_depth = self.blocks[claim.0].scope_depth;
        let loop_depth = self.blocks[head.0].scope_depth;
        for block in [break_cleanup, advance, exit] {
            self.blocks[block.0].scope_depth = loop_depth;
        }
        let option = self.lower_task_poll_result(destination)?;
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value: option,
        });
        let zero = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination: zero,
            value: MirConstant::Integer(0),
        });
        let present = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: present,
            op: BinaryOp::Equal,
            left: tag,
            right: zero,
        });
        self.terminate(MirTerminator::Branch {
            condition: present,
            then_block: unpack,
            else_block: exhausted,
        })?;
        self.switch_to(exhausted);
        if types.is_owned(option_type) {
            self.cursor_deinit(option, Some(1));
        } else {
            self.discard_value(Some(option));
        }
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![],
        })?;

        self.switch_to(unpack);
        let pair = self.next_value(pair_type);
        self.push_statement(MirStatement::EnumProject {
            destination: pair,
            value: option,
            variant: 0,
            field: 0,
        });
        let mut fields = Vec::new();
        for (index, ty) in [element, input].into_iter().enumerate() {
            let field = self.next_value(ty);
            self.push_statement(MirStatement::Project {
                destination: field,
                base: pair,
                access: MirFieldAccess::Index(index),
            });
            let field = if types.is_shared(option_type) && types.is_shared(ty) {
                let retained = self.next_value(ty);
                self.push_statement(MirStatement::Dup {
                    destination: retained,
                    value: field,
                });
                retained
            } else {
                field
            };
            fields.push(field);
        }
        // End both move protocols before any poll or control-flow edge.
        if types.is_owned(option_type) {
            self.cursor_deinit(pair, None);
            self.cursor_deinit(option, Some(0));
        } else {
            self.discard_value(Some(option));
        }
        let bound = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination: bound,
            local: cursor,
            value: Some(fields[1]),
        });
        let current_position = self.next_value(Type::U64);
        self.push_statement(MirStatement::Read {
            destination: current_position,
            local: position,
        });
        let one = self.next_value(Type::U64);
        self.push_statement(MirStatement::Const {
            destination: one,
            value: MirConstant::Integer(1),
        });
        let next_position = self.next_value(Type::U64);
        self.push_statement(MirStatement::Binary {
            destination: next_position,
            op: BinaryOp::Add,
            left: current_position,
            right: one,
        });
        let bound = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination: bound,
            local: position,
            value: Some(next_position),
        });
        self.lower_for_body(
            index,
            item,
            current_position,
            fields[0],
            advance,
            break_cleanup,
            state,
            body,
            collect,
            function_names,
            types,
        )?;
        self.switch_to(advance);
        let has_backedge = self.blocks.iter().any(|block| {
            matches!(block.terminator, Some(MirTerminator::Goto { target, .. }) if target == advance)
        });
        if has_backedge {
            // The next cursor is already bound into the cursor local; the
            // backedge carries no block arguments.
            self.terminate(MirTerminator::Goto {
                target: head,
                arguments: vec![],
            })?;
        } else {
            self.terminate(MirTerminator::Unreachable)?;
        }
        self.switch_to(break_cleanup);
        let has_break = self.blocks.iter().any(|block| {
            matches!(block.terminator, Some(MirTerminator::Goto { target, .. }) if target == break_cleanup)
        });
        if has_break {
            // The unpacked successor is still held by the cursor local here.
            if matches!(
                self.locals[cursor.0].ownership,
                MirOwnership::Owned | MirOwnership::Shared
            ) {
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::DropLocal {
                    destination,
                    local: cursor,
                });
            }
            self.terminate(MirTerminator::Goto {
                target: exit,
                arguments: vec![],
            })?;
        } else {
            self.terminate(MirTerminator::Unreachable)?;
        }
        self.bindings.pop();
        self.switch_to(exit);
        Ok(())
    }

    /// Standalone `cursor.advance()` on the builtin byte cursor. Consumes the
    /// receiver value; the runtime adopts its reference.
    pub(super) fn lower_bytes_cursor_advance(
        &mut self,
        expression: &CoreExpr,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::BytesCursorAdvance,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: receiver,
            }],
        });
        Ok(Some(destination))
    }

    /// Standalone `cursor.advance()` on a builtin map cursor.
    pub(super) fn lower_map_cursor_advance(
        &mut self,
        expression: &CoreExpr,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let map_id = match receiver.ty {
            Type::MapCursor(id) | Type::MapKeyCursor(id) | Type::MapValueCursor(id) => id,
            _ => unreachable!("map cursor advance guard"),
        };
        let info = types.map_info(map_id);
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::MapCursorAdvance(info.key, info.value),
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: receiver,
            }],
        });
        Ok(Some(destination))
    }

    /// Standalone `cursor.advance()` on a builtin MutList/MutMap/MutSet cursor.
    pub(super) fn lower_mut_cursor_advance(
        &mut self,
        expression: &CoreExpr,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(receiver_value) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let intrinsic = match receiver.ty {
            Type::MutListCursor(id) => RuntimeIntrinsic::MutListCursorAdvance(types.list_type(id)),
            Type::MutMapCursor(id) => {
                let info = types.map_info(id);
                RuntimeIntrinsic::MutMapCursorAdvance(info.key, info.value)
            }
            Type::MutSetCursor(id) => RuntimeIntrinsic::MutSetCursorAdvance(types.map_info(id).key),
            _ => unreachable!("mut cursor advance guard"),
        };
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: receiver_value,
            }],
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_list_advance(
        &mut self,
        expression: &CoreExpr,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(list) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let Type::List(id) = receiver.ty else {
            unreachable!()
        };
        let Type::Option(option_id) = expression.ty else {
            unreachable!()
        };
        let pair_type = types.option_type(option_id);
        let element = types.list_type(id);
        Ok(Some(self.list_advance_from_value(
            list,
            receiver.ty,
            element,
            pair_type,
            expression.ty,
            types,
        )?))
    }

    pub(super) fn lower_range_advance(
        &mut self,
        expression: &CoreExpr,
        receiver: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(receiver) = self.lower_value(receiver, function_names, types)? else {
            return Ok(None);
        };
        let input = self.value_types[receiver.0];
        let item = types.range_item(input).expect("range item");
        let Type::Option(option_id) = expression.ty else {
            return Err(Diagnostic::codegen("range advance result is not Option"));
        };
        let pair = types.option_type(option_id);
        Ok(Some(self.range_advance_from_value(
            receiver,
            input,
            item,
            pair,
            expression.ty,
        )?))
    }

    fn range_advance_from_value(
        &mut self,
        receiver: MirValueId,
        input: Type,
        item: Type,
        pair_type: Type,
        option_type: Type,
    ) -> Result<MirValueId, Diagnostic> {
        let Type::Struct(struct_id) = input else {
            return Err(Diagnostic::codegen("invalid range type"));
        };
        let Type::Option(option_id) = option_type else {
            return Err(Diagnostic::codegen("invalid range Option"));
        };
        let project = |this: &mut Self, base: MirValueId, index: usize, ty: Type| {
            let destination = this.next_value(ty);
            this.push_statement(MirStatement::Project {
                destination,
                base,
                access: MirFieldAccess::Index(index),
            });
            destination
        };
        let current = project(self, receiver, 0, item);
        let end = project(self, receiver, 1, item);
        let step = project(self, receiver, 2, item);
        let inclusive = project(self, receiver, 3, Type::Bool);
        let exhausted = project(self, receiver, 4, Type::Bool);
        let zero = self.next_value(item);
        self.push_statement(MirStatement::Const {
            destination: zero,
            value: MirConstant::Integer(0),
        });
        let positive = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: positive,
            op: BinaryOp::Greater,
            left: step,
            right: zero,
        });
        let present = self.new_block();
        let none = self.new_block();
        let exit = self.new_block();
        let bounds = self.new_block();
        let equal_bound = self.new_block();
        let ordered_bound = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: exhausted,
            then_block: none,
            else_block: bounds,
        })?;
        self.switch_to(bounds);
        let equal = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: equal,
            op: BinaryOp::Equal,
            left: current,
            right: end,
        });
        self.terminate(MirTerminator::Branch {
            condition: equal,
            then_block: equal_bound,
            else_block: ordered_bound,
        })?;
        self.switch_to(equal_bound);
        self.terminate(MirTerminator::Branch {
            condition: inclusive,
            then_block: present,
            else_block: none,
        })?;
        self.switch_to(ordered_bound);
        let ascending = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: ascending,
            op: BinaryOp::Less,
            left: current,
            right: end,
        });
        let valid = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: valid,
            op: BinaryOp::Equal,
            left: positive,
            right: ascending,
        });
        self.terminate(MirTerminator::Branch {
            condition: valid,
            then_block: present,
            else_block: none,
        })?;
        self.switch_to(none);
        let none_value = self.next_value(option_type);
        self.push_statement(MirStatement::EnumConstruct {
            destination: none_value,
            enum_id: MirTypeId::Option(option_id),
            variant: 1,
            arguments: vec![],
        });
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![none_value],
        })?;
        self.switch_to(present);
        // Mark a wrapped successor exhausted before it can be observed.
        let next = self.next_value(item);
        self.push_statement(MirStatement::Binary {
            destination: next,
            op: BinaryOp::Add,
            left: current,
            right: step,
        });
        let wrapped_up = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: wrapped_up,
            op: BinaryOp::Less,
            left: next,
            right: current,
        });
        let wrapped = self.next_value(Type::Bool);
        // A nonzero step cannot preserve current, so overflow is exactly when
        // the wrapped result moves against the step's direction.
        self.push_statement(MirStatement::Binary {
            destination: wrapped,
            op: BinaryOp::Equal,
            left: positive,
            right: wrapped_up,
        });
        let next_range = self.next_value(input);
        self.push_statement(MirStatement::Construct {
            destination: next_range,
            type_id: MirTypeId::Struct(struct_id),
            fields: vec![next, end, step, inclusive, wrapped],
        });
        let pair = self.next_value(pair_type);
        self.push_statement(MirStatement::Tuple {
            destination: pair,
            elements: vec![current, next_range],
        });
        let some_value = self.next_value(option_type);
        self.push_statement(MirStatement::EnumConstruct {
            destination: some_value,
            enum_id: MirTypeId::Option(option_id),
            variant: 0,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: pair,
            }],
        });
        let some_from = self.current;
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![some_value],
        })?;
        self.switch_to(exit);
        let destination = self.next_value(option_type);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(none, none_value), (some_from, some_value)],
        });
        Ok(destination)
    }

    /// Inlines `Cursor.advance` for a List receiver value: `None` when the
    /// remaining list is empty, otherwise `Some((head, tail))`.
    fn list_advance_from_value(
        &mut self,
        list: MirValueId,
        list_type: Type,
        element: Type,
        pair_type: Type,
        option_type: Type,
        types: &CheckedTypes,
    ) -> Result<MirValueId, Diagnostic> {
        let Type::Option(option_id) = option_type else {
            unreachable!("list advance returns an Option")
        };
        let retained = self.next_value(list_type);
        self.push_statement(MirStatement::Dup {
            destination: retained,
            value: list,
        });
        let empty = self.for_call(Type::Bool, RuntimeIntrinsic::ListIsEmpty, vec![retained]);
        let none = self.new_block();
        let some = self.new_block();
        let exit = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: empty,
            then_block: none,
            else_block: some,
        })?;
        self.switch_to(none);
        self.discard_value(Some(list));
        let none_value = self.next_value(option_type);
        self.push_statement(MirStatement::EnumConstruct {
            destination: none_value,
            enum_id: MirTypeId::Option(option_id),
            variant: 1,
            arguments: vec![],
        });
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![none_value],
        })?;

        self.switch_to(some);
        let retained = self.next_value(list_type);
        self.push_statement(MirStatement::Dup {
            destination: retained,
            value: list,
        });
        let head = self.for_call(
            Type::Option(types.option_id(element).expect("List head")),
            RuntimeIntrinsic::ListHead(element),
            vec![retained],
        );
        let tail = self.for_call(
            Type::Option(types.option_id(list_type).expect("List tail")),
            RuntimeIntrinsic::ListTail(element),
            vec![list],
        );
        let mut fields = Vec::new();
        for (wrapped, ty) in [(head, element), (tail, list_type)] {
            let field = self.next_value(ty);
            self.push_statement(MirStatement::EnumProject {
                destination: field,
                value: wrapped,
                variant: 0,
                field: 0,
            });
            let field = if types.is_shared(ty) {
                let retained = self.next_value(ty);
                self.push_statement(MirStatement::Dup {
                    destination: retained,
                    value: field,
                });
                retained
            } else {
                field
            };
            fields.push(field);
            self.discard_value(Some(wrapped));
        }
        let pair = self.next_value(pair_type);
        self.push_statement(MirStatement::Tuple {
            destination: pair,
            elements: fields,
        });
        let some_value = self.next_value(option_type);
        self.push_statement(MirStatement::EnumConstruct {
            destination: some_value,
            enum_id: MirTypeId::Option(option_id),
            variant: 0,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value: pair,
            }],
        });
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![some_value],
        })?;
        self.switch_to(exit);
        let destination = self.next_value(option_type);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(none, none_value), (some, some_value)],
        });
        Ok(destination)
    }
}
