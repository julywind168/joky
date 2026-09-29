//! Control-flow reference analysis. Calls use signature-wide input dependencies;
//! no body summaries, addresses, or allocation-site inequality promises are used.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
type Set = BTreeSet<usize>;

#[derive(Clone, Default, PartialEq, Eq)]
struct Reference {
    roots: Set,
    loans: Set,
    ancestors: Set,
    invalid: bool,
}
type State = BTreeMap<usize, Reference>;

fn operands(op: &Op) -> (Vec<&Slot>, Option<&Slot>) {
    match op {
        Op::Reference { dst, src, .. } | Op::Reserve { dst, src } => (vec![src], Some(dst)),
        Op::Alloc { dst, .. } => (vec![], Some(dst)),
        Op::ReadMemory { reference, .. } | Op::WriteMemory { reference, .. } => {
            (vec![reference], None)
        }
        Op::Call { args, result, .. } => (
            args.iter()
                .flatten()
                .filter(|s| s.ty.reference_type().is_some())
                .collect(),
            result.as_ref().filter(|s| s.ty.reference_type().is_some()),
        ),
        _ => (vec![], None),
    }
}
fn join(to: &mut State, from: &State) -> bool {
    let old = to.clone();
    for (&slot, value) in from {
        match to.entry(slot) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(value.clone());
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let target = entry.get_mut();
                target.roots.extend(&value.roots);
                target.loans.extend(&value.loans);
                // An ancestry permission must hold on every incoming path.
                target
                    .ancestors
                    .retain(|loan| value.ancestors.contains(loan));
                target.invalid |= value.invalid;
            }
        }
    }
    *to != old
}

pub(super) fn check(plan: &FunctionPlan, info: &FunctionInfo) -> Result<(), Error> {
    let n = plan.ops.len();
    let mut types = BTreeMap::new();
    let mut initial = State::new();
    let mut index = 0;
    for ty in &info.signature.params {
        if ty.reference_type().is_some() {
            types.insert(index, ty.clone());
            initial.insert(
                index,
                Reference {
                    roots: Set::from([index]),
                    loans: Set::from([index]),
                    ..Default::default()
                },
            );
        }
        index += ty.parameter_slots();
    }
    for op in &plan.ops {
        let (uses, def) = operands(op);
        for slot in uses.into_iter().chain(def) {
            types.insert(slot.index, slot.ty.clone());
        }
    }
    if types.is_empty() {
        return Ok(());
    }
    // Explicit caps make adversarial control-flow analyses fail closed.
    if types.len() > 4096 || n > 100_000 {
        return Err(type_error(
            &plan.span,
            ErrorCode::ResourceLimit,
            "Reference analysis exceeds its compilation budget",
        ));
    }
    let mut labels = vec![n; plan.label_count];
    for (pc, op) in plan.ops.iter().enumerate() {
        if let Op::Label(label) = op {
            labels[*label] = pc;
        }
    }
    let next: Vec<Vec<usize>> = plan
        .ops
        .iter()
        .enumerate()
        .map(|(pc, op)| match op {
            Op::Jump(label) => vec![labels[*label]],
            Op::Branch {
                then_label,
                else_label,
                ..
            } => vec![labels[*then_label], labels[*else_label]],
            _ => vec![pc + 1],
        })
        .collect();
    let mut live = vec![Set::new(); n + 1];
    if let Some(result) = &plan.result {
        if result.ty.reference_type().is_some() {
            live[n].insert(result.index);
        }
    }
    let mut budget = 2_000_000usize;
    loop {
        let mut changed = false;
        for pc in (0..n).rev() {
            budget = budget.checked_sub(types.len()).ok_or_else(|| {
                type_error(
                    &plan.span,
                    ErrorCode::ResourceLimit,
                    "Reference liveness exceeds its compilation budget",
                )
            })?;
            let (uses, def) = operands(&plan.ops[pc]);
            let mut input: Set = next[pc]
                .iter()
                .flat_map(|&next| live[next].iter().copied())
                .collect();
            if let Some(dst) = def {
                input.remove(&dst.index);
            }
            input.extend(uses.iter().map(|s| s.index));
            if live[pc] != input {
                live[pc] = input;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let reservations: Set = plan
        .ops
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| matches!(op, Op::Reserve { .. }).then_some(plan.slot_count + pc))
        .collect();
    let mut states = vec![None; n + 1];
    states[0] = Some(initial);
    let mut queue = VecDeque::from([0]);
    let mut queued = vec![false; n + 1];
    queued[0] = true;
    budget = 2_000_000;
    while let Some(pc) = queue.pop_front() {
        queued[pc] = false;
        budget = budget.checked_sub(types.len()).ok_or_else(|| {
            type_error(
                &plan.span,
                ErrorCode::ResourceLimit,
                "Reference flow analysis exceeds its compilation budget",
            )
        })?;
        let mut state = states[pc].clone().unwrap();
        let span = plan.op_spans.get(pc).unwrap_or(&plan.span);
        let check_access = |slot: &Slot,
                            exclusive: bool,
                            state: &State,
                            alive: &Set,
                            ignore: Option<usize>|
         -> Result<(), Error> {
            let value = state
                .get(&slot.index)
                .filter(|v| !v.invalid)
                .ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        "Use of a moved or uninitialized exclusive reference",
                    )
                })?;
            for &other in alive {
                if other == slot.index || Some(other) == ignore {
                    continue;
                }
                let Some(alias) = state.get(&other).filter(|v| !v.invalid) else {
                    continue;
                };
                if value.roots.is_disjoint(&alias.roots) {
                    continue;
                }
                // A reborrow may use its ancestors while freezing them. Equality
                // does not count as ancestry (including repeated loop sites).
                let ancestor = alias.loans.is_subset(&value.ancestors)
                    && alias.loans.is_disjoint(&value.loans);
                if ancestor {
                    continue;
                }
                if exclusive
                    || types[&other].reference_type().is_some_and(|r| r.mutable)
                        && !alias.loans.is_subset(&reservations)
                {
                    return Err(type_error(span, ErrorCode::InvalidAssignment, "Reference access conflicts with a live borrow; finish using the borrowed view first"));
                }
            }
            Ok(())
        };
        if pc == n {
            if let Some(result) = &plan.result {
                if let Some(r) = result.ty.reference_type() {
                    check_access(result, r.mutable, &state, &live[pc], None)?;
                }
            }
            continue;
        }
        let alive: Set = next[pc]
            .iter()
            .flat_map(|&next| live[next].iter().copied())
            .collect();
        let loan = plan.slot_count + pc;
        match &plan.ops[pc] {
            Op::Reserve { dst, src } => {
                check_access(src, false, &state, &alive, Some(dst.index))?;
                let mut value = state[&src.index].clone();
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::Reference { dst, src, moving } => {
                let exclusive = dst.ty.reference_type().unwrap().mutable;
                check_access(src, exclusive, &state, &alive, Some(dst.index))?;
                let mut value = state[&src.index].clone();
                if *moving {
                    if src.ty.reference_type().unwrap().mutable && dst.index != src.index {
                        state.get_mut(&src.index).unwrap().invalid = true;
                    }
                } else {
                    value.ancestors.extend(&value.loans);
                    value.loans = Set::from([loan]);
                }
                state.insert(dst.index, value);
            }
            Op::Alloc { dst, .. } => {
                state.insert(
                    dst.index,
                    Reference {
                        roots: Set::from([loan]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    },
                );
            }
            Op::ReadMemory { reference, .. } => {
                check_access(reference, false, &state, &live[pc], None)?
            }
            Op::WriteMemory { reference, .. } => {
                check_access(reference, true, &state, &live[pc], None)?
            }
            Op::Call { args, result, .. } => {
                let refs: Vec<_> = args
                    .iter()
                    .flatten()
                    .filter(|s| s.ty.reference_type().is_some())
                    .collect();
                // Argument reborrows have already been checked when evaluated;
                // at the call all of them must still coexist and be valid.
                for arg in &refs {
                    check_access(
                        arg,
                        arg.ty.reference_type().unwrap().mutable,
                        &state,
                        &live[pc],
                        None,
                    )?;
                }
                if let Some(dst) = result.as_ref().filter(|s| s.ty.reference_type().is_some()) {
                    let mutable = dst.ty.reference_type().unwrap().mutable;
                    let mut value = Reference {
                        roots: Set::from([loan]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    };
                    for arg in refs {
                        let source = &state[&arg.index];
                        // Mutable results can alias any exclusive input, never a shared one.
                        if !mutable || arg.ty.reference_type().unwrap().mutable {
                            value.roots.extend(&source.roots);
                        }
                        value.ancestors.extend(&source.ancestors);
                        value.ancestors.extend(&source.loans);
                    }
                    state.insert(dst.index, value);
                }
            }
            _ => {}
        }
        for &target in &next[pc] {
            let changed = if let Some(existing) = &mut states[target] {
                join(existing, &state)
            } else {
                states[target] = Some(state.clone());
                true
            };
            if changed && !queued[target] {
                queue.push_back(target);
                queued[target] = true;
            }
        }
    }
    Ok(())
}
