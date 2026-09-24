//! Shared operand borrowing and projection for structural comparisons.
use super::super::*;

impl Lowerer<'_> {
    pub(super) fn comparison_bind(&mut self, value: MirValueId) -> MirLocalId {
        let name = format!("@comparison/{}", self.locals.len());
        let local = self.new_local_with_ownership(
            &name,
            self.value_types[value.0],
            self.value_ownership[value.0],
        );
        self.bind_local(&name, local);
        let destination = self.next_value(Type::Unit);
        self.push_statement(MirStatement::Bind {
            destination,
            local,
            value: Some(value),
        });
        local
    }

    pub(super) fn comparison_borrow(
        &mut self,
        local: MirLocalId,
        types: &CheckedTypes,
    ) -> MirValueId {
        let ty = self.locals[local.0].ty;
        let destination = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
        self.push_statement(if types.is_owned(ty) {
            MirStatement::BorrowLocal { destination, local }
        } else {
            MirStatement::Read { destination, local }
        });
        destination
    }

    pub(super) fn comparison_copy(&mut self, local: MirLocalId) -> MirValueId {
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

    pub(super) fn comparison_project(
        &mut self,
        local: MirLocalId,
        ty: Type,
        variant: Option<usize>,
        index: usize,
        types: &CheckedTypes,
    ) -> MirLocalId {
        let value = self.comparison_borrow(local, types);
        let destination = self.next_value_with_ownership(ty, MirOwnership::Borrowed);
        self.push_statement(if let Some(variant) = variant {
            MirStatement::EnumProject {
                destination,
                value,
                variant,
                field: index,
            }
        } else {
            MirStatement::Project {
                destination,
                base: value,
                access: MirFieldAccess::Index(index),
            }
        });
        self.comparison_bind(destination)
    }

    pub(super) fn comparison_intrinsic(
        &mut self,
        intrinsic: RuntimeIntrinsic,
        ty: Type,
        value: MirValueId,
    ) -> MirValueId {
        let destination = self.next_value(ty);
        self.push_statement(MirStatement::RuntimeCall {
            destination,
            intrinsic,
            arguments: vec![MirCallArgument {
                parameter: 0,
                value,
            }],
        });
        destination
    }

    pub(super) fn comparison_drop_local(&mut self, local: MirLocalId) {
        if self.locals[local.0].ownership == MirOwnership::Shared {
            let destination = self.next_value(Type::Unit);
            self.push_statement(MirStatement::DropLocal { destination, local });
        }
    }

    pub(super) fn comparison_bool(&mut self, value: bool) -> MirValueId {
        let destination = self.next_value(Type::Bool);
        self.push_statement(MirStatement::Const {
            destination,
            value: MirConstant::Boolean(value),
        });
        destination
    }

    pub(super) fn comparison_scalar(&mut self, left: MirValueId, right: MirValueId) -> MirValueId {
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
