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
#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    slots: BTreeMap<usize, Reference>,
    // Stored loans live until the root arena is released, even if their values die.
    retained: BTreeMap<(usize, usize), (Reference, bool)>,
    contents: BTreeMap<usize, Reference>,
}
impl std::ops::Deref for State {
    type Target = BTreeMap<usize, Reference>;
    fn deref(&self) -> &Self::Target {
        &self.slots
    }
}
impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.slots
    }
}
fn refs(slot: &Slot) -> Vec<Slot> {
    slot.scalars()
        .filter(|s| s.ty.reference_type().is_some())
        .collect()
}
fn operands(op: &Op) -> (Vec<Slot>, Vec<Slot>) {
    match op {
        Op::Reference { dst, src, .. } | Op::Reserve { dst, src } | Op::Copy(dst, src) => {
            (refs(src), refs(dst))
        }
        Op::Alloc { dst, value, .. } => (value.iter().flat_map(refs).collect(), refs(dst)),
        Op::ReadMemory { dst, reference, .. } => {
            (refs(reference), dst.iter().flat_map(refs).collect())
        }
        Op::WriteMemory {
            reference, value, ..
        } => (
            std::iter::once(reference)
                .chain(value)
                .flat_map(refs)
                .collect(),
            vec![],
        ),
        Op::Call { args, result, .. } => (
            args.iter().flatten().flat_map(refs).collect(),
            result.iter().flat_map(refs).collect(),
        ),
        Op::Literal(dst, _) => (vec![], refs(dst)),
        _ => (vec![], vec![]),
    }
}
impl FunctionPlan {
    pub(crate) fn has_references(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|s| s.ty.contains_references())
            || self.ops.iter().any(|op| {
                let (used, defined) = operands(op);
                !used.is_empty() || !defined.is_empty()
            })
    }
}
fn merge(target: &mut Reference, value: &Reference) {
    target.roots.extend(&value.roots);
    target.loans.extend(&value.loans);
    target
        .ancestors
        .retain(|loan| value.ancestors.contains(loan));
    target.invalid |= value.invalid;
}
fn union(values: impl Iterator<Item = Reference>) -> Reference {
    let mut result = Reference::default();
    for value in values {
        result.roots.extend(value.roots);
        result.loans.extend(value.loans);
        result.ancestors.extend(value.ancestors);
        result.invalid |= value.invalid;
    }
    result
}
fn retain(state: &mut State, key: (usize, usize), value: Reference, mutable: bool) {
    if let Some((old, exclusive)) = state.retained.get_mut(&key) {
        merge(old, &value);
        *exclusive |= mutable;
    } else {
        state.retained.insert(key, (value, mutable));
    }
}
fn join(to: &mut State, from: &State) -> bool {
    let old = to.clone();
    for (&slot, value) in &from.slots {
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
    for (&key, (value, mutable)) in &from.retained {
        retain(to, key, value.clone(), *mutable);
    }
    for (&key, value) in &from.contents {
        to.contents
            .entry(key)
            .and_modify(|old| merge(old, value))
            .or_insert_with(|| value.clone());
    }
    *to != old
}

pub(super) fn check(
    plan: &FunctionPlan,
    info: &FunctionInfo,
    functions: &HashMap<String, FunctionInfo>,
    types_registry: &TypeRegistry,
) -> Result<(), Error> {
    let n = plan.ops.len();
    let mut types = BTreeMap::new();
    let mut initial = State::default();
    let mut index = 0;
    for ty in &info.signature.params {
        for slot in refs(&Slot {
            index,
            ty: ty.clone(),
        }) {
            types.insert(slot.index, slot.ty);
            initial.insert(
                slot.index,
                Reference {
                    roots: Set::from([slot.index]),
                    loans: Set::from([slot.index]),
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
        live[n].extend(refs(result).into_iter().map(|s| s.index));
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
            for dst in def {
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
    let mut construction = Set::new();
    for (&index, ty) in &types {
        if let Some(r) = ty.reference_type() {
            if r.mutable
                && plan
                    .pointee(&r.target, types_registry)?
                    .contains_references()
            {
                construction.insert(index);
            }
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
        let input = states[pc].as_ref().unwrap();
        let cost = types.len()
            + input
                .slots
                .values()
                .chain(input.contents.values())
                .chain(input.retained.values().map(|(r, _)| r))
                .map(|r| 1 + r.roots.len() + r.loans.len() + r.ancestors.len())
                .sum::<usize>();
        budget = budget.checked_sub(cost).ok_or_else(|| {
            type_error(
                &plan.span,
                ErrorCode::ResourceLimit,
                "Reference flow analysis exceeds its compilation budget",
            )
        })?;
        let mut state = input.clone();
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
                        "Use of a moved, sealed or uninitialized exclusive reference",
                    )
                })?;
            let conflict = |alias: &Reference, mutable: bool| {
                if value.roots.is_disjoint(&alias.roots) {
                    return false;
                }
                let ancestor = alias.loans.is_subset(&value.ancestors)
                    && alias.loans.is_disjoint(&value.loans);
                !ancestor && (exclusive || mutable && !alias.loans.is_subset(&reservations))
            };
            for &other in alive {
                if other == slot.index || Some(other) == ignore {
                    continue;
                }
                if let Some(alias) = state.get(&other).filter(|v| !v.invalid) {
                    if conflict(
                        alias,
                        types[&other].reference_type().is_some_and(|r| r.mutable),
                    ) {
                        return Err(type_error(span, ErrorCode::InvalidAssignment,
                            "Reference access conflicts with a live borrow; finish using the borrowed view first"));
                    }
                }
            }
            for (alias, mutable) in state.retained.values() {
                if conflict(alias, *mutable) {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        "Reference access conflicts with a borrow retained by the task arena",
                    ));
                }
            }
            Ok(())
        };
        if pc == n {
            if let Some(result) = &plan.result {
                for slot in refs(result) {
                    check_access(
                        &slot,
                        slot.ty.reference_type().unwrap().mutable,
                        &state,
                        &live[pc],
                        None,
                    )?;
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
                if !exclusive && construction.contains(&src.index) {
                    for (&index, alias) in &mut state.slots {
                        if construction.contains(&index) && !alias.roots.is_disjoint(&value.roots) {
                            alias.invalid = true;
                        }
                    }
                }
                state.insert(dst.index, value);
            }
            Op::Literal(dst, _) if dst.ty.reference_type().is_some() => {
                state.insert(dst.index, Reference::default());
            }
            Op::Alloc { dst, value, .. } => {
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    check_access(input, false, &state, &live[pc], None)?;
                    let value = state[&input.index].clone();
                    retain(&mut state, (loan, input.index), value, false);
                }
                let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                state
                    .contents
                    .entry(loan)
                    .and_modify(|old| merge(old, &contents))
                    .or_insert(contents);
                state.insert(
                    dst.index,
                    Reference {
                        roots: Set::from([loan]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    },
                );
            }
            Op::ReadMemory { reference, dst, .. } => {
                check_access(reference, false, &state, &live[pc], None)?;
                let source = state[&reference.index].clone();
                let mut value = union(source.roots.iter().map(|root| {
                    state
                        .contents
                        .get(root)
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
                reference, value, ..
            } => {
                check_access(reference, true, &state, &live[pc], None)?;
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    check_access(input, false, &state, &live[pc], None)?;
                    let value = state[&input.index].clone();
                    retain(&mut state, (loan, input.index), value, false);
                }
                let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                for root in state[&reference.index].roots.clone() {
                    state
                        .contents
                        .entry(root)
                        .and_modify(|old| merge(old, &contents))
                        .or_insert_with(|| contents.clone());
                }
            }
            Op::Call {
                name, args, result, ..
            } => {
                let inputs: Vec<_> = args.iter().flatten().flat_map(refs).collect();
                for arg in &inputs {
                    check_access(
                        arg,
                        arg.ty.reference_type().unwrap().mutable,
                        &state,
                        &live[pc],
                        None,
                    )?;
                }
                if functions[name].uses_arena {
                    for arg in &inputs {
                        let value = state[&arg.index].clone();
                        retain(
                            &mut state,
                            (loan, arg.index),
                            value,
                            arg.ty.reference_type().unwrap().mutable,
                        );
                    }
                }
                for dst in result.iter().flat_map(refs) {
                    let mutable = dst.ty.reference_type().unwrap().mutable;
                    let mut value = Reference {
                        roots: Set::from([loan]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    };
                    for arg in &inputs {
                        let source = &state[&arg.index];
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
