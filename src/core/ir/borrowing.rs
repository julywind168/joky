//! Control-flow reference and buffer ownership analysis. Calls use signature-wide input dependencies;
//! no body summaries, addresses, or allocation-site inequality promises are used.
use super::graph::GraphOp;
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
type Set = BTreeSet<usize>;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Origin {
    root: usize,
    path: Vec<String>,
}
impl Origin {
    fn new(root: usize) -> Self {
        Self {
            root,
            path: Vec::new(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Reference {
    origins: BTreeSet<Origin>,
    loans: Set,
    ancestors: Set,
    invalid: bool,
}
impl Reference {
    fn has_root(&self, roots: &Set) -> bool {
        self.origins
            .iter()
            .any(|origin| roots.contains(&origin.root))
    }

    fn projected(&self, path: &[String], local_roots: &Set) -> Self {
        let mut value = self.clone();
        value.origins = value
            .origins
            .into_iter()
            .map(|mut origin| {
                // Field splitting is limited to proven local storage. Arena and
                // parameter origins retain their existing whole-object contract.
                if local_roots.contains(&origin.root) {
                    origin.path.extend_from_slice(path);
                }
                origin
            })
            .collect();
        value
    }

    fn overlaps(&self, other: &Self) -> bool {
        // Lexicographic ordering keeps descendants contiguous. This merge walk
        // checks prefix overlap without a quadratic product of possible paths.
        let mut left = self.origins.iter().peekable();
        let mut right = other.origins.iter().peekable();
        while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
            if a.root == b.root && (a.path.starts_with(&b.path) || b.path.starts_with(&a.path)) {
                return true;
            }
            if a < b {
                left.next();
            } else {
                right.next();
            }
        }
        false
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    slots: BTreeMap<usize, Reference>,
    // Stored loans live until the root arena is released, even if their values die.
    retained: BTreeMap<(usize, usize), (Reference, bool)>,
    // Object/control identity and stored reference dependencies stay separate:
    // copying an array element does not borrow its relocatable payload.
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
        .filter(|s| s.ty.reference_type().is_some() || s.ty.is_buffer())
        .collect()
}
fn operands(op: &Op) -> (Vec<Slot>, Vec<Slot>) {
    match op {
        Op::Buffer(super::BufferOp::Storage { args, dst, .. }) => (
            args.iter().flat_map(refs).collect(),
            dst.iter().flat_map(refs).collect(),
        ),
        Op::Buffer(super::BufferOp::Move { dst, src }) => (refs(src), refs(dst)),
        Op::Buffer(super::BufferOp::Extract { dst, owner, .. }) => (refs(owner), refs(dst)),
        Op::Buffer(super::BufferOp::Replace {
            dst,
            src,
            owner,
            place,
            ..
        }) => (
            [owner, src, place].into_iter().flat_map(refs).collect(),
            refs(dst),
        ),
        Op::Buffer(super::BufferOp::Borrow { dst, src, place }) => (
            std::iter::once(src).chain(place).flat_map(refs).collect(),
            refs(dst),
        ),
        Op::Buffer(super::BufferOp::Access { src, .. }) => (refs(src), vec![]),
        Op::Buffer(super::BufferOp::End(owner)) => (refs(owner), vec![]),
        Op::Graph(op) => match op.as_ref() {
            GraphOp::Link {
                reference, source, ..
            } => (vec![reference.clone(), source.clone()], vec![]),
            GraphOp::Seal { dst, members, .. } => (members.clone(), refs(dst)),
        },
        Op::Reference { dst, src, .. }
        | Op::Reserve { dst, src }
        | Op::Project { dst, src, .. }
        | Op::PatternProject { dst, src, .. }
        | Op::Copy(dst, src) => (refs(src), refs(dst)),
        Op::Alloc { dst, value, .. } | Op::Local { dst, value, .. } => {
            (value.iter().flat_map(refs).collect(), refs(dst))
        }
        Op::EndLocal(local) => (refs(local), vec![]),
        Op::ReadMemory { dst, reference, .. } => {
            (refs(reference), dst.iter().flat_map(refs).collect())
        }
        Op::ReadOwner {
            dst,
            reference,
            owner,
            ..
        } => (
            std::iter::once(reference)
                .chain(std::iter::once(owner))
                .flat_map(refs)
                .collect(),
            dst.iter().flat_map(refs).collect(),
        ),
        Op::WriteMemory {
            reference, value, ..
        }
        | Op::StoreOwner {
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
        Op::Scope { branches, .. } => (
            branches
                .iter()
                .flat_map(|branch| branch.args.iter().flatten())
                .flat_map(refs)
                .collect(),
            vec![],
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
    target.origins.extend(value.origins.iter().cloned());
    target.loans.extend(&value.loans);
    target
        .ancestors
        .retain(|loan| value.ancestors.contains(loan));
    target.invalid |= value.invalid;
}
fn union(values: impl Iterator<Item = Reference>) -> Reference {
    let mut result = Reference::default();
    for value in values {
        result.origins.extend(value.origins);
        result.loans.extend(value.loans);
        result.ancestors.extend(value.ancestors);
        result.invalid |= value.invalid;
    }
    result
}
fn buffer_contents(state: &State, slot: &Slot) -> Reference {
    union(
        state[&slot.index]
            .origins
            .iter()
            .filter_map(|origin| state.contents.get(&origin.root).cloned()),
    )
}
fn add_contents(state: &mut State, identity: &Reference, contents: &Reference) {
    for origin in &identity.origins {
        state
            .contents
            .entry(origin.root)
            .and_modify(|old| merge(old, contents))
            .or_insert_with(|| contents.clone());
    }
}
fn reference_buffer(ty: &Type) -> bool {
    ty.buffer_elements()
        .iter()
        .any(|element| element.contains_references())
}
fn writable_buffer(ty: &Type) -> bool {
    reference_buffer(ty) && (ty.is_buffer() || ty.reference_type().is_some_and(|r| r.mutable))
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
                target.origins.extend(value.origins.iter().cloned());
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
    let local_roots: Set = plan
        .ops
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| matches!(op, Op::Local { .. }).then_some(plan.slot_count + pc))
        .collect();
    let local_places: Set = plan
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Local { dst, .. } => Some(dst.index),
            _ => None,
        })
        .collect();
    let mut types = BTreeMap::new();
    let mut initial = State::default();
    let mut owner_roots = Set::new();
    let mut index = 0;
    for ty in &info.signature.params {
        for slot in refs(&Slot {
            index,
            ty: ty.clone(),
        }) {
            if slot.ty.is_buffer() {
                owner_roots.insert(slot.index);
            }
            if reference_buffer(&slot.ty) {
                // Parameter contents have an independent, task-lived origin.
                // Call sites substitute the actual stored loans, not this view's
                // control-block loan. These ids cannot collide with CFG roots.
                let content_root = plan.slot_count + n + slot.index;
                initial.contents.insert(
                    slot.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(content_root)]),
                        loans: Set::from([content_root]),
                        ..Default::default()
                    },
                );
            }
            types.insert(slot.index, slot.ty);
            initial.insert(
                slot.index,
                Reference {
                    origins: BTreeSet::from([Origin::new(slot.index)]),
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
    for (pc, op) in plan.ops.iter().enumerate() {
        if matches!(op, Op::Call { result: Some(dst), .. } | Op::Buffer(super::BufferOp::Storage { dst: Some(dst), .. }) if dst.ty.is_owner())
        {
            owner_roots.insert(plan.slot_count + pc);
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
        .filter_map(|(pc, op)| {
            matches!(
                op,
                Op::Reserve { .. }
                    | Op::Project {
                        reservation: true,
                        ..
                    }
            )
            .then_some(plan.slot_count + pc)
        })
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
                .map(|r| {
                    1 + r
                        .origins
                        .iter()
                        .map(|origin| 1 + origin.path.len())
                        .sum::<usize>()
                        + r.loans.len()
                        + r.ancestors.len()
                })
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
                            ignore: Option<usize>,
                            path: &[String]|
         -> Result<(), Error> {
            let value = state
                .get(&slot.index)
                .filter(|v| !v.invalid)
                .ok_or_else(|| {
                    type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        if slot.ty.is_buffer() {
                            "Use of a moved or uninitialized buffer owner"
                        } else {
                            "Use of a moved, sealed or uninitialized exclusive reference"
                        },
                    )
                })?;
            let projection;
            let value = if path.is_empty() {
                value
            } else {
                projection = value.projected(path, &local_roots);
                &projection
            };
            let conflict = |alias: &Reference, mutable: bool| {
                if !value.overlaps(alias) {
                    return false;
                }
                let ancestor = alias.loans.is_subset(&value.ancestors)
                    && alias.loans.is_disjoint(&value.loans);
                !ancestor && (exclusive || mutable && !alias.loans.is_subset(&reservations))
            };
            for &other in alive {
                // Local storage and unique buffer owners are not competing loans.
                // Access through an owner still checks every live view; lifetime
                // markers keeping the owner live must not freeze its views.
                if other == slot.index
                    || Some(other) == ignore
                    || local_places.contains(&other)
                    || types[&other].is_buffer()
                {
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
        let check_storage = |slot: &Slot, state: &State| -> Result<(), Error> {
            if slot.ty.contains_buffers() {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "Buffer owners and views cannot be retained by the task arena",
                ));
            }
            if state
                .get(&slot.index)
                .is_some_and(|value| value.has_root(&local_roots))
            {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "A local borrow cannot be retained by the task arena",
                ));
            }
            Ok(())
        };
        if pc == n {
            if let Some(result) = &plan.result {
                for slot in refs(result) {
                    if slot.ty.is_buffer_view()
                        && state
                            .get(&slot.index)
                            .is_some_and(|value| value.has_root(&owner_roots))
                    {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidAssignment,
                            "Cannot return a view of a local or by-value buffer owner",
                        ));
                    }
                    if state
                        .get(&slot.index)
                        .is_some_and(|value| value.has_root(&local_roots))
                    {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidAssignment,
                            "Cannot return a reference to a local binding",
                        ));
                    }
                    check_access(
                        &slot,
                        slot.ty.is_buffer() || slot.ty.reference_type().is_some_and(|r| r.mutable),
                        &state,
                        &live[pc],
                        None,
                        &[],
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
            Op::Buffer(super::BufferOp::Storage {
                intrinsic,
                args,
                dst,
                ..
            }) => {
                use crate::parser::ast::BufferIntrinsic;
                for arg in args.iter().flat_map(refs) {
                    check_access(
                        &arg,
                        arg.ty.is_buffer() || arg.ty.reference_type().unwrap().mutable,
                        &state,
                        &live[pc],
                        None,
                        &[],
                    )?;
                }
                let mut result = Reference {
                    origins: BTreeSet::from([Origin::new(loan)]),
                    loans: Set::from([loan]),
                    ..Default::default()
                };
                match intrinsic {
                    intrinsic if intrinsic.borrows_storage() => {
                        result = state[&args[0].index].clone();
                        result.ancestors.extend(&result.loans);
                        result.loans = Set::from([loan]);
                    }
                    BufferIntrinsic::Load
                    | BufferIntrinsic::ElementLoad
                    | BufferIntrinsic::SliceLoad => {
                        result = buffer_contents(&state, &args[0]);
                        result.ancestors.extend(&result.loans);
                        result.loans = Set::from([loan]);
                    }
                    BufferIntrinsic::Store
                    | BufferIntrinsic::InitializeNext
                    | BufferIntrinsic::ElementStore
                    | BufferIntrinsic::SliceStore => {
                        let inputs = refs(args.last().unwrap());
                        for arg in inputs.iter().filter(|arg| !arg.ty.is_buffer()) {
                            check_storage(arg, &state)?;
                            let value = state[&arg.index].clone();
                            retain(&mut state, (loan, arg.index), value, false);
                        }
                        let contents = union(inputs.iter().map(|s| state[&s.index].clone()));
                        let identity = state[&args[0].index].clone();
                        add_contents(&mut state, &identity, &contents);
                    }
                    BufferIntrinsic::Replace => {
                        let contents = buffer_contents(&state, &args[1]);
                        let identity = state[&args[0].index].clone();
                        add_contents(&mut state, &identity, &contents);
                    }
                    BufferIntrinsic::Transfer => {
                        let contents = buffer_contents(&state, &args[0]);
                        let identity = state[&args[1].index].clone();
                        add_contents(&mut state, &identity, &contents);
                    }
                    _ => {}
                }
                // Reclaiming buffer storage never revokes copied content
                // references: their shared loans remain until the root ends.
                for arg in args.iter().filter(|arg| arg.ty.is_owner()).flat_map(refs) {
                    state.get_mut(&arg.index).unwrap().invalid = true;
                }
                for dst in dst.iter().flat_map(refs) {
                    state.insert(dst.index, result.clone());
                }
            }
            Op::Buffer(super::BufferOp::Move { dst, src }) => {
                for (dst, src) in refs(dst).into_iter().zip(refs(src)) {
                    if dst.index != src.index && state.get(&dst.index).is_some_and(|v| !v.invalid) {
                        check_access(&dst, true, &state, &alive, Some(src.index), &[])?;
                    }
                    check_access(&src, true, &state, &alive, Some(dst.index), &[])?;
                    let value = state[&src.index].clone();
                    if dst.index != src.index {
                        state.get_mut(&src.index).unwrap().invalid = true;
                    }
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(super::BufferOp::Access { src, mutable }) => {
                for src in refs(src) {
                    check_access(&src, *mutable, &state, &live[pc], None, &[])?;
                }
            }
            Op::Buffer(super::BufferOp::Borrow { dst, src, place }) => {
                let inputs = refs(src);
                for src in &inputs {
                    check_access(
                        src,
                        dst.ty.reference_type().unwrap().mutable,
                        &state,
                        &alive,
                        Some(dst.index),
                        &[],
                    )?;
                }
                let mut value = union(inputs.iter().map(|src| state[&src.index].clone()));
                if let Some(place) = place {
                    check_access(
                        place,
                        dst.ty.reference_type().unwrap().mutable,
                        &state,
                        &alive,
                        Some(dst.index),
                        &[],
                    )?;
                    let storage = &state[&place.index];
                    value.origins.extend(storage.origins.iter().cloned());
                    value.ancestors.extend(&storage.ancestors);
                    value.ancestors.extend(&storage.loans);
                }
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::Buffer(super::BufferOp::End(owner)) => {
                for owner in refs(owner) {
                    // Moving out ends this binding without ending the destination owner.
                    if state.get(&owner.index).is_some_and(|v| !v.invalid) {
                        check_access(&owner, true, &state, &alive, None, &[])?;
                        state.get_mut(&owner.index).unwrap().invalid = true;
                    }
                }
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
                            .and_modify(|old| merge(old, &contents))
                            .or_insert_with(|| contents.clone());
                    }
                }
                GraphOp::Seal { dst, root, members } => {
                    let mut value = state[&root.index].clone();
                    value.ancestors.extend(&value.loans);
                    value.loans = Set::from([loan]);
                    for member in members {
                        let contents = state[&member.index].clone();
                        retain(&mut state, (loan, member.index), contents, false);
                        state.get_mut(&member.index).unwrap().invalid = true;
                    }
                    state.insert(dst.index, value);
                }
            },
            Op::Reserve { dst, src } => {
                check_access(src, false, &state, &alive, Some(dst.index), &[])?;
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
                check_access(
                    src,
                    !reservation && dst.ty.reference_type().unwrap().mutable,
                    &state,
                    &alive,
                    Some(dst.index),
                    path,
                )?;
                let mut value = state[&src.index].projected(path, &local_roots);
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::PatternProject { dst, src, .. } => {
                check_access(src, false, &state, &alive, Some(dst.index), &[])?;
                let mut value = state[&src.index].clone();
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
            }
            Op::Reference { dst, src, moving } => {
                let exclusive = dst.ty.reference_type().unwrap().mutable;
                check_access(src, exclusive, &state, &alive, Some(dst.index), &[])?;
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
                        if construction.contains(&index) && alias.overlaps(&value) {
                            alias.invalid = true;
                        }
                    }
                }
                state.insert(dst.index, value);
            }
            Op::Literal(dst, _) if dst.ty.reference_type().is_some() || dst.ty.is_buffer() => {
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
                }) {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidAssignment,
                        "A local borrow cannot outlive its binding's scope",
                    ));
                }

                for value in state.slots.values_mut() {
                    if value.has_root(&roots) {
                        value.invalid = true;
                    }
                }
            }
            Op::Local { dst, .. } => {
                state.insert(
                    dst.index,
                    Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    },
                );
            }
            Op::Alloc { dst, value, .. } => {
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    check_storage(input, &state)?;
                    check_access(input, false, &state, &live[pc], None, &[])?;
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
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    },
                );
            }
            Op::ReadOwner { .. } | Op::Buffer(super::BufferOp::Extract { .. }) => {
                let (owner, selected, dst, reference) = match &plan.ops[pc] {
                    Op::ReadOwner {
                        owner,
                        path,
                        dst,
                        reference,
                    } => (
                        owner,
                        owner.field_path(path),
                        dst.as_ref().unwrap(),
                        Some(reference),
                    ),
                    Op::Buffer(super::BufferOp::Extract { owner, src, dst }) => {
                        (owner, src.clone(), dst, None)
                    }
                    _ => unreachable!(),
                };
                let outputs = refs(dst);
                if let Some(reference) = reference {
                    check_access(reference, true, &state, &alive, Some(outputs[0].index), &[])?;
                }
                // Consuming tuple patterns partition a fresh tuple temporary.
                // Its leaves are detached in order, so prior leaves are already moved.
                let consumed = if reference.is_none() && matches!(owner.ty, Type::Tuple(_)) {
                    &selected
                } else {
                    owner
                };
                for input in refs(consumed) {
                    check_access(&input, true, &state, &alive, None, &[])?;
                }
                let values: Vec<_> = refs(&selected)
                    .iter()
                    .map(|src| state[&src.index].clone())
                    .collect();
                // Record/enum projections consume the whole source. Tuple
                // patterns detach each leaf and end the remainder after binding.
                for input in refs(consumed) {
                    state.get_mut(&input.index).unwrap().invalid = true;
                }
                for (dst, value) in outputs.into_iter().zip(values) {
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(super::BufferOp::Replace {
                dst,
                src,
                owner,
                place,
                ..
            }) => {
                check_access(place, true, &state, &live[pc], None, &[])?;
                for input in refs(owner).into_iter().chain(refs(src)) {
                    check_access(&input, true, &state, &live[pc], None, &[])?;
                }
                for (dst, src) in refs(dst).into_iter().zip(refs(src)) {
                    let value = state[&src.index].clone();
                    state.get_mut(&src.index).unwrap().invalid = true;
                    state.insert(dst.index, value);
                }
            }
            Op::StoreOwner {
                reference, value, ..
            } => {
                check_access(reference, true, &state, &live[pc], None, &[])?;
                for input in value.iter().flat_map(refs) {
                    check_access(&input, false, &state, &live[pc], None, &[])?;
                }
            }
            Op::ReadMemory {
                reference,
                dst,
                path,
            } => {
                check_access(reference, false, &state, &live[pc], None, path)?;
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
                ..
            } => {
                check_access(reference, true, &state, &live[pc], None, path)?;
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    check_storage(input, &state)?;
                    check_access(input, false, &state, &live[pc], None, &[])?;
                    let value = state[&input.index].clone();
                    retain(&mut state, (loan, input.index), value, false);
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
                        .and_modify(|old| merge(old, &contents))
                        .or_insert_with(|| contents.clone());
                }
            }
            Op::Scope { branches, .. } => {
                // Scope/race is one structured boundary in the caller CFG. All
                // argument snapshots stay live until branches complete or cancel,
                // including while later arguments are evaluated or suspend.
                for arg in branches
                    .iter()
                    .flat_map(|branch| branch.args.iter().flatten())
                    .flat_map(refs)
                {
                    if arg.ty.contains_buffers() {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidCall,
                            "Buffer owners and views cannot enter scope or race",
                        ));
                    }
                    check_access(
                        &arg,
                        arg.ty.reference_type().unwrap().mutable,
                        &state,
                        &live[pc],
                        None,
                        &[],
                    )?;
                }
                // Validate all branches before retaining any inputs. Ordinary
                // loans end at the join; allocation-capable branches impose
                // the same task-long storage contract as sequential calls.
                for arg in branches
                    .iter()
                    .filter(|branch| functions[&branch.name].uses_arena)
                    .flat_map(|branch| branch.args.iter().flatten())
                    .flat_map(refs)
                {
                    let value = state[&arg.index].clone();
                    check_storage(&arg, &state)?;
                    retain(
                        &mut state,
                        (loan, arg.index),
                        value,
                        arg.ty.reference_type().unwrap().mutable,
                    );
                }
            }
            Op::Call {
                name, args, result, ..
            } => {
                let inputs: Vec<_> = args.iter().flatten().flat_map(refs).collect();
                for arg in &inputs {
                    check_access(
                        arg,
                        arg.ty.is_buffer() || arg.ty.reference_type().is_some_and(|r| r.mutable),
                        &state,
                        &live[pc],
                        None,
                        &[],
                    )?;
                }
                // Writable reference-element buffers impose a storage contract
                // even in allocation-free helpers (notably set). This follows
                // the signature, so aliases/generics/wrappers cannot hide it.
                if functions[name].uses_arena || inputs.iter().any(|arg| writable_buffer(&arg.ty)) {
                    for arg in &inputs {
                        // Owner/view types cannot be stored: allocation does not
                        // turn their temporary loans into task-long retention.
                        if arg.ty.contains_buffers() {
                            continue;
                        }
                        check_storage(arg, &state)?;
                        let value = state[&arg.index].clone();
                        retain(
                            &mut state,
                            (loan, arg.index),
                            value,
                            arg.ty.reference_type().unwrap().mutable,
                        );
                    }
                }
                let contents = union(inputs.iter().map(|arg| {
                    if !arg.ty.buffer_elements().is_empty() {
                        buffer_contents(&state, arg)
                    } else {
                        state[&arg.index].clone()
                    }
                }));
                for arg in inputs.iter().filter(|arg| writable_buffer(&arg.ty)) {
                    let identity = state[&arg.index].clone();
                    add_contents(&mut state, &identity, &contents);
                }
                for dst in result.iter().flat_map(refs) {
                    let mutable =
                        dst.ty.is_buffer() || dst.ty.reference_type().is_some_and(|r| r.mutable);
                    let mut value = Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        ..Default::default()
                    };
                    for arg in &inputs {
                        let source = if dst.ty.is_buffer() {
                            // Owner identity flows only from consumed owners.
                            if !arg.ty.is_buffer() {
                                continue;
                            }
                            state[&arg.index].clone()
                        } else if dst.ty.is_buffer_view() {
                            // Preserve the existing signature-wide dependency
                            // contract for returned views, including ordinary
                            // reference inputs. Owners still cannot lend a view.
                            if arg.ty.is_buffer() {
                                continue;
                            }
                            state[&arg.index].clone()
                        } else if !arg.ty.buffer_elements().is_empty() {
                            // Shared element values can outlive this owner/view;
                            // they never grant exclusive access to the referent.
                            if mutable {
                                continue;
                            }
                            buffer_contents(&state, arg)
                        } else {
                            state[&arg.index].clone()
                        };
                        if !mutable
                            || arg.ty.is_buffer()
                            || arg.ty.reference_type().is_some_and(|r| r.mutable)
                        {
                            value.origins.extend(source.origins.iter().cloned());
                        }
                        value.ancestors.extend(&source.ancestors);
                        value.ancestors.extend(&source.loans);
                    }
                    if dst.ty.is_buffer() && reference_buffer(&dst.ty) {
                        add_contents(&mut state, &value, &contents);
                    }
                    state.insert(dst.index, value);
                }
                for arg in inputs.iter().filter(|arg| arg.ty.is_buffer()) {
                    state.get_mut(&arg.index).unwrap().invalid = true;
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
