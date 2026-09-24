use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind};
use crate::syntax::NodeId;

impl Lowerer<'_> {
    pub(super) fn lower_default_debug_call(
        &mut self,
        receiver: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Some(value) = self.lower_borrowed_value(receiver, functions, types)? else {
            return Ok(None);
        };
        let borrowed = if self.value_ownership[value.0] == MirOwnership::Borrowed {
            value
        } else {
            let local = self.new_local_with_ownership(
                "@debug/receiver",
                receiver.ty,
                MirOwnership::Borrowed,
            );
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Bind {
                destination,
                local,
                value: Some(value),
            });
            self.debug_borrow_local(local, types)
        };
        let parent = if self.resolve_local("@debug/path").is_some() {
            self.lower_value(
                &CoreExpr {
                    id: receiver.id,
                    ty: Type::Bytes,
                    kind: CoreExprKind::Name("@debug/path".into()),
                },
                functions,
                types,
            )?
            .unwrap()
        } else {
            self.debug_intrinsic(RuntimeIntrinsic::BytesNew, Type::Bytes, vec![])
        };
        let destination = self.next_value(Type::String);
        let name = crate::hir::default_debug_name(receiver.ty);
        let function = *self
            .function_ids
            .get(&name)
            .ok_or_else(|| Diagnostic::codegen(format!("missing default Debug for {name}")))?;
        self.push_statement(MirStatement::Call {
            destination,
            function,
            arguments: vec![
                MirCallArgument {
                    parameter: 0,
                    value: borrowed,
                },
                MirCallArgument {
                    parameter: 1,
                    value: parent,
                },
            ],
            continuation: None,
        });
        // Shared aggregates are duplicated by lower_borrowed_value; the helper
        // borrows that duplicate, so the caller retains its cleanup obligation.
        self.discard_value(Some(value));
        Ok(Some(destination))
    }

    pub(super) fn lower_default_debug_body(
        &mut self,
        source: NodeId,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let local = self
            .resolve_local("@debug/value")
            .expect("default Debug receiver");
        let ty = self.locals[local.0].ty;
        let name = match ty {
            Type::Struct(id) => types.struct_name(id),
            Type::Class(id) => types.class_name(id),
            Type::Enum(id) => types.enum_name(id),
            _ => unreachable!(),
        };
        let name = name
            .rsplit('/')
            .next()
            .unwrap_or(name)
            .split('$')
            .next()
            .unwrap_or(name);
        let parent = self
            .lower_value(
                &CoreExpr {
                    id: source,
                    ty: Type::Bytes,
                    kind: CoreExprKind::Name("@debug/parent".into()),
                },
                functions,
                types,
            )?
            .unwrap();
        let value = self.debug_borrow_local(local, types);
        let path = self.debug_intrinsic(
            RuntimeIntrinsic::DebugPath(ty),
            Type::Bytes,
            vec![parent, value],
        );
        let path_local = self.new_local("@debug/path", Type::Bytes);
        self.bind_local("@debug/path", path_local);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination,
            local: path_local,
            value: Some(path),
        });
        let path = self
            .lower_value(
                &CoreExpr {
                    id: source,
                    ty: Type::Bytes,
                    kind: CoreExprKind::Name("@debug/path".into()),
                },
                functions,
                types,
            )?
            .unwrap();
        let status = self.debug_intrinsic(RuntimeIntrinsic::DebugPathStatus, Type::I32, vec![path]);
        let expanded = self.new_block();
        let limited = self.new_block();
        let merge = self.new_block();
        let zero = self.debug_integer(0);
        let condition = self.debug_equal(status, zero);
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: expanded,
            else_block: limited,
        })?;
        self.switch_to(limited);
        let cycle = self.new_block();
        let depth = self.new_block();
        let one = self.debug_integer(1);
        let condition = self.debug_equal(status, one);
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: cycle,
            else_block: depth,
        })?;
        let mut incoming = Vec::new();
        for (block, marker) in [(cycle, "<cycle>"), (depth, "<max-depth>")] {
            self.switch_to(block);
            let text = self.debug_text(&format!("{name}({marker})"));
            incoming.push((self.current, text));
            self.terminate(MirTerminator::Goto {
                target: merge,
                arguments: vec![text],
            })?;
        }
        self.switch_to(expanded);
        if let Type::Enum(id) = ty {
            let value = self.debug_borrow_local(local, types);
            let tag = self.next_value(Type::I32);
            self.push_statement(MirStatement::EnumTag {
                destination: tag,
                value,
            });
            for (variant, definition) in types.enum_variants(id).iter().enumerate() {
                let selected = self.new_block();
                let next = self.new_block();
                self.mark_scoped(selected);
                let expected = self.debug_integer(variant as u64);
                let condition = self.debug_equal(tag, expected);
                self.terminate(MirTerminator::Branch {
                    condition,
                    then_block: selected,
                    else_block: next,
                })?;
                self.switch_to(selected);
                if let Some(text) = self.debug_named_fields(
                    local,
                    &format!("{name}.{}", definition.name),
                    &definition.fields,
                    Some(variant),
                    source,
                    functions,
                    types,
                )? {
                    incoming.push((self.current, text));
                    self.terminate(MirTerminator::Goto {
                        target: merge,
                        arguments: vec![text],
                    })?;
                }
                self.switch_to(next);
            }
            self.terminate(MirTerminator::Unreachable)?;
        } else {
            let fields = match ty {
                Type::Struct(id) => types.struct_fields(id),
                Type::Class(id) => types.class_fields(id),
                _ => unreachable!(),
            };
            if let Some(text) =
                self.debug_named_fields(local, name, fields, None, source, functions, types)?
            {
                incoming.push((self.current, text));
                self.terminate(MirTerminator::Goto {
                    target: merge,
                    arguments: vec![text],
                })?;
            }
        }
        self.switch_to(merge);
        let destination = self.next_value(Type::String);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming,
        });
        Ok(Some(destination))
    }

    #[allow(clippy::too_many_arguments)]
    fn debug_named_fields(
        &mut self,
        local: MirLocalId,
        name: &str,
        fields: &[(String, Type)],
        variant: Option<usize>,
        source: NodeId,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if fields.is_empty() && variant.is_some() {
            return Ok(Some(self.debug_text(name)));
        }
        let mut text = self.debug_text(&format!("{name}("));
        for (index, (name, ty)) in fields.iter().enumerate() {
            let label = self.debug_text(&format!("{}{name}: ", if index == 0 { "" } else { ", " }));
            text = self.debug_concat(text, label);
            let value = self.debug_borrow_local(local, types);
            let field = self.next_value_with_ownership(*ty, MirOwnership::Borrowed);
            self.push_statement(if let Some(variant) = variant {
                MirStatement::EnumProject {
                    destination: field,
                    value,
                    variant,
                    field: index,
                }
            } else {
                MirStatement::Project {
                    destination: field,
                    base: value,
                    access: MirFieldAccess::Index(index),
                }
            });
            let Some(field) = self.debug_projected_value(field, source, functions, types)? else {
                return Ok(None);
            };
            text = self.debug_concat(text, field);
        }
        let closing = self.debug_text(")");
        Ok(Some(self.debug_concat(text, closing)))
    }

    fn debug_borrow_local(&mut self, local: MirLocalId, types: &CheckedTypes) -> MirValueId {
        let ty = self.locals[local.0].ty;
        let destination = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
        self.push_statement(if types.is_owned(ty) {
            MirStatement::BorrowLocal { destination, local }
        } else {
            MirStatement::Read { destination, local }
        });
        destination
    }

    pub(super) fn debug_intrinsic(
        &mut self,
        intrinsic: RuntimeIntrinsic,
        ty: Type,
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

    fn debug_integer(&mut self, value: u64) -> MirValueId {
        let destination = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination,
            value: MirConstant::Integer(value),
        });
        destination
    }

    fn debug_equal(&mut self, left: MirValueId, right: MirValueId) -> MirValueId {
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination,
            op: BinaryOp::Equal,
            left,
            right,
        });
        destination
    }
}
