//! Lexicographic comparisons borrow operands and stop at the first non-equal result.
use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};
use crate::syntax::NodeId;

struct OrderingContext<'a> {
    partial: bool,
    source: NodeId,
    functions: &'a HashSet<String>,
    types: &'a CheckedTypes,
}

impl OrderingContext<'_> {
    fn result_type(&self) -> Type {
        if self.partial {
            self.types.partial_ordering_type()
        } else {
            self.types.ordering_type()
        }
    }
}

impl Lowerer<'_> {
    pub(super) fn lower_aggregate_ordering(
        &mut self,
        left: &CoreExpr,
        right: &CoreExpr,
        partial: bool,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let context = OrderingContext {
            partial,
            source: left.id,
            functions,
            types,
        };
        let enter = self.new_block();
        let exit = self.new_block();
        self.mark_scoped(enter);
        self.terminate(MirTerminator::Goto {
            target: enter,
            arguments: vec![],
        })?;
        self.switch_to(enter);
        self.bindings.push(HashMap::new());
        let result = self.lower_ordering_operands(left, right, &context)?;
        self.bindings.pop();
        let from = self.current;
        if let Some(value) = result {
            self.terminate(MirTerminator::Goto {
                target: exit,
                arguments: vec![value],
            })?;
            self.switch_to(exit);
            Ok(Some(self.ordering_phi(vec![(from, value)], &context)))
        } else {
            self.switch_to(exit);
            self.terminate(MirTerminator::Unreachable)?;
            Ok(None)
        }
    }

    fn lower_ordering_operands(
        &mut self,
        left: &CoreExpr,
        right: &CoreExpr,
        context: &OrderingContext<'_>,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(value) = self.lower_borrowed_value(left, context.functions, context.types)? else {
            return Ok(None);
        };
        let lhs = self.comparison_bind(value);
        let Some(value) = self.lower_borrowed_value(right, context.functions, context.types)?
        else {
            return Ok(None);
        };
        let rhs = self.comparison_bind(value);
        match left.ty {
            Type::Tuple(id) => {
                let done = self.new_block();
                let mut incoming = Vec::new();
                for (index, ty) in context.types.tuple_elements(id).iter().copied().enumerate() {
                    let a = self.comparison_project(lhs, ty, None, index, context.types);
                    let b = self.comparison_project(rhs, ty, None, index, context.types);
                    let Some(order) = self.ordering_compare(a, b, context)? else {
                        // Earlier members may already have produced a result.
                        self.switch_to(done);
                        if incoming.is_empty() {
                            self.terminate(MirTerminator::Unreachable)?;
                            return Ok(None);
                        }
                        return Ok(Some(self.ordering_phi(incoming, context)));
                    };
                    self.ordering_continue_if_equal(order, done, &mut incoming, context)?;
                }
                self.ordering_finish(1, done, &mut incoming, context)?;
                self.switch_to(done);
                Ok(Some(self.ordering_phi(incoming, context)))
            }
            Type::Option(_) | Type::Result(_) => self.lower_variant_ordering([lhs, rhs], context),
            Type::List(id) => {
                self.lower_list_ordering([lhs, rhs], context.types.list_type(id), context)
            }
            _ => unreachable!("aggregate ordering type checked"),
        }
    }

    fn lower_variant_ordering(
        &mut self,
        locals: [MirLocalId; 2],
        context: &OrderingContext<'_>,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let ty = self.locals[locals[0].0].ty;
        let [left, right] = locals.map(|local| {
            let value = self.comparison_borrow(local, context.types);
            self.ordering_tag(value)
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
        // Option stores Some as tag 0, but None sorts first. Result's Ok tag 0 sorts first.
        let (left, right) = if matches!(ty, Type::Option(_)) {
            (right, left)
        } else {
            (left, right)
        };
        let destination = self.next_value(context.result_type());
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: if context.partial {
                RuntimeIntrinsic::PartialCompare(Type::I32)
            } else {
                RuntimeIntrinsic::Compare(Type::I32)
            },
            arguments: vec![
                MirCallArgument {
                    parameter: 0,
                    value: left,
                },
                MirCallArgument {
                    parameter: 1,
                    value: right,
                },
            ],
        });
        let mut incoming = vec![(different, destination)];
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![destination],
        })?;

        self.switch_to(same);
        let zero = self.ordering_integer(0);
        let condition = self.comparison_scalar(left, zero);
        let first = self.new_block();
        let second = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: first,
            else_block: second,
        })?;
        let payloads = match ty {
            Type::Option(id) => [Some(context.types.option_type(id)), None],
            Type::Result(id) => {
                let (ok, err) = context.types.result_types(id);
                [Some(ok), Some(err)]
            }
            _ => unreachable!(),
        };
        for (variant, (block, payload)) in [first, second].into_iter().zip(payloads).enumerate() {
            self.switch_to(block);
            if let Some(ty) = payload {
                let a = self.comparison_project(locals[0], ty, Some(variant), 0, context.types);
                let b = self.comparison_project(locals[1], ty, Some(variant), 0, context.types);
                if let Some(value) = self.ordering_compare(a, b, context)? {
                    incoming.push((self.current, value));
                    self.terminate(MirTerminator::Goto {
                        target: done,
                        arguments: vec![value],
                    })?;
                }
            } else {
                self.ordering_finish(1, done, &mut incoming, context)?;
            }
        }
        self.switch_to(done);
        Ok(Some(self.ordering_phi(incoming, context)))
    }

    fn lower_list_ordering(
        &mut self,
        originals: [MirLocalId; 2],
        element: Type,
        context: &OrderingContext<'_>,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let types = context.types;
        let list = self.locals[originals[0].0].ty;
        let initial = originals.map(|local| self.comparison_copy(local));
        let start = self.current;
        let head = self.new_block();
        let done = self.new_block();
        self.terminate(MirTerminator::Goto {
            target: head,
            arguments: initial.to_vec(),
        })?;
        self.switch_to(head);
        let cursors = [self.next_value(list), self.next_value(list)];
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
        let less = self.new_block();
        let equal = self.new_block();
        let greater = self.new_block();
        let present = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition: empty[0],
            then_block: left_empty,
            else_block: left_present,
        })?;
        self.switch_to(left_empty);
        self.terminate(MirTerminator::Branch {
            condition: empty[1],
            then_block: equal,
            else_block: less,
        })?;
        self.switch_to(left_present);
        self.terminate(MirTerminator::Branch {
            condition: empty[1],
            then_block: greater,
            else_block: present,
        })?;
        let mut incoming = Vec::new();
        for (tag, block) in [less, equal, greater].into_iter().enumerate() {
            self.switch_to(block);
            self.ordering_finish(tag, done, &mut incoming, context)?;
        }
        self.switch_to(present);
        let option = Type::Option(types.option_id(element).expect("ordering head type"));
        let heads = locals.map(|local| {
            let value = self.comparison_copy(local);
            let value =
                self.comparison_intrinsic(RuntimeIntrinsic::ListHead(element), option, value);
            self.comparison_bind(value)
        });
        let a = self.comparison_project(heads[0], element, Some(0), 0, types);
        let b = self.comparison_project(heads[1], element, Some(0), 0, types);
        if let Some(order) = self.ordering_compare(a, b, context)? {
            for local in heads {
                self.comparison_drop_local(local);
            }
            self.ordering_continue_if_equal(order, done, &mut incoming, context)?;
            let advance = self.current;
            let option = Type::Option(types.option_id(list).expect("ordering tail type"));
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
        }
        self.switch_to(done);
        let destination = self.ordering_phi(incoming, context);
        for local in locals {
            self.comparison_drop_local(local);
        }
        Ok(Some(destination))
    }

    fn ordering_compare(
        &mut self,
        left: MirLocalId,
        right: MirLocalId,
        context: &OrderingContext<'_>,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let [left, right] = [left, right].map(|local| CoreExpr {
            id: context.source,
            ty: self.locals[local.0].ty,
            kind: CoreExprKind::Name(self.locals[local.0].name.clone()),
        });
        let method = if context.partial {
            crate::sema::PARTIAL_ORD_METHOD
        } else {
            crate::sema::ORD_METHOD
        };
        let mut arguments = vec![CoreCallArgument {
            label: Some("other".into()),
            value: right,
        }];
        let callee = if let Some(symbol) = context
            .types
            .interface
            .method_symbols
            .get(&(left.ty, method.into()))
        {
            arguments.insert(
                0,
                CoreCallArgument {
                    label: Some("self".into()),
                    value: left,
                },
            );
            CoreExprKind::ExternalSymbol(symbol.clone())
        } else {
            CoreExprKind::Field {
                value: Box::new(left),
                access: FieldAccess::Name(method.into()),
            }
        };
        self.lower_value(
            &CoreExpr {
                id: context.source,
                ty: context.result_type(),
                kind: CoreExprKind::Call {
                    callee: Box::new(CoreExpr {
                        id: context.source,
                        ty: Type::Unit,
                        kind: callee,
                    }),
                    type_arguments: vec![],
                    arguments,
                    effect_operation: None,
                },
            },
            context.functions,
            context.types,
        )
    }

    fn ordering_tag(&mut self, value: MirValueId) -> MirValueId {
        let destination = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag { destination, value });
        destination
    }

    fn ordering_continue_if_equal(
        &mut self,
        value: MirValueId,
        done: MirBlockId,
        incoming: &mut Vec<(MirBlockId, MirValueId)>,
        context: &OrderingContext<'_>,
    ) -> Result<(), Diagnostic> {
        let ordering = if context.partial {
            let tag = self.ordering_tag(value);
            let zero = self.ordering_integer(0);
            let condition = self.comparison_scalar(tag, zero);
            let some = self.new_block();
            let none = self.new_block();
            self.terminate(MirTerminator::Branch {
                condition,
                then_block: some,
                else_block: none,
            })?;
            self.switch_to(none);
            incoming.push((none, value));
            self.terminate(MirTerminator::Goto {
                target: done,
                arguments: vec![value],
            })?;
            self.switch_to(some);
            let destination = self.next_value(context.types.ordering_type());
            self.push_statement(MirStatement::EnumProject {
                destination,
                value,
                variant: 0,
                field: 0,
            });
            destination
        } else {
            value
        };
        let tag = self.ordering_tag(ordering);
        let equal = self.ordering_integer(1);
        let condition = self.comparison_scalar(tag, equal);
        let next = self.new_block();
        let different = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: next,
            else_block: different,
        })?;
        self.switch_to(different);
        incoming.push((different, value));
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![value],
        })?;
        self.switch_to(next);
        Ok(())
    }

    fn ordering_finish(
        &mut self,
        variant: usize,
        done: MirBlockId,
        incoming: &mut Vec<(MirBlockId, MirValueId)>,
        context: &OrderingContext<'_>,
    ) -> Result<(), Diagnostic> {
        let mut value = self.next_value(context.types.ordering_type());
        self.push_statement(MirStatement::EnumConstruct {
            destination: value,
            enum_id: MirTypeId::from_type(context.types.ordering_type()).expect("Ordering type"),
            variant,
            arguments: vec![],
        });
        if context.partial {
            let destination = self.next_value(context.result_type());
            self.push_statement(MirStatement::EnumConstruct {
                destination,
                enum_id: MirTypeId::from_type(context.result_type())
                    .expect("Option(Ordering) type"),
                variant: 0,
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value,
                }],
            });
            value = destination;
        }
        incoming.push((self.current, value));
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![value],
        })
    }

    fn ordering_phi(
        &mut self,
        incoming: Vec<(MirBlockId, MirValueId)>,
        context: &OrderingContext<'_>,
    ) -> MirValueId {
        let destination = self.next_value(context.result_type());
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        destination
    }
}
