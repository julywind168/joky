//! Convert a partial ordering into a relational operator result.
use super::*;
use crate::hir::{CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(super) fn lower_ordering_test(
        &mut self,
        name: &str,
        comparison: &CoreExpr,
        functions: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let op = match name {
            "@ordering/Less" => BinaryOp::Less,
            "@ordering/LessEqual" => BinaryOp::LessEqual,
            "@ordering/Greater" => BinaryOp::Greater,
            "@ordering/GreaterEqual" => BinaryOp::GreaterEqual,
            _ => return Err(Diagnostic::codegen("unknown ordering operator")),
        };
        // Preserve scalar constant folding after selecting the builtin trait.
        if let CoreExprKind::Call {
            callee, arguments, ..
        } = &comparison.kind
        {
            if let CoreExprKind::Field {
                value,
                access: FieldAccess::Name(method),
            } = &callee.kind
            {
                if method == crate::sema::PARTIAL_ORD_METHOD
                    && (value.ty.is_numeric() || value.ty == Type::Bool)
                {
                    let Some(left) = self.lower_value(value, functions, types)? else {
                        return Ok(None);
                    };
                    let Some(right) = self.lower_value(&arguments[0].value, functions, types)?
                    else {
                        return Ok(None);
                    };
                    let destination = self.next_value(Type::Bool);
                    self.push_statement(MirStatement::Binary {
                        destination,
                        op,
                        left,
                        right,
                    });
                    return Ok(Some(destination));
                }
            }
        }
        let Some(value) = self.lower_value(comparison, functions, types)? else {
            return Ok(None);
        };
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value,
        });
        let zero = self.ordering_integer(0);
        let condition = self.ordering_binary(BinaryOp::Equal, tag, zero);
        let some = self.new_block();
        let none = self.new_block();
        let done = self.new_block();
        self.terminate(MirTerminator::Branch {
            condition,
            then_block: some,
            else_block: none,
        })?;
        self.switch_to(some);
        let ordering = self.next_value(types.ordering_type());
        self.push_statement(MirStatement::EnumProject {
            destination: ordering,
            value,
            variant: 0,
            field: 0,
        });
        let tag = self.next_value(Type::I32);
        self.push_statement(MirStatement::EnumTag {
            destination: tag,
            value: ordering,
        });
        let equal = self.ordering_integer(1);
        let result = self.ordering_binary(op, tag, equal);
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![result],
        })?;
        self.switch_to(none);
        let unordered = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Const {
            destination: unordered,
            value: MirConstant::Boolean(false),
        });
        self.terminate(MirTerminator::Goto {
            target: done,
            arguments: vec![unordered],
        })?;
        self.switch_to(done);
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Phi {
            destination,
            incoming: vec![(some, result), (none, unordered)],
        });
        Ok(Some(destination))
    }

    pub(super) fn ordering_integer(&mut self, value: u64) -> MirValueId {
        let destination = self.next_value(Type::I32);
        self.push_statement(MirStatement::Const {
            destination,
            value: MirConstant::Integer(value),
        });
        destination
    }

    fn ordering_binary(&mut self, op: BinaryOp, left: MirValueId, right: MirValueId) -> MirValueId {
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Binary {
            destination,
            op,
            left,
            right,
        });
        destination
    }
}
