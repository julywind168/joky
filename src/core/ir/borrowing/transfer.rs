//! Per-function facts and per-instruction reference-state transfers.
//! The control-flow worklist stays in borrowing.rs; handlers do not schedule edges.
mod access;
mod buffers;
mod calls;
mod tasks;
use super::*;

/// Function-wide facts and the optional escape-summary output for inference.
pub(super) struct Facts<'a> {
    pub plan: &'a FunctionPlan,
    pub functions: &'a HashMap<String, FunctionInfo>,
    pub types: &'a BTreeMap<usize, Type>,
    pub local_roots: &'a Set,
    pub owner_roots: &'a Set,
    pub local_places: &'a Set,
    pub argument_roots: &'a Set,
    pub arena_parameter_roots: &'a Set,
    pub explicit_allocation_roots: &'a Set,
    pub reservations: &'a Set,
    pub construction: &'a Set,
    pub escaped: Option<&'a RefCell<Escapes>>,
}

/// One visit, with a stable diagnostic location and live-in set.
/// Live-out sets remain explicit because reads and moves use different boundaries.
pub(super) struct Instruction<'a> {
    pub facts: &'a Facts<'a>,
    pub pc: usize,
    pub live: &'a Set,
}

impl Instruction<'_> {
    fn span(&self) -> &Span {
        self.facts
            .plan
            .ops
            .spans()
            .get(self.pc)
            .unwrap_or(&self.facts.plan.span)
    }
    fn loan(&self) -> usize {
        self.facts.plan.slot_count + self.pc
    }
    pub(super) fn apply(&self, state: &mut State, alive: &Set) -> Result<(), Error> {
        self.check_suspension(state)?;
        let loan = self.loan();
        match &self.facts.plan.ops[self.pc] {
            Op::ArenaNew { dst } => {
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        storage: true,
                        ..Default::default()
                    },
                );
            }
            Op::ArenaAlloc {
                dst, arena, value, ..
            } => {
                self.check_access(arena, false, state, self.live, None, &[])?;
                let arenas = state[&arena.index].arenas.clone();
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    self.check_access(input, false, state, self.live, None, &[])?;
                    self.check_arena_storage(input, &arenas, state, false)?;
                    let value = state[&input.index].clone();
                    for arena in &arenas {
                        retain_arena(state, *arena, (loan, input.index), value.clone());
                    }
                }
                let contents = union(inputs.iter().map(|input| state[&input.index].clone()));
                state
                    .contents
                    .entry(loan)
                    .and_modify(|old| {
                        merge(old, &contents);
                    })
                    .or_insert(contents);
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        arenas,
                        loans: Set::from([loan]),
                        storage: true,
                        ..Default::default()
                    },
                );
            }
            Op::DomainOwner {
                packet,
                value,
                decode,
                ..
            } => {
                self.check_access(packet, !decode, state, self.live, None, &[])?;
                if *decode {
                    state.insert(
                        value.index,
                        Reference {
                            origins: BTreeSet::from([Origin::new(loan)]),
                            loans: Set::from([loan]),
                            storage: true,
                            ..Default::default()
                        },
                    );
                } else {
                    self.check_access(value, false, state, self.live, None, &[])?;
                }
            }
            Op::Text { dst, source } => {
                if let TextSource::Clone(src) = source {
                    self.check_access(src, false, state, self.live, None, &[])?;
                }
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        storage: true,
                        ..Default::default()
                    },
                );
            }
            Op::Graph(op) => match op.as_ref() {
                GraphOp::Link {
                    reference, source, ..
                } => {
                    // These operands are fresh, unobservable group capabilities,
                    // checked lexically by graph.rs, never ordinary references.
                    // No shared loan is published while wiring the group.
                    let contents = state[&source.index].clone();
                    for root in state[&reference.index]
                        .origins
                        .iter()
                        .map(|origin| origin.root)
                        .collect::<Set>()
                    {
                        state
                            .contents
                            .entry(root)
                            .and_modify(|old| {
                                merge(old, &contents);
                            })
                            .or_insert_with(|| contents.clone());
                    }
                }
                GraphOp::Seal { dst, root, members } => {
                    let mut value = state[&root.index].clone();
                    value.ancestors.extend(&value.loans);
                    value.loans = Set::from([loan]);
                    for member in members {
                        let contents = state[&member.index].clone();
                        retain(&mut state.retained, (loan, member.index), contents, false);
                        state.get_mut(&member.index).unwrap().invalid = true;
                    }
                    state.insert(dst.index, value);
                }
            },
            Op::Reserve { dst, src } => {
                self.check_access(src, false, state, alive, Some(dst.index), &[])?;
                let mut value = state[&src.index].clone();
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::Project {
                dst,
                src,
                path,
                reservation,
            } => {
                self.check_access(
                    src,
                    !reservation && dst.ty.reference_type().unwrap().mutable,
                    state,
                    alive,
                    Some(dst.index),
                    path,
                )?;
                let mut value = state[&src.index].projected(path, self.facts.local_roots);
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::PatternProject { dst, src, .. } | Op::EntryProject { dst, src, .. } => {
                let path: Vec<_> = match &self.facts.plan.ops[self.pc] {
                    Op::PatternProject { path, .. } => {
                        path.iter().copied().map(Projection::Pattern).collect()
                    }
                    Op::EntryProject { column, .. } => vec![Projection::Entry(*column)],
                    _ => unreachable!(),
                };
                // Validate availability separately from access: sibling pattern
                // bindings are disjoint even when their parent is an array loan.
                if state.get(&src.index).is_none_or(|v| v.invalid) {
                    self.check_access(src, false, state, alive, None, &[])?;
                }
                let mut value = state[&src.index].clone();
                value.storage |= src.ty.is_buffer_view();
                value.origins = value
                    .origins
                    .into_iter()
                    .map(|mut origin| {
                        origin.path.extend(path.iter().cloned());
                        origin
                    })
                    .collect();
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
                self.check_access(
                    dst,
                    dst.ty.reference_type().unwrap().mutable,
                    state,
                    alive,
                    None,
                    &[],
                )?;
            }
            Op::Reference { dst, src, moving } => {
                let exclusive = dst.ty.reference_type().unwrap().mutable;
                self.check_access(src, exclusive, state, alive, Some(dst.index), &[])?;
                let mut value = state[&src.index].clone();
                if *moving {
                    if src.ty.reference_type().unwrap().mutable && dst.index != src.index {
                        state.get_mut(&src.index).unwrap().invalid = true;
                    }
                } else {
                    value.ancestors.extend(&value.loans);
                    value.loans = Set::from([loan]);
                }
                if !exclusive && self.facts.construction.contains(&src.index) {
                    for (&index, alias) in std::rc::Rc::make_mut(&mut state.slots) {
                        if self.facts.construction.contains(&index) && alias.overlaps(&value) {
                            alias.invalid = true;
                        }
                    }
                }
                state.insert(dst.index, value);
            }
            Op::Literal(dst, _) if dst.ty.reference_type().is_some() || dst.ty.is_owner() => {
                state.insert(dst.index, Reference::default());
            }
            Op::EndLocal(local) => {
                let roots: Set = state[&local.index]
                    .origins
                    .iter()
                    .map(|origin| origin.root)
                    .collect();
                if alive.iter().any(|index| {
                    state
                        .get(index)
                        .is_some_and(|value| !value.invalid && value.has_root(&roots))
                }) || state
                    .arena_retained
                    .values()
                    .chain(state.regions.values())
                    .flat_map(|region| region.values())
                    .any(|(value, _)| value.has_root(&roots))
                {
                    return Err(type_error(
                        self.span(),
                        ErrorCode::InvalidAssignment,
                        "A local borrow cannot outlive its binding's scope",
                    ));
                }

                for value in std::rc::Rc::make_mut(&mut state.slots).values_mut() {
                    if value.has_root(&roots) {
                        value.invalid = true;
                    }
                }
            }
            Op::Closure(op) => {
                if let closures::ClosureOp::Move { dst, src } = op.as_ref() {
                    if src.ty.is_callable_parameter() {
                        let value = state[&src.index].clone();
                        state.insert(dst.index, value);
                        // Callable moves preserve capture origins, but end the old
                        // abstract value just like concrete environment moves.
                        if src.index != dst.index && !src.ty.is_copy_value() {
                            state.get_mut(&src.index).unwrap().invalid = true;
                        }
                    }
                }
                if let closures::ClosureOp::BorrowCapture { dst, reference } = op.as_ref() {
                    let value = state[&reference.index].clone();
                    for dst in refs(dst) {
                        state.insert(dst.index, value.clone());
                    }
                }
            }
            Op::Place(op) => match op {
                PlaceOp::View { reference, dst, .. } => {
                    self.check_access(
                        reference,
                        dst.ty.reference_type().unwrap().mutable,
                        state,
                        self.live,
                        None,
                        &[],
                    )?;
                    // Keep the whole array root and any enclosing storage loan.
                    // Dynamic indices (including empty ranges) never split loans.
                    let mut result = state[&reference.index].clone();
                    result.storage = true;
                    result.interior = true;
                    result.ancestors.extend(&result.loans);
                    result.loans = Set::from([loan]);
                    state.insert(dst.index, result);
                }
                PlaceOp::WriteNext { reference, value }
                | PlaceOp::Store {
                    reference, value, ..
                } => {
                    self.check_access(reference, true, state, self.live, None, &[])?;
                    for input in value.iter().flat_map(refs) {
                        self.check_access(&input, true, state, self.live, None, &[])?;
                        state.get_mut(&input.index).unwrap().invalid = true;
                    }
                }
                PlaceOp::Copy { dst, src } | PlaceOp::MoveInit { dst, src, .. }
                    if dst.ty.is_owner() =>
                {
                    if dst.index != src.index {
                        if state.get(&dst.index).is_some_and(|v| !v.invalid) {
                            self.check_access(dst, true, state, alive, Some(src.index), &[])?;
                        }
                        self.check_access(src, true, state, alive, Some(dst.index), &[])?;
                        state.get_mut(&src.index).unwrap().invalid = true;
                        state.insert(
                            dst.index,
                            Reference {
                                origins: BTreeSet::from([Origin::new(loan)]),
                                loans: Set::from([loan]),
                                storage: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                PlaceOp::Begin(dst) | PlaceOp::Allocate(dst) if dst.ty.is_owner() => {
                    state.insert(
                        dst.index,
                        Reference {
                            origins: BTreeSet::from([Origin::new(loan)]),
                            loans: Set::from([loan]),
                            storage: true,
                            ..Default::default()
                        },
                    );
                }
                PlaceOp::End(dst) if dst.ty.is_owner() => {
                    if state.get(&dst.index).is_some_and(|v| !v.invalid) {
                        self.check_access(dst, true, state, alive, None, &[])?;
                        state.get_mut(&dst.index).unwrap().invalid = true;
                    }
                }
                PlaceOp::Read { dst, reference, .. } if dst.ty.is_owner() => {
                    self.check_access(reference, true, state, alive, Some(dst.index), &[])?;
                    // Flattened aggregates may have several owning payload slots.
                    // Register every lane, including inactive enum lanes, just as
                    // ordinary aggregate returns do.
                    for dst in refs(dst) {
                        state.insert(
                            dst.index,
                            Reference {
                                origins: BTreeSet::from([Origin::new(loan)]),
                                loans: Set::from([loan]),
                                storage: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                PlaceOp::Write {
                    reference, value, ..
                } if value.ty.is_owner() => {
                    self.check_access(reference, true, state, alive, Some(value.index), &[])?;
                    for value in refs(value) {
                        self.check_access(&value, true, state, alive, Some(reference.index), &[])?;
                        state.get_mut(&value.index).unwrap().invalid = true;
                    }
                }
                PlaceOp::Load { reference, .. } => {
                    self.check_access(reference, false, state, self.live, None, &[])?;
                }
                _ => {}
            },
            Op::Local { dst, value, target } => {
                let mut arenas = Set::new();
                if target.is_closure_value() || target.contains_references() {
                    let contents = union(
                        value
                            .iter()
                            .flat_map(refs)
                            .filter_map(|s| state.get(&s.index).cloned()),
                    );
                    // Borrowed closures capture the local pointer's place. Its
                    // stored reference must keep the explicit arena alive even
                    // after the original scalar reference is no longer live.
                    arenas = contents.arenas.clone();
                    state.contents.insert(loan, contents);
                }
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        arenas,
                        loans: Set::from([loan]),
                        storage: target.is_place(),
                        ..Default::default()
                    },
                );
            }
            Op::Alloc { dst, value, .. } => {
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    self.check_storage(input, state)?;
                    self.check_access(input, false, state, self.live, None, &[])?;
                    let value = state[&input.index].clone();
                    retain(&mut state.retained, (loan, input.index), value, false);
                }
                let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                state
                    .contents
                    .entry(loan)
                    .and_modify(|old| {
                        merge(old, &contents);
                    })
                    .or_insert(contents);
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    },
                );
            }
            Op::StoreOwner {
                reference, value, ..
            } => {
                self.check_access(reference, true, state, self.live, None, &[])?;
                for input in value.iter().flat_map(refs) {
                    self.check_access(&input, false, state, self.live, None, &[])?;
                }
            }
            Op::ReadMemory {
                reference,
                dst,
                path,
            } => {
                self.check_access(reference, false, state, self.live, None, path)?;
                let source = state[&reference.index].clone();
                let mut value = union(source.origins.iter().map(|origin| {
                    state
                        .contents
                        .get(&origin.root)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                }));
                value.ancestors.extend(&source.ancestors);
                value.ancestors.extend(&source.loans);
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                for dst in dst.iter().flat_map(refs) {
                    state.insert(dst.index, value.clone());
                }
            }
            Op::WriteMemory {
                reference,
                value,
                path,
                target,
            } => {
                self.check_access(reference, true, state, self.live, None, path)?;
                let destination = &state[&reference.index];
                let arenas = if !destination.origins.is_empty()
                    && destination
                        .origins
                        .iter()
                        .all(|origin| self.facts.explicit_allocation_roots.contains(&origin.root))
                {
                    destination.arenas.clone()
                } else {
                    Set::new()
                };
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    // Checked closure assignment writes private local storage. Its
                    // contents keep their lexical roots, never an arena retention.
                    if !target.is_closure_value() {
                        if arenas.is_empty() {
                            self.check_storage(input, state)?;
                        } else {
                            self.check_arena_storage(input, &arenas, state, false)?;
                        }
                    }
                    self.check_access(input, false, state, self.live, None, &[])?;
                    if !target.is_closure_value() {
                        let value = state[&input.index].clone();
                        if arenas.is_empty() {
                            retain(&mut state.retained, (loan, input.index), value, false);
                        } else {
                            for arena in &arenas {
                                retain_arena(state, *arena, (loan, input.index), value.clone());
                            }
                        }
                    }
                }
                let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                for root in state[&reference.index]
                    .origins
                    .iter()
                    .map(|origin| origin.root)
                    .collect::<Set>()
                {
                    state
                        .contents
                        .entry(root)
                        .and_modify(|old| {
                            merge(old, &contents);
                        })
                        .or_insert_with(|| contents.clone());
                    if self.facts.local_roots.contains(&root) {
                        for alias in std::rc::Rc::make_mut(&mut state.slots).values_mut() {
                            if alias.origins.iter().any(|origin| origin.root == root) {
                                alias.arenas.extend(&contents.arenas);
                            }
                        }
                    }
                }
            }
            Op::Buffer(_) | Op::ReadOwner { .. } => self.buffers(state, alive)?,
            Op::Concurrent { .. } | Op::TaskBranch { .. } | Op::TaskJoin { .. } => {
                self.tasks(state, alive)?
            }
            Op::Call {
                name, args, result, ..
            } => self.call(name, args, result, state, alive)?,
            _ => {}
        }
        Ok(())
    }
}
