use super::super::*;
use crate::hir::{CoreExpr, CoreExprKind, CorePattern};

impl Lowerer<'_> {
    pub(super) fn lower_when(
        &mut self,
        cowns: &[CoreExpr],
        bindings: Option<&[CorePattern]>,
        until: Option<&CoreExpr>,
        body: &CoreExpr,
        function_names: &HashSet<String>,
        types: &CheckedTypes,
    ) -> Result<Option<MirValueId>, Diagnostic> {
        if cowns.is_empty() {
            return Err(Diagnostic::codegen("MIR when requires at least one Cown"));
        }
        let mut leases = Vec::with_capacity(cowns.len());
        let mut acquire_arguments = Vec::with_capacity(cowns.len());
        for cown_expression in cowns {
            let Some(cown) = self.lower_value(cown_expression, function_names, types)? else {
                return Ok(None);
            };
            let payload_type = match self.value_types[cown.0] {
                Type::Cown(id) => types.cown_type(id),
                _ => return Err(Diagnostic::codegen("MIR when receiver is not a Cown")),
            };
            // Keep a capability for the matching release while the acquire
            // consumes the lowered handle in MIR ownership.
            let release_cown =
                self.next_value_with_ownership(cown_expression.ty, MirOwnership::Shared);
            self.push_statement(MirStatement::Dup {
                destination: release_cown,
                value: cown,
            });
            acquire_arguments.push(MirCallArgument {
                parameter: acquire_arguments.len(),
                value: cown,
            });
            leases.push((release_cown, payload_type));
        }
        self.lower_cown_acquisition(acquire_arguments, false)?;
        let check_block = if until.is_some() {
            let block = self.new_block();
            // Rechecking stays inside this lease region. Loop-edge cleanup must
            // not release the set that the condition wait just reacquired.
            self.block_cown_depths[block.0] = self.cown_leases.len() + leases.len();
            self.terminate(MirTerminator::Goto {
                target: block,
                arguments: Vec::new(),
            })?;
            self.switch_to(block);
            block
        } else {
            self.current
        };
        let mut resolved_leases = Vec::with_capacity(leases.len());
        for (release_cown, payload_type) in leases {
            let payload = self.next_value_with_ownership(payload_type, MirOwnership::Borrowed);
            self.push_statement(MirStatement::RuntimeCall {
                destination: payload,
                intrinsic: RuntimeIntrinsic::CownPayload(payload_type),
                arguments: vec![MirCallArgument {
                    parameter: 0,
                    value: release_cown,
                }],
            });
            resolved_leases.push((release_cown, payload, payload_type));
        }
        self.bindings.push(HashMap::new());
        for (index, (_, payload, payload_type)) in resolved_leases.iter().enumerate() {
            let binding_name = bindings.and_then(|patterns| patterns.get(index)).and_then(
                |pattern| match pattern {
                    CorePattern::Binding { name, .. } => Some(name.as_str()),
                    CorePattern::Wildcard { .. }
                    | CorePattern::EnumVariant { .. }
                    | CorePattern::Tuple { .. } => None,
                },
            );
            let implicit_name = bindings.is_none().then(|| match &cowns[index].kind {
                CoreExprKind::Name(name) => Some(name.as_str()),
                _ => None,
            });
            if let Some(name) = binding_name.or(implicit_name.flatten()) {
                let local =
                    self.new_local_with_ownership(name, *payload_type, MirOwnership::Borrowed);
                self.bind_local(name, local);
                let destination = self.next_value(Type::Unit);
                self.push_statement(MirStatement::Bind {
                    local,
                    value: Some(*payload),
                    destination,
                });
            }
        }
        let lease_depth = self.cown_leases.len();
        self.cown_leases
            .extend(resolved_leases.iter().map(|(cown, _, _)| *cown));
        if let Some(until) = until {
            let condition = self
                .lower_value(until, function_names, types)?
                .ok_or_else(|| Diagnostic::codegen("until guard must complete"))?;
            let body_block = self.new_block();
            let wait_block = self.new_block();
            self.terminate(MirTerminator::Branch {
                condition,
                then_block: body_block,
                else_block: wait_block,
            })?;
            self.switch_to(wait_block);
            let mut arguments = Vec::new();
            for (index, (cown, _, _)) in resolved_leases.iter().enumerate() {
                let value =
                    self.next_value_with_ownership(self.value_types[cown.0], MirOwnership::Shared);
                self.push_statement(MirStatement::Dup {
                    destination: value,
                    value: *cown,
                });
                arguments.push(MirCallArgument {
                    parameter: index,
                    value,
                });
            }
            self.lower_cown_acquisition(arguments, true)?;
            self.terminate(MirTerminator::Goto {
                target: check_block,
                arguments: Vec::new(),
            })?;
            self.switch_to(body_block);
        }
        let body_value = self.lower_value(body, function_names, types)?;
        self.bindings.pop();
        if self.is_open() {
            self.release_cown_leases_from(lease_depth);
        }
        self.cown_leases.truncate(lease_depth);
        Ok(body_value)
    }
    fn lower_cown_acquisition(
        &mut self,
        arguments: Vec<MirCallArgument>,
        wait_for_change: bool,
    ) -> Result<(), Diagnostic> {
        let destination = self.next_value(Type::Unit);
        let suspend_block = self.current;
        let resume_block = self.new_block();
        let continuation = MirContinuationId(self.next_continuation);
        self.next_continuation += 1;
        self.continuations.push(MirContinuation {
            id: continuation,
            kind: MirContinuationKind::CownAcquire,
            operation: None,
            callee: None,
            suspend_block,
            resume_block,
            resume_destination: Some(destination),
            generation: 0,
            locals_before_suspend: self.locals.len(),
            spill_values: Vec::new(),
            spill_slots: Vec::new(),
            frame_slots: Vec::new(),
        });
        self.push_statement(MirStatement::CownAcquire {
            destination,
            arguments,
            continuation,
            wait_for_change,
        });
        self.terminate(MirTerminator::Goto {
            target: resume_block,
            arguments: Vec::new(),
        })?;
        self.switch_to(resume_block);
        self.push_statement(MirStatement::Resume { continuation });
        Ok(())
    }
}
