use super::super::*;
use crate::hir::{CoreCallArgument, CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_dynamic_value(
        &mut self,
        expression: &CoreExpr,
        payload: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let Type::Dyn(id) = expression.ty else {
            unreachable!()
        };
        let Some(value) = self.lower_value(payload, functions, types)? else {
            return Ok(None);
        };
        if matches!(payload.ty, Type::Dyn(_)) {
            let destination = self.next_value(expression.ty);
            self.push_statement(MirStatement::DynamicUpcast { destination, value });
            return Ok(Some(destination));
        }
        let methods = types.dynamic_types[id]
            .methods
            .iter()
            .enumerate()
            .map(|(slot, _)| {
                self.function_ids
                    .get(&crate::hir::dynamic_method_name(id, payload.ty, slot))
                    .copied()
                    .ok_or_else(|| Diagnostic::codegen("dynamic method wrapper is missing"))
            })
            .collect::<Result<_, _>>()?;
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::DynamicValue {
            destination,
            methods,
            value,
        });
        Ok(Some(destination))
    }

    pub(super) fn lower_dynamic_call(
        &mut self,
        expression: &CoreExpr,
        callee: &CoreExpr,
        arguments: &[CoreCallArgument],
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let CoreExprKind::Field {
            value: receiver,
            access: FieldAccess::Name(name),
        } = &callee.kind
        else {
            unreachable!()
        };
        let Type::Dyn(id) = receiver.ty else {
            unreachable!()
        };
        let (slot, method) = types.dynamic_types[id]
            .methods
            .iter()
            .enumerate()
            .find(|(_, method)| method.implementation_name == *name)
            .ok_or_else(|| Diagnostic::codegen("dynamic method is missing"))?;
        let receiver = if method.receiver == crate::syntax::ReceiverMode::Owned {
            let Some(value) = self.lower_value(receiver, functions, types)? else {
                return Ok(None);
            };
            let local = self.new_local("<consumed dynamic receiver>", receiver.ty);
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::Bind {
                local,
                value: Some(value),
                destination,
            });
            let destination = self.next_value_with_ownership(receiver.ty, MirOwnership::Borrowed);
            self.push_statement(MirStatement::BorrowLocal { destination, local });
            destination
        } else {
            let Some(value) = self.lower_borrowed_value(receiver, functions, types)? else {
                return Ok(None);
            };
            value
        };
        let callee = self
            .next_value_with_ownership(Type::Function(method.signature), MirOwnership::Borrowed);
        self.push_statement(MirStatement::Project {
            destination: callee,
            base: receiver,
            access: MirFieldAccess::Index(slot),
        });
        let signature = types.function_type(method.signature);
        let mut used = vec![false; signature.parameters.len()];
        let mut next = 0;
        let mut operands = Vec::new();
        for argument in arguments {
            let parameter = resolve_argument_index(
                argument.label.as_deref(),
                &signature.parameter_names,
                &mut used,
                &mut next,
            )?;
            let value = if method.parameter_borrows[parameter] {
                self.lower_borrowed_value(&argument.value, functions, types)?
            } else {
                self.lower_value(&argument.value, functions, types)?
            };
            let Some(value) = value else { return Ok(None) };
            operands.push(MirCallArgument { parameter, value });
        }
        let destination = self.next_value(expression.ty);
        self.push_statement(MirStatement::CallIndirect {
            destination,
            callee,
            arguments: operands,
            may_suspend: true,
            continuation: None,
        });
        let destination = self.lower_task_poll_result(destination)?;
        Ok(Some(destination))
    }
}
