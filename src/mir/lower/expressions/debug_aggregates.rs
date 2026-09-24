use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind};
use crate::syntax::NodeId;

impl Lowerer<'_> {
    pub(super) fn lower_debug_aggregate(
        &mut self,
        value: MirValueId,
        source: NodeId,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let ty = self.value_types[value.0];
        if let Type::Tuple(id) = ty {
            let elements = types.tuple_elements(id);
            // Reborrow each field separately so a projected tuple borrow is
            // not live across a user-defined Debug call on one of its fields.
            let local = if types.is_owned(ty) {
                let local =
                    self.new_local_with_ownership("@debug/tuple", ty, MirOwnership::Borrowed);
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(value),
                    destination,
                });
                Some(local)
            } else {
                None
            };
            let mut text = self.debug_text("(");
            for (index, ty) in elements.iter().copied().enumerate() {
                if index != 0 {
                    let separator = self.debug_text(", ");
                    text = self.debug_concat(text, separator);
                }
                let base = if let Some(local) = local {
                    let destination = self
                        .next_value_with_ownership(self.locals[local.0].ty, MirOwnership::Borrowed);
                    self.push_statement(MirStatement::BorrowLocal { destination, local });
                    destination
                } else {
                    value
                };
                let field = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
                self.push_statement(MirStatement::Project {
                    destination: field,
                    base,
                    access: MirFieldAccess::Index(index),
                });
                let Some(field) =
                    self.debug_projected_value(field, source, function_names, types)?
                else {
                    return Ok(None);
                };
                text = self.debug_concat(text, field);
            }
            let closing = self.debug_text(if elements.len() == 1 { ",)" } else { ")" });
            return Ok(Some(self.debug_concat(text, closing)));
        }

        let variants = match ty {
            Type::Option(id) => [("Some", Some(types.option_type(id))), ("None", None)],
            Type::Result(id) => {
                let (ok, err) = types.result_types(id);
                [("Ok", Some(ok)), ("Err", Some(err))]
            }
            _ => {
                return Err(Diagnostic::codegen(
                    "Debug expected a tuple, Option or Result",
                ))
            }
        };
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value,
        });
        let zero = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination: zero,
            value: MirConstant::Integer(0),
        });
        let condition = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination: condition,
            op: BinaryOp::Equal,
            left: tag,
            right: zero,
        });
        let first = self.new_block();
        let second = self.new_block();
        let merge = self.new_block();
        self.mark_scoped(first);
        self.mark_scoped(second);
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: first,
            else_block: second,
        })?;

        // Project only the active variant: the inactive payload may contain null
        // pointers or uninitialized words and must never reach a Debug method.
        let mut incoming = Vec::new();
        for (variant, ((name, payload), block)) in
            variants.into_iter().zip([first, second]).enumerate()
        {
            self.switch_to(block);
            let text = if let Some(ty) = payload {
                let field = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
                self.push_statement(MirStatement::EnumProject {
                    destination: field,
                    value,
                    variant,
                    field: 0,
                });
                let Some(field) =
                    self.debug_projected_value(field, source, function_names, types)?
                else {
                    continue;
                };
                let opening = self.debug_text(&format!("{name}("));
                let text = self.debug_concat(opening, field);
                let closing = self.debug_text(")");
                self.debug_concat(text, closing)
            } else {
                self.debug_text(name)
            };
            incoming.push((self.current, text));
            self.terminate(MirTerminator::Goto {
                target: merge,
                arguments: vec![text],
            })?;
        }
        self.switch_to(merge);
        if incoming.is_empty() {
            self.terminate(MirTerminator::Unreachable)?;
            return Ok(None);
        }
        let destination = self.next_value(Type::String);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    pub(super) fn debug_projected_value(
        &mut self,
        value: MirValueId,
        source: NodeId,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let ty = self.value_types[value.0];
        let name = format!("@debug/{}", self.locals.len());
        let local = self.new_local_with_ownership(&name, ty, MirOwnership::Borrowed);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            local,
            value: Some(value),
            destination,
        });
        self.bindings.push(HashMap::from([(name.clone(), local)]));
        let result = self.lower_debug(
            &CoreExpr {
                id: source,
                ty,
                kind: CoreExprKind::Name(name),
            },
            function_names,
            types,
        );
        self.bindings.pop();
        result
    }

    pub(super) fn debug_text(&mut self, text: &str) -> MirValueId {
        let destination = self.next_value(Type::String);
        self.push_statement(MirStatement::Const {
            destination,
            value: MirConstant::String(text.into()),
        });
        destination
    }

    pub(super) fn debug_concat(&mut self, left: MirValueId, right: MirValueId) -> MirValueId {
        let destination = self.next_value(Type::String);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic: RuntimeIntrinsic::StringConcat,
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
        destination
    }
}
