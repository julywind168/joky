//! Iterative List formatting, including user-defined Debug on each element.
use super::super::*;
use crate::hir::CoreExpr;

impl Lowerer<'_> {
    pub(super) fn lower_debug_list(
        &mut self,
        receiver: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Type::List(id) = receiver.ty else {
            unreachable!()
        };
        let element = types.list_type(id);
        let enter = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(enter);
        self.terminate(MirTerminator::Goto {
            target: enter,
            arguments: vec![],
        })?;
        self.switch_to(enter);
        let Some(list) = self.lower_value(receiver, functions, types)? else {
            self.switch_to(exit);
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        };
        let opening = self.debug_text("List#{");
        let separator = self.debug_text("");
        let start = self.current;
        let header = self.new_block();
        let body = self.new_block();
        let done = self.new_block();
        let initial = [list, opening, separator];
        self.terminate(MirTerminator::Goto {
            target: header,
            arguments: initial.to_vec(),
        })?;
        self.switch_to(header);
        let cursors = [
            self.next_value(receiver.ty),
            self.next_value(Type::String),
            self.next_value(Type::String),
        ];
        for (destination, value) in cursors.into_iter().zip(initial) {
            self.push_statement(MirStatement::Phi {
                destination,
                incoming: vec![(start, value)],
            });
        }
        let [cursor, text, separator] = cursors.map(|value| self.debug_list_bind(value));
        let value = self.debug_list_copy(cursor);
        let empty = self.debug_intrinsic(RuntimeIntrinsic::ListIsEmpty, Type::Bool, vec![value]);
        self.terminate(MirTerminator::Branch {
            condition: empty,
            then_block: done,
            else_block: body,
        })?;
        self.switch_to(body);
        let value = self.debug_list_copy(cursor);
        let option = Type::Option(types.option_id(element).expect("List Debug head type"));
        let head = self.debug_intrinsic(RuntimeIntrinsic::ListHead(element), option, vec![value]);
        let head = self.debug_list_bind(head);
        let field = self.debug_list_project(head, element);
        if let Some(displayed) = self.debug_projected_value(field, receiver.id, functions, types)? {
            // The projected element borrows the head wrapper only through Debug.
            self.debug_list_drop(head);
            let previous = self.debug_list_copy(text);
            let between = self.debug_list_copy(separator);
            let joined = self.debug_concat(previous, between);
            let joined = self.debug_concat(joined, displayed);
            let between = self.debug_text(", ");
            let value = self.debug_list_copy(cursor);
            let option = Type::Option(types.option_id(receiver.ty).expect("List Debug tail type"));
            let tail =
                self.debug_intrinsic(RuntimeIntrinsic::ListTail(element), option, vec![value]);
            let tail = self.debug_list_bind(tail);
            let projected = self.debug_list_project(tail, receiver.ty);
            let next = self.next_value(receiver.ty);
            self.push_statement(MirStatement::Dup {
                destination: next,
                value: projected,
            });
            for local in [tail, cursor, text, separator] {
                self.debug_list_drop(local);
            }
            let from = self.current;
            let arguments = [next, joined, between];
            self.terminate(MirTerminator::Goto {
                target: header,
                arguments: arguments.to_vec(),
            })?;
            for statement in &mut self.blocks[header.0].statements {
                if let MirStatement::Phi {
                    destination,
                    incoming,
                } = statement
                {
                    if let Some(index) = cursors.iter().position(|cursor| cursor == destination) {
                        incoming.push((from, arguments[index]));
                    }
                }
            }
        }
        self.switch_to(done);
        let text = self.debug_list_copy(text);
        let closing = self.debug_text("}");
        let text = self.debug_concat(text, closing);
        self.terminate(MirTerminator::Goto {
            target: exit,
            arguments: vec![text],
        })?;
        self.switch_to(exit);
        let destination = self.next_value(Type::String);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(done, text)],
        });
        Ok(Some(destination))
    }

    fn debug_list_bind(&mut self, value: MirValueId) -> MirLocalId {
        let local = self.new_local("@debug/list", self.value_types[value.0]);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination,
            local,
            value: Some(value),
        });
        local
    }

    fn debug_list_copy(&mut self, local: MirLocalId) -> MirValueId {
        let value = self.next_value(self.locals[local.0].ty);
        self.push_statement(MirStatement::Read {
            destination: value,
            local,
        });
        let destination = self.next_value(self.locals[local.0].ty);
        self.push_statement(MirStatement::Dup { destination, value });
        destination
    }

    fn debug_list_project(&mut self, local: MirLocalId, ty: Type) -> MirValueId {
        let value = self.next_value_with_ownership(self.locals[local.0].ty, MirOwnership::Borrowed);
        self.push_statement(MirStatement::Read {
            destination: value,
            local,
        });
        let destination = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
        self.push_statement(MirStatement::EnumProject {
            destination,
            value,
            variant: 0,
            field: 0,
        });
        destination
    }

    fn debug_list_drop(&mut self, local: MirLocalId) {
        if self.locals[local.0].ownership == MirOwnership::Shared {
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::DropLocal { destination, local });
        }
    }
}
