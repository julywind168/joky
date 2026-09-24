use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind};

impl Lowerer<'_> {
    pub(in crate::mir::lower) fn captured_field(
        &self,
        declaration: Option<crate::syntax::NodeId>,
        name: &str,
    ) -> Option<(MirLocalId, usize)> {
        let (local, state) = self.capture_state.as_ref()?;
        state
            .captures
            .iter()
            .position(|capture| {
                if let Some(declaration) = declaration {
                    capture.declaration == Some(declaration)
                } else {
                    capture.name == name && self.resolve_local(name).is_none()
                }
            })
            .map(|index| (*local, index))
    }

    pub(in crate::mir::lower) fn expression_capture(
        &self,
        expression: &CoreExpr,
        name: &str,
    ) -> Option<(MirLocalId, usize)> {
        self.captured_field(self.types.local_bindings.get(&expression.id).copied(), name)
    }

    pub(in crate::mir::lower) fn borrow_capture_state(&mut self, local: MirLocalId) -> MirValueId {
        let destination =
            self.next_value_with_ownership(self.locals[local.0].ty, MirOwnership::Borrowed);
        self.push_statement(MirStatement::BorrowLocal { destination, local });
        destination
    }

    pub(in crate::mir::lower) fn read_capture(
        &mut self,
        local: MirLocalId,
        index: usize,
        ty: Type,
        borrowed: bool,
    ) -> Result<MirValueId, Diagnostic> {
        if self.types.is_owned(ty) && !borrowed {
            return Err(Diagnostic::semantic(
                "cannot move an owned capture out of a borrowed closure environment",
                self.current_span.unwrap_or(crate::Span::new(0, 0)),
            ));
        }
        let base = self.borrow_capture_state(local);
        let ownership = if self.types.is_owned(ty) {
            MirOwnership::Borrowed
        } else {
            ownership_for_type(ty, self.types)
        };
        let destination = self.next_value_with_ownership(ty, ownership);
        self.push_statement(MirStatement::Project {
            destination,
            base,
            access: MirFieldAccess::Index(index),
        });
        if self.types.is_shared(ty) {
            let duplicate = self.next_value(ty);
            self.push_statement(MirStatement::Dup {
                destination: duplicate,
                value: destination,
            });
            Ok(duplicate)
        } else {
            Ok(destination)
        }
    }

    pub(super) fn lower_closure(
        &mut self,
        expression: &CoreExpr,
        function_name: &str,
        captures: &[(String, Type)],
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        let function = *self
            .closure_ids
            .get(function_name)
            .ok_or_else(|| Diagnostic::codegen("MIR closure function was not resolved"))?;
        let destination = self.next_value(expression.ty);
        let CoreExprKind::Closure {
            capture_bindings, ..
        } = &expression.kind
        else {
            unreachable!("closure lowering requires a closure expression");
        };
        let state_type = self
            .function_bodies
            .get(function_name)
            .and_then(|function| function.closure_state.as_ref())
            .map(|state| state.ty);
        let capture_values = captures
            .iter()
            .enumerate()
            .map(|(index, (name, ty))| {
                let binding = capture_bindings.get(index);
                let declaration = binding.and_then(|binding| binding.declaration);
                if let Some((local, field)) = self.captured_field(declaration, name) {
                    if binding.is_some_and(|binding| binding.mutable) {
                        return Err(Diagnostic::semantic(
                            "cannot consume a mutable capture from a borrowed closure environment",
                            self.current_span.unwrap_or(crate::Span::new(0, 0)),
                        ));
                    }
                    return self.read_capture(local, field, *ty, false).map(Some);
                }
                let local = declaration
                    .and_then(|id| self.source_locals.get(&id).copied())
                    .or_else(|| self.resolve_local(name));
                if let Some(local) = local {
                    if binding.is_some_and(|binding| binding.mutable) {
                        let destination = self.next_value(*ty);
                        self.push_statement(MirStatement::TakeLocal { destination, local });
                        return Ok(Some(destination));
                    }
                    if self.locals[local.0].ownership == MirOwnership::Borrowed {
                        let destination =
                            self.next_value_with_ownership(*ty, MirOwnership::Borrowed);
                        self.push_statement(MirStatement::BorrowLocal { destination, local });
                        return Ok(Some(destination));
                    }
                }
                self.lower_value(
                    &CoreExpr {
                        id: expression.id,
                        ty: *ty,
                        kind: CoreExprKind::Name(name.clone()),
                    },
                    function_names,
                    types,
                )
            })
            .collect::<Result<Option<Vec<_>>, _>>()?
            .unwrap_or_default();
        let capture_values = if let Some(ty) = state_type {
            let state = self.next_value(ty);
            self.push_statement(MirStatement::Construct {
                destination: state,
                type_id: MirTypeId::from_type(ty).expect("closure state is a class"),
                fields: capture_values,
            });
            vec![state]
        } else {
            capture_values
        };
        self.push_statement(MirStatement::FunctionValue {
            destination,
            function,
            captures: capture_values,
        });
        Ok(Some(destination))
    }
}
