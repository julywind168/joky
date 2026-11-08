//! Structured task captures, results and scope loans.
use super::*;

impl Instruction<'_> {
    pub(super) fn tasks(&self, state: &mut State, alive: &Set) -> Result<(), Error> {
        let loan = self.loan();
        match &self.facts.plan.ops[self.pc] {
            Op::Concurrent {
                branches, result, ..
            } => {
                for branch in branches {
                    self.record_child_escapes(&branch.name, &branch.args, state);
                }
                if alive.iter().any(|index| {
                    self.facts.types[index].is_buffer_view()
                        && !self.facts.types[index].is_suspend_safe_view()
                        && state.get(index).is_some_and(|value| {
                            !value.invalid && value.has_root(self.facts.owner_roots)
                        })
                }) {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidCall,
                        "A borrowed text view cannot remain live across suspension",
                    ));
                }

                // Join/race is one structured boundary in the caller CFG. All
                // argument snapshots stay live until branches complete or cancel,
                // including while later arguments are evaluated or suspend.
                for arg in branches
                    .iter()
                    .flat_map(|branch| branch.args.iter().flatten())
                    .flat_map(refs)
                {
                    if !arg.ty.is_suspend_safe_owner()
                        && !arg.ty.is_suspend_safe_view()
                        && (arg.ty.contains_buffers() || state[&arg.index].storage)
                    {
                        return Err(type_error(
                            self.span(),
                            ErrorCode::InvalidCall,
                            "Buffer owners and views cannot enter scope or race",
                        ));
                    }
                    self.check_access(
                        &arg,
                        arg.ty.is_suspend_safe_owner()
                            || arg.ty.reference_type().is_some_and(|r| r.mutable),
                        state,
                        self.live,
                        None,
                        &[],
                    )?;
                }
                // Validate all branches before retaining any inputs. Short loans
                // end at the join; only retained parameters impose task storage.
                for arg in branches.iter().flat_map(|branch| {
                    retained_child_arguments(
                        &self.facts.functions[&branch.name],
                        &branch.name,
                        &branch.args,
                    )
                }) {
                    let value = state[&arg.index].clone();
                    self.check_storage(&arg, state)?;
                    retain(
                        &mut state.retained,
                        (loan, arg.index),
                        value,
                        arg.ty.reference_type().unwrap().mutable,
                    );
                }
                // Reference-free results own fresh storage or consumed inputs.
                for dst in result.iter().flat_map(refs) {
                    let mut value = Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        storage: true,
                        ..Default::default()
                    };
                    for arg in branches
                        .iter()
                        .flat_map(|b| b.args.iter().flatten())
                        .flat_map(refs)
                        .filter(|s| s.ty.is_owner())
                    {
                        let source = &state[&arg.index];
                        value.origins.extend(source.origins.iter().cloned());
                        value.ancestors.extend(&source.ancestors);
                        value.ancestors.extend(&source.loans);
                    }
                    state.insert(dst.index, value);
                }
                for branch in branches {
                    for dst in branch.result.iter().flat_map(refs) {
                        let origin = self.facts.plan.slot_count
                            + self.facts.plan.ops.len()
                            + branch.result.as_ref().unwrap().index;
                        let mut value = Reference {
                            origins: BTreeSet::from([Origin::new(origin)]),
                            loans: Set::from([origin]),
                            storage: true,
                            ..Default::default()
                        };
                        for arg in branch
                            .args
                            .iter()
                            .flatten()
                            .flat_map(refs)
                            .filter(|s| s.ty.is_owner())
                        {
                            let source = &state[&arg.index];
                            value.origins.extend(source.origins.iter().cloned());
                            value.ancestors.extend(&source.ancestors);
                            value.ancestors.extend(&source.loans);
                        }
                        state.insert(dst.index, value);
                    }
                }
                for arg in branches
                    .iter()
                    .flat_map(|branch| branch.args.iter().flatten())
                    .flat_map(refs)
                    .filter(|arg| arg.ty == Type::String || arg.ty.is_suspend_safe_owner())
                {
                    state.get_mut(&arg.index).unwrap().invalidate(self.pc);
                }
            }
            // A dynamic branch registration snapshots its captures here while
            // the parent body keeps running. Reference captures stay live as
            // scope-tagged loans until the scope's join; retained parameters
            // impose the same task-long storage contract as ordinary calls.
            Op::TaskBranch {
                scope,
                entry,
                captures,
                ..
            } => {
                self.record_child_escapes(entry, captures, state);
                if alive.iter().any(|index| {
                    self.facts.types[index].is_buffer_view()
                        && !self.facts.types[index].is_suspend_safe_view()
                        && state.get(index).is_some_and(|value| {
                            !value.invalid && value.has_root(self.facts.owner_roots)
                        })
                }) {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidCall,
                        "A borrowed text view cannot remain live across suspension",
                    ));
                }
                let capture_refs: Vec<_> = captures.iter().flatten().flat_map(refs).collect();
                for capture in &capture_refs {
                    if !capture.ty.is_suspend_safe_owner()
                        && !capture.ty.is_suspend_safe_view()
                        && (capture.ty.contains_buffers() || state[&capture.index].storage)
                    {
                        return Err(type_error(
                            self.span(),
                            ErrorCode::InvalidCall,
                            "Buffer owners and views cannot enter scope or race",
                        ));
                    }
                    self.check_access(
                        capture,
                        capture.ty.is_suspend_safe_owner()
                            || capture.ty.reference_type().is_some_and(|r| r.mutable),
                        state,
                        self.live,
                        None,
                        &[],
                    )?;
                }
                for capture in capture_refs
                    .iter()
                    .filter(|c| c.ty.reference_type().is_some())
                {
                    let value = state[&capture.index].clone();
                    retain_region(
                        state,
                        *scope,
                        (loan, capture.index),
                        value,
                        capture.ty.reference_type().unwrap().mutable,
                    );
                }
                for capture in
                    retained_child_arguments(&self.facts.functions[entry], entry, captures)
                {
                    self.check_storage(&capture, state)?;
                    let value = state[&capture.index].clone();
                    retain(
                        &mut state.retained,
                        (loan, capture.index),
                        value,
                        capture.ty.reference_type().unwrap().mutable,
                    );
                }
                for capture in capture_refs
                    .iter()
                    .filter(|c| c.ty == Type::String || c.ty.is_suspend_safe_owner())
                {
                    state.get_mut(&capture.index).unwrap().invalidate(self.pc);
                }
            }
            // Joining ends the scope's child-task loans on every path through
            // the join, including abrupt exits routed here before jumping on.
            Op::TaskJoin { scope, .. } => {
                state.regions.remove(scope);
            }
            _ => unreachable!("tasks transfer requires a matching instruction"),
        }
        Ok(())
    }
}
