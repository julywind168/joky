//! Container storage and ownership transfers.
use super::*;

impl Instruction<'_> {
    pub(super) fn buffers(&self, state: &mut State, alive: &Set) -> Result<(), Error> {
        let loan = self.loan();
        match &self.facts.plan.ops[self.pc] {
            Op::Buffer(BufferOp::Storage {
                intrinsic,
                args,
                dst,
                ..
            }) => {
                use crate::parser::ast::BufferIntrinsic;
                for arg in args.iter().flat_map(refs) {
                    self.check_access(
                        &arg,
                        exclusive_access(&arg.ty),
                        state,
                        self.live,
                        None,
                        &[],
                    )?;
                }
                let mut result = Reference {
                    origins: BTreeSet::from([Origin::new(loan)]),
                    loans: Set::from([loan]),
                    storage: dst.as_ref().is_some_and(|s| s.ty.is_owner()),
                    ..Default::default()
                };
                match intrinsic {
                    intrinsic if intrinsic.borrows_storage() => {
                        result = state[&args[0].index].clone();
                        result.interior = true;
                        result.ancestors.extend(&result.loans);
                        result.loans = Set::from([loan]);
                    }
                    BufferIntrinsic::TakeLast if !dst.as_ref().is_some_and(|s| s.ty.is_owner()) => {
                        result = buffer_contents(state, &args[0]);
                        result.ancestors.extend(&result.loans);
                        result.loans = Set::from([loan]);
                    }
                    BufferIntrinsic::Load
                    | BufferIntrinsic::ElementLoad
                    | BufferIntrinsic::SliceLoad => {
                        result = buffer_contents(state, &args[0]);
                        result.ancestors.extend(&result.loans);
                        result.loans = Set::from([loan]);
                    }
                    BufferIntrinsic::Store
                    | BufferIntrinsic::InitializeNext
                    | BufferIntrinsic::ElementStore
                    | BufferIntrinsic::SliceStore => {
                        let inputs = refs(args.last().unwrap());
                        for arg in inputs.iter().filter(|arg| !arg.ty.is_owner()) {
                            self.check_storage(arg, state)?;
                            let value = state[&arg.index].clone();
                            retain(&mut state.retained, (loan, arg.index), value, false);
                        }
                        let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                        let identity = state[&args[0].index].clone();
                        add_contents(state, &identity, &contents);
                    }
                    BufferIntrinsic::Replace => {
                        let contents = buffer_contents(state, &args[1]);
                        let identity = state[&args[0].index].clone();
                        add_contents(state, &identity, &contents);
                    }
                    BufferIntrinsic::Transfer => {
                        let contents = buffer_contents(state, &args[0]);
                        let identity = state[&args[1].index].clone();
                        add_contents(state, &identity, &contents);
                    }
                    _ => {}
                }
                // Reclaiming buffer storage never revokes copied content
                // references: their shared loans remain until the root ends.
                for arg in args.iter().filter(|arg| arg.ty.is_owner()).flat_map(refs) {
                    state.get_mut(&arg.index).unwrap().invalidate(self.pc);
                }
                for dst in dst.iter().flat_map(refs) {
                    state.insert(dst.index, result.clone());
                }
            }
            Op::Buffer(BufferOp::SwapReferences { left, right, .. }) => {
                self.check_access(left, true, state, self.live, None, &[])?;
                self.check_access(right, true, state, self.live, None, &[])?;
                let left_contents = buffer_contents(state, left);
                let right_contents = buffer_contents(state, right);
                let left_identity = state[&left.index].clone();
                let right_identity = state[&right.index].clone();
                add_contents(state, &left_identity, &right_contents);
                add_contents(state, &right_identity, &left_contents);
            }
            Op::Buffer(BufferOp::Swap {
                reference, value, ..
            }) => {
                self.check_access(reference, true, state, self.live, None, &[])?;
                let old_contents = buffer_contents(state, reference);
                let new_contents = union(
                    refs(value)
                        .iter()
                        .map(|input| buffer_contents(state, input)),
                );
                let identity = state[&reference.index].clone();
                add_contents(state, &identity, &new_contents);
                for input in refs(value) {
                    self.check_access(&input, true, state, self.live, None, &[])?;
                    state.insert(
                        input.index,
                        Reference {
                            origins: BTreeSet::from([Origin::new(loan)]),
                            loans: Set::from([loan]),
                            storage: true,
                            ..Default::default()
                        },
                    );
                    let identity = state[&input.index].clone();
                    add_contents(state, &identity, &old_contents);
                }
            }
            Op::Buffer(BufferOp::Move { dst, src }) => {
                for (dst, src) in refs(dst).into_iter().zip(refs(src)) {
                    if dst.index != src.index
                        && dst.ty.is_owner()
                        && state.get(&dst.index).is_some_and(|v| !v.invalid)
                    {
                        self.check_access(&dst, true, state, alive, Some(src.index), &[])?;
                        if dst.ty.is_explicit_arena() {
                            let roots: Vec<_> = state[&dst.index]
                                .origins
                                .iter()
                                .map(|origin| origin.root)
                                .collect();
                            for root in roots {
                                state.arena_retained.remove(&root);
                            }
                        }
                    }
                    let exclusive = exclusive_access(&src.ty);
                    self.check_access(&src, exclusive, state, alive, Some(dst.index), &[])?;
                    let mut value = state[&src.index].clone();
                    let contents = buffer_contents(state, &src);
                    if dst.index != src.index {
                        state.get_mut(&src.index).unwrap().invalidate(self.pc);
                        if dst.ty.is_owner() {
                            if dst.ty.is_explicit_arena() {
                                let roots =
                                    value.origins.iter().map(|origin| origin.root).collect();
                                move_arena(state, &roots, loan);
                            }
                            // The move has rejected every surviving source loan.
                            // Rebase the unique capability so a later loop allocation
                            // at the same call site cannot alias its previous owner.
                            value.origins = BTreeSet::from([Origin::new(loan)]);
                            value.loans = Set::from([loan]);
                            value.ancestors.clear();
                            add_contents(state, &value, &contents);
                        }
                    }
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(BufferOp::Access { src, mutable }) => {
                for src in refs(src) {
                    let exclusive = *mutable && exclusive_access(&src.ty);
                    self.check_access(&src, exclusive, state, self.live, None, &[])?;
                }
            }
            Op::Buffer(BufferOp::Borrow {
                dst,
                src,
                place,
                reservation,
            }) => {
                let inputs = refs(src);
                for src in &inputs {
                    self.check_access(
                        src,
                        !reservation && dst.ty.reference_type().unwrap().mutable,
                        state,
                        alive,
                        Some(dst.index),
                        &[],
                    )?;
                }
                let mut value = union(inputs.iter().map(|src| state[&src.index].clone()));
                if src.ty.is_explicit_arena() {
                    value
                        .arenas
                        .extend(value.origins.iter().map(|origin| origin.root));
                }
                if let Some(place) = place {
                    self.check_access(
                        place,
                        !reservation && dst.ty.reference_type().unwrap().mutable,
                        state,
                        alive,
                        Some(dst.index),
                        &[],
                    )?;
                    let storage = &state[&place.index];
                    value.origins.extend(storage.origins.iter().cloned());
                    value.arenas.extend(&storage.arenas);
                    value.ancestors.extend(&storage.ancestors);
                    value.ancestors.extend(&storage.loans);
                }
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::Buffer(BufferOp::End(owner)) => {
                for owner in refs(owner) {
                    // Moving out ends this binding without ending the destination owner.
                    if state.get(&owner.index).is_some_and(|v| !v.invalid) {
                        // Ending an inline shared-reference lane does not end its referent.
                        if owner.ty.is_owner() {
                            self.check_access(&owner, true, state, alive, None, &[])?;
                        }
                        if owner.ty.is_explicit_arena() {
                            let roots: Vec<_> = state[&owner.index]
                                .origins
                                .iter()
                                .map(|origin| origin.root)
                                .collect();
                            for root in roots {
                                state.arena_retained.remove(&root);
                            }
                        }
                        state.get_mut(&owner.index).unwrap().invalidate(self.pc);
                    }
                }
            }
            Op::ReadOwner { .. } | Op::Buffer(BufferOp::Extract { .. }) => {
                let (owner, selected, dst, reference) = match &self.facts.plan.ops[self.pc] {
                    Op::ReadOwner {
                        owner,
                        ownership_path,
                        dst,
                        reference,
                        ..
                    } => (
                        owner,
                        if owner.ty.is_place() {
                            owner.clone()
                        } else {
                            owner.field_path(ownership_path)
                        },
                        dst.as_ref().unwrap(),
                        Some(reference),
                    ),
                    Op::Buffer(BufferOp::Extract { owner, src, dst }) => {
                        (owner, src.clone(), dst, None)
                    }
                    _ => unreachable!(),
                };
                let outputs = refs(dst);
                if let Some(reference) = reference {
                    self.check_access(reference, true, state, alive, Some(outputs[0].index), &[])?;
                }
                let detach = matches!(
                    &self.facts.plan.ops[self.pc],
                    Op::ReadOwner { detach: true, .. }
                );
                let consumed = if detach { &selected } else { owner };
                for input in refs(consumed) {
                    let exclusive = exclusive_access(&input.ty);
                    self.check_access(&input, exclusive, state, alive, None, &[])?;
                }
                let values: Vec<_> = refs(&selected)
                    .iter()
                    .map(|src| state[&src.index].clone())
                    .collect();
                // Record projections consume the whole source; tuple patterns
                // detach each leaf through Detach and end the remainder after.
                for input in refs(consumed) {
                    state.get_mut(&input.index).unwrap().invalidate(self.pc);
                }
                for (index, dst) in outputs.into_iter().enumerate() {
                    let value = values[if owner.ty.is_place() { 0 } else { index }].clone();
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(BufferOp::Detach { dst, src, .. }) => {
                // Irrefutable destructuring detaches each leaf in turn. Only
                // the selected lanes are read here; the explicit End that
                // follows releases the remainder and ends the binding.
                for input in refs(src) {
                    let exclusive = exclusive_access(&input.ty);
                    self.check_access(&input, exclusive, state, alive, None, &[])?;
                }
                let values: Vec<_> = refs(src)
                    .iter()
                    .map(|slot| (state[&slot.index].clone(), buffer_contents(state, slot)))
                    .collect();
                for input in refs(src) {
                    state.get_mut(&input.index).unwrap().invalidate(self.pc);
                }
                for (dst, (mut value, contents)) in refs(dst).into_iter().zip(values) {
                    if dst.ty.is_owner() {
                        // Checked detachment creates a unique cleanup group after
                        // rejecting live aliases to the source. Call-result lanes
                        // may share conservative origins, but detached owners no
                        // longer share control identity. Stored element references
                        // keep their original dependencies, separately from it.
                        value.origins = BTreeSet::from([Origin::new(loan)]);
                        value.loans = Set::from([loan]);
                        value.ancestors.clear();
                        add_contents(state, &value, &contents);
                    }
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(BufferOp::Replace {
                dst,
                src,
                owner,
                place,
                ..
            }) => {
                self.check_access(place, true, state, self.live, None, &[])?;
                for input in refs(owner).into_iter().chain(refs(src)) {
                    self.check_access(&input, true, state, self.live, None, &[])?;
                }
                if owner.ty.is_place() {
                    for input in refs(src) {
                        state.get_mut(&input.index).unwrap().invalidate(self.pc);
                    }
                } else {
                    for (dst, src) in refs(dst).into_iter().zip(refs(src)) {
                        let value = state[&src.index].clone();
                        state.get_mut(&src.index).unwrap().invalidate(self.pc);
                        state.insert(dst.index, value);
                    }
                }
            }
            _ => unreachable!("buffers transfer requires a matching instruction"),
        }
        Ok(())
    }
}
