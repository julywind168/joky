//! Control-flow reference and buffer ownership analysis. Proven nonescaping parameters
//! are excluded from conservative call dependencies; no address inequality is assumed.
use super::graph::GraphOp;
use super::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
type Set = BTreeSet<usize>;

#[derive(Default)]
struct Escapes {
    returned: Set,
    retained: Set,
    returns_interior: bool,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Projection {
    Field(String),
    Pattern(usize),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Origin {
    root: usize,
    path: Vec<Projection>,
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
    /// This loan addresses reclaimable owner storage, even if its type is &Int32.
    storage: bool,
    /// Derived from element/Slice storage rather than a stable owning object.
    interior: bool,
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
                    origin
                        .path
                        .extend(path.iter().cloned().map(Projection::Field));
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
/// Loans held for child tasks of one dynamic task scope, keyed by scope id.
/// They end at that scope's join; nested scopes keep their own entries.
type RegionLoans = BTreeMap<(usize, usize), (Reference, bool)>;

#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    slots: BTreeMap<usize, Reference>,
    // Stored loans live until the root arena is released, even if their values die.
    retained: BTreeMap<(usize, usize), (Reference, bool)>,
    regions: BTreeMap<u32, RegionLoans>,
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
        .filter_map(|slot| {
            if slot.ty.is_callable_parameter() {
                Some(Slot {
                    index: slot.index,
                    ty: Type::reference(Type::Unit, false),
                })
            } else if slot.ty.reference_type().is_some() || slot.ty.is_buffer() {
                Some(slot)
            } else {
                None
            }
        })
        .collect()
}

fn operands(op: &Op) -> (Vec<Slot>, Vec<Slot>) {
    match op {
        Op::Closure(op) => match op.as_ref() {
            closures::ClosureOp::BorrowCapture { dst, reference } => (refs(reference), refs(dst)),
            closures::ClosureOp::Access(slot)
            | closures::ClosureOp::End(slot)
            | closures::ClosureOp::Consume(slot) => (refs(slot), vec![]),
            closures::ClosureOp::Move { dst, src } if src.ty.is_callable_parameter() => {
                (refs(src), refs(dst))
            }
            _ => (vec![], vec![]),
        },
        Op::Text { dst, source } => (
            match source {
                TextSource::Literal(_) => vec![],
                TextSource::Clone(src) => refs(src),
                // Float scalars carry no borrow; only the owned result matters.
                TextSource::Float(_) => vec![],
            },
            refs(dst),
        ),
        Op::Buffer(super::BufferOp::Storage { args, dst, .. }) => (
            args.iter().flat_map(refs).collect(),
            dst.iter().flat_map(refs).collect(),
        ),
        Op::Buffer(super::BufferOp::SwapReferences { left, right, .. }) => {
            ([left, right].into_iter().flat_map(refs).collect(), vec![])
        }
        Op::Buffer(super::BufferOp::Swap {
            reference,
            value,
            place,
        }) => (
            std::iter::once(reference)
                .chain(std::iter::once(value))
                .chain(place)
                .flat_map(refs)
                .collect(),
            refs(value),
        ),
        Op::Buffer(super::BufferOp::Move { dst, src }) => (refs(src), refs(dst)),
        Op::Buffer(super::BufferOp::Extract { dst, owner, .. }) => (refs(owner), refs(dst)),
        Op::Buffer(super::BufferOp::Detach { dst, src, .. }) => (refs(src), refs(dst)),
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
        Op::TaskBranch { captures, .. } => {
            (captures.iter().flatten().flat_map(refs).collect(), vec![])
        }
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
// Calls flatten aggregates; source parameter effects must follow the same layout.
fn call_arguments<'a>(
    info: &FunctionInfo,
    name: &str,
    args: &'a [Option<Slot>],
) -> Vec<(Option<usize>, &'a Option<Slot>)> {
    let known_layout = !info
        .signature
        .params
        .iter()
        .flat_map(Type::scalar_types)
        .any(|ty| matches!(ty, Type::Parameter(_) | Type::Projection(_)));
    if known_layout {
        info.signature
            .params
            .iter()
            .enumerate()
            .flat_map(|(index, ty)| std::iter::repeat_n(Some(index), ty.parameter_slots()))
            .zip(args)
            .collect()
    } else {
        args.iter()
            .enumerate()
            .map(|(index, arg)| {
                (
                    if name.starts_with("__joky_callable_") && index == 0 {
                        Some(0)
                    } else {
                        None
                    },
                    arg,
                )
            })
            .collect()
    }
}

fn merge(target: &mut Reference, value: &Reference) {
    target.origins.extend(value.origins.iter().cloned());
    target.loans.extend(&value.loans);
    target
        .ancestors
        .retain(|loan| value.ancestors.contains(loan));
    target.invalid |= value.invalid;
    target.storage |= value.storage;
    target.interior |= value.interior;
}
fn union(values: impl Iterator<Item = Reference>) -> Reference {
    let mut result = Reference::default();
    for value in values {
        result.origins.extend(value.origins);
        result.loans.extend(value.loans);
        result.ancestors.extend(value.ancestors);
        result.invalid |= value.invalid;
        result.storage |= value.storage;
        result.interior |= value.interior;
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
fn retain_region(
    state: &mut State,
    scope: u32,
    key: (usize, usize),
    value: Reference,
    mutable: bool,
) {
    let region = state.regions.entry(scope).or_default();
    if let Some((old, exclusive)) = region.get_mut(&key) {
        merge(old, &value);
        *exclusive |= mutable;
    } else {
        region.insert(key, (value, mutable));
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
                target.storage |= value.storage;
                target.interior |= value.interior;
            }
        }
    }
    for (&key, (value, mutable)) in &from.retained {
        retain(to, key, value.clone(), *mutable);
    }
    for (&scope, region) in &from.regions {
        for (&key, (value, mutable)) in region {
            retain_region(to, scope, key, value.clone(), *mutable);
        }
    }
    for (&key, value) in &from.contents {
        to.contents
            .entry(key)
            .and_modify(|old| merge(old, value))
            .or_insert_with(|| value.clone());
    }
    *to != old
}

/// Prove parameter guarantees together, including mutually recursive call cycles.
/// Starting with all candidates and removing disproved ones computes a greatest
/// fixed point: a recursive call cannot hide a store or return in any member.
pub(super) fn infer_module(
    functions: &mut HashMap<String, FunctionInfo>,
    bodies: &HashMap<String, FunctionPlan>,
) -> Result<(), Error> {
    let mut callers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pending = BTreeSet::new();
    for (name, body) in bodies {
        let info = functions.get_mut(name).unwrap();
        if !info.infer_noescape {
            continue;
        }
        info.inferred_retained = Some(Vec::new());
        info.returns_interior = false;
        info.inferred_noescape = info
            .signature
            .params
            .iter()
            .enumerate()
            .filter(|(index, ty)| {
                !info.noescape
                    && !info.noescape_params.contains(index)
                    && refs(&Slot {
                        index: 0,
                        ty: (*ty).clone(),
                    })
                    .iter()
                    .any(|s| !s.ty.is_buffer())
            })
            .map(|(index, _)| index)
            .collect();
        if info.signature.ret.contains_references()
            || info.signature.params.iter().any(|ty| {
                !refs(&Slot {
                    index: 0,
                    ty: ty.clone(),
                })
                .is_empty()
            })
        {
            pending.insert(name.clone());
        }
        for op in &body.ops {
            let callees: Vec<_> = match op {
                Op::Call { name, .. } => vec![name],
                Op::Scope { branches, .. } => branches.iter().map(|b| &b.name).collect(),
                Op::TaskBranch { entry, .. } => vec![entry],
                _ => vec![],
            };
            for callee in callees {
                callers
                    .entry(callee.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
    let mut budget = 20_000_000usize;
    while let Some(name) = pending.pop_first() {
        let info = &functions[&name];
        let body = &bodies[&name];
        budget = budget.checked_sub(body.ops.len().max(1)).ok_or_else(|| {
            type_error(
                &info.span,
                ErrorCode::ResourceLimit,
                "Borrow effect inference exceeds its compilation budget",
            )
        })?;
        let escaped = RefCell::new(Escapes::default());
        let result = analyze(body, info, functions, &body.type_registry, Some(&escaped));
        // An invalid body cannot establish any guarantees. Run ordinary checking
        // after convergence for the original diagnostic, never publish partial proof.
        if let Err(error) = &result {
            if error.code() == ErrorCode::ResourceLimit {
                return Err(error.clone());
            }
        }
        let escaped = escaped.into_inner();
        let mut slot = 0;
        let mut rejected = Set::new();
        let mut retained = Set::new();
        for (index, ty) in info.signature.params.iter().enumerate() {
            let resolved = if let Some(reference) = ty.reference_type() {
                Type::reference(
                    body.pointee(&reference.target, &body.type_registry)?,
                    reference.mutable,
                )
            } else {
                ty.clone()
            };
            let roots: Set = refs(&Slot {
                index: slot,
                ty: resolved,
            })
            .iter()
            .flat_map(|s| {
                let mut roots = vec![s.index];
                if reference_buffer(&s.ty) {
                    roots.push(body.slot_count + body.ops.len() + s.index);
                }
                roots
            })
            .collect();
            slot += ty.parameter_slots();
            if result.is_err() || !roots.is_disjoint(&escaped.retained) {
                retained.insert(index);
                rejected.insert(index);
            }
            if !roots.is_disjoint(&escaped.returned) {
                rejected.insert(index);
            }
        }
        let info = functions.get_mut(&name).unwrap();
        let previous = (
            info.inferred_noescape.len(),
            info.inferred_retained.as_ref().unwrap().len(),
            info.returns_interior,
        );
        info.inferred_noescape
            .retain(|index| !rejected.contains(index));
        let stored = info.inferred_retained.as_mut().unwrap();
        for index in retained {
            if !stored.contains(&index) {
                stored.push(index);
            }
        }
        info.returns_interior |= result.is_err() || escaped.returns_interior;
        if previous
            != (
                info.inferred_noescape.len(),
                stored.len(),
                info.returns_interior,
            )
        {
            pending.extend(callers.get(&name).into_iter().flatten().cloned());
        }
    }
    Ok(())
}

pub(super) fn check(
    plan: &FunctionPlan,
    info: &FunctionInfo,
    functions: &HashMap<String, FunctionInfo>,
    types_registry: &TypeRegistry,
) -> Result<(), Error> {
    analyze(plan, info, functions, types_registry, None)
}

fn analyze(
    plan: &FunctionPlan,
    info: &FunctionInfo,
    functions: &HashMap<String, FunctionInfo>,
    types_registry: &TypeRegistry,
    escaped: Option<&RefCell<Escapes>>,
) -> Result<(), Error> {
    let record_escape = |value: Option<&Reference>, returned: bool| {
        if let (Some(escaped), Some(value)) = (escaped, value) {
            let mut escaped = escaped.borrow_mut();
            if returned {
                escaped.returns_interior |= value.interior;
            }
            let roots = if returned {
                &mut escaped.returned
            } else {
                &mut escaped.retained
            };
            roots.extend(value.origins.iter().map(|origin| origin.root));
        }
    };
    let n = plan.ops.len();
    let resolved = |ty: &Type| -> Result<Type, Error> {
        Ok(if let Some(r) = ty.reference_type() {
            Type::reference(plan.pointee(&r.target, types_registry)?, r.mutable)
        } else {
            ty.clone()
        })
    };
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
    let mut argument_roots = Set::new();
    let mut index = 0;
    for (parameter, ty) in info.signature.params.iter().enumerate() {
        for slot in refs(&Slot {
            index,
            ty: resolved(ty)?,
        }) {
            if !slot.ty.is_buffer() && info.parameter_noescape(parameter) {
                argument_roots.insert(slot.index);
            }
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
            types.insert(slot.index, slot.ty.clone());
            initial.insert(
                slot.index,
                Reference {
                    origins: BTreeSet::from([Origin::new(slot.index)]),
                    loans: Set::from([slot.index]),
                    storage: slot.ty.is_buffer() || slot.ty.is_buffer_view(),
                    ..Default::default()
                },
            );
        }
        index += ty.parameter_slots();
    }
    for op in &plan.ops {
        let (uses, def) = operands(op);
        for slot in uses.into_iter().chain(def) {
            types.insert(slot.index, resolved(&slot.ty)?);
        }
    }
    for (pc, op) in plan.ops.iter().enumerate() {
        if matches!(op, Op::Text { .. })
            || matches!(op, Op::Call { result: Some(dst), .. } | Op::Buffer(super::BufferOp::Storage { dst: Some(dst), .. }) if dst.ty.is_owner())
            || matches!(op, Op::Buffer(super::BufferOp::Swap { .. }))
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
                && !r.target.is_closure_value()
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
            for (alias, mutable) in state
                .retained
                .values()
                .chain(state.regions.values().flat_map(|region| region.values()))
            {
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
            record_escape(state.get(&slot.index), false);
            if escaped.is_none()
                && state
                    .get(&slot.index)
                    .is_some_and(|v| v.has_root(&argument_roots))
            {
                return Err(type_error(
                    span,
                    ErrorCode::InvalidAssignment,
                    "@noescape function cannot retain an argument borrow",
                ));
            }
            if slot.ty.contains_buffers() || state.get(&slot.index).is_some_and(|v| v.storage) {
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
        let record_child_escapes = |name: &str, args: &[Option<Slot>]| {
            let callee = &functions[name];
            if !callee.host_text {
                for (index, arg) in call_arguments(callee, name, args) {
                    if index.map_or(!callee.noescape, |i| callee.parameter_retained(i)) {
                        for slot in arg.iter().flat_map(refs) {
                            record_escape(state.get(&slot.index), false);
                        }
                    }
                }
            }
        };
        if pc == n {
            if let Some(result) = &plan.result {
                for slot in refs(result) {
                    record_escape(state.get(&slot.index), true);
                    if escaped.is_none()
                        && state
                            .get(&slot.index)
                            .is_some_and(|v| v.has_root(&argument_roots))
                    {
                        return Err(type_error(
                            span,
                            ErrorCode::InvalidAssignment,
                            "@noescape function cannot return an argument borrow",
                        ));
                    }
                    if !slot.ty.is_buffer()
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
        let suspending = match &plan.ops[pc] {
            Op::Call { name, .. } => functions[name].suspends,
            Op::Scope { .. } | Op::TaskBranch { .. } | Op::TaskJoin { .. } => true,
            _ => false,
        };
        if suspending
            && live[pc]
                .iter()
                .any(|index| types.get(index).is_some_and(Type::contains_dyn))
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "A dyn view cannot remain live across suspension or enter concurrency",
            ));
        }
        // Places and compiler-only write reservations survive a poll. Source
        // loans to Copy locals must end before yielding. Reference-free whole
        // owners have stable storage; interior views are checked separately.
        // Checked TCP output lanes are pinned UInt64 places. The private adapter
        // retains them only until synchronous stop, before parent-place cleanup.
        let tcp_output = |index: &usize| {
            matches!(&plan.ops[pc], Op::Call { name, args, .. }
            if functions[name].tcp.is_some() && args.iter().flatten().any(|arg| arg.index == *index)
            && types[index].reference_type().is_some_and(|r| r.mutable && r.target == Type::UInt64))
        };
        if suspending
            && live[pc].iter().any(|index| {
                !local_places.contains(index)
                    && !tcp_output(index)
                    && !types[index].is_suspend_safe_view()
                    && state.get(index).is_some_and(|value| {
                        !value.invalid
                            && value.has_root(&local_roots)
                            && !value.loans.is_subset(&reservations)
                    })
            })
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "A local borrow cannot remain live across suspension",
            ));
        }
        if suspending
            && live[pc].iter().any(|index| {
                types[index].reference_type().is_some()
                    && !local_places.contains(index)
                    && state
                        .get(index)
                        .is_some_and(|value| !value.invalid && value.interior)
            })
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "An interior element or Slice borrow cannot remain live across suspension",
            ));
        }
        if suspending
            && live[pc].iter().any(|index| {
                types[index].reference_type().is_some()
                    && !types[index].is_buffer_view()
                    && state.get(index).is_some_and(|value| {
                        !value.invalid && value.storage && value.has_root(&owner_roots)
                    })
            })
        {
            return Err(type_error(
                span,
                ErrorCode::InvalidCall,
                "A borrowed owner or element view cannot remain live across suspension",
            ));
        }
        let loan = plan.slot_count + pc;
        match &plan.ops[pc] {
            Op::Text { dst, source } => {
                if let TextSource::Clone(src) = source {
                    check_access(src, false, &state, &live[pc], None, &[])?;
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
            Op::Buffer(super::BufferOp::SwapReferences { left, right, .. }) => {
                check_access(left, true, &state, &live[pc], None, &[])?;
                check_access(right, true, &state, &live[pc], None, &[])?;
                let left_contents = buffer_contents(&state, left);
                let right_contents = buffer_contents(&state, right);
                let left_identity = state[&left.index].clone();
                let right_identity = state[&right.index].clone();
                add_contents(&mut state, &left_identity, &right_contents);
                add_contents(&mut state, &right_identity, &left_contents);
            }
            Op::Buffer(super::BufferOp::Swap {
                reference, value, ..
            }) => {
                check_access(reference, true, &state, &live[pc], None, &[])?;
                let old_contents = buffer_contents(&state, reference);
                let new_contents = union(
                    refs(value)
                        .iter()
                        .map(|input| buffer_contents(&state, input)),
                );
                let identity = state[&reference.index].clone();
                add_contents(&mut state, &identity, &new_contents);
                for input in refs(value) {
                    check_access(&input, true, &state, &live[pc], None, &[])?;
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
                    add_contents(&mut state, &identity, &old_contents);
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
            Op::PatternProject { dst, src, path, .. } => {
                // Validate availability separately from access: sibling pattern
                // bindings are disjoint even when their parent is an array loan.
                if state.get(&src.index).is_none_or(|v| v.invalid) {
                    check_access(src, false, &state, &alive, None, &[])?;
                }
                let mut value = state[&src.index].clone();
                value.storage |= src.ty.is_buffer_view();
                value.origins = value
                    .origins
                    .into_iter()
                    .map(|mut origin| {
                        origin
                            .path
                            .extend(path.iter().copied().map(Projection::Pattern));
                        origin
                    })
                    .collect();
                value.ancestors.extend(&value.loans);
                value.loans = Set::from([loan]);
                state.insert(dst.index, value);
                check_access(
                    dst,
                    dst.ty.reference_type().unwrap().mutable,
                    &state,
                    &alive,
                    None,
                    &[],
                )?;
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
            Op::Closure(op) => {
                if let closures::ClosureOp::Move { dst, src } = op.as_ref() {
                    if src.ty.is_callable_parameter() {
                        let value = state[&src.index].clone();
                        state.insert(dst.index, value);
                    }
                }
                if let closures::ClosureOp::BorrowCapture { dst, reference } = op.as_ref() {
                    let value = state[&reference.index].clone();
                    for dst in refs(dst) {
                        state.insert(dst.index, value.clone());
                    }
                }
            }
            Op::Local { dst, value, target } => {
                if target.is_closure_value() || target.contains_references() {
                    let contents = union(
                        value
                            .iter()
                            .flat_map(refs)
                            .filter_map(|s| state.get(&s.index).cloned()),
                    );
                    state.contents.insert(loan, contents);
                }
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
                        ownership_path,
                        dst,
                        reference,
                        ..
                    } => (
                        owner,
                        owner.field_path(ownership_path),
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
                let detach = matches!(&plan.ops[pc], Op::ReadOwner { detach: true, .. });
                let consumed = if detach { &selected } else { owner };
                for input in refs(consumed) {
                    check_access(&input, true, &state, &alive, None, &[])?;
                }
                let values: Vec<_> = refs(&selected)
                    .iter()
                    .map(|src| state[&src.index].clone())
                    .collect();
                // Record projections consume the whole source; tuple patterns
                // detach each leaf through Detach and end the remainder after.
                for input in refs(consumed) {
                    state.get_mut(&input.index).unwrap().invalid = true;
                }
                for (dst, value) in outputs.into_iter().zip(values) {
                    state.insert(dst.index, value);
                }
            }
            Op::Buffer(super::BufferOp::Detach { dst, src, .. }) => {
                // Irrefutable destructuring detaches each leaf in turn. Only
                // the selected lanes are read here; the explicit End that
                // follows releases the remainder and ends the binding.
                for input in refs(src) {
                    check_access(&input, true, &state, &alive, None, &[])?;
                }
                let values: Vec<_> = refs(src)
                    .iter()
                    .map(|slot| state[&slot.index].clone())
                    .collect();
                for input in refs(src) {
                    state.get_mut(&input.index).unwrap().invalid = true;
                }
                for (dst, value) in refs(dst).into_iter().zip(values) {
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
                target,
            } => {
                check_access(reference, true, &state, &live[pc], None, path)?;
                let inputs: Vec<_> = value.iter().flat_map(refs).collect();
                for input in &inputs {
                    // Checked closure assignment writes private local storage. Its
                    // contents keep their lexical roots, never an arena retention.
                    if !target.is_closure_value() {
                        check_storage(input, &state)?;
                    }
                    check_access(input, false, &state, &live[pc], None, &[])?;
                    if !target.is_closure_value() {
                        let value = state[&input.index].clone();
                        retain(&mut state, (loan, input.index), value, false);
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
                        .and_modify(|old| merge(old, &contents))
                        .or_insert_with(|| contents.clone());
                }
            }
            Op::Scope { branches, .. } => {
                for branch in branches {
                    record_child_escapes(&branch.name, &branch.args);
                }
                if alive.iter().any(|index| {
                    types[index].is_buffer_view()
                        && !types[index].is_suspend_safe_view()
                        && state
                            .get(index)
                            .is_some_and(|value| !value.invalid && value.has_root(&owner_roots))
                }) {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "A borrowed text view cannot remain live across suspension",
                    ));
                }

                // Scope/race is one structured boundary in the caller CFG. All
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
                            span,
                            ErrorCode::InvalidCall,
                            "Buffer owners and views cannot enter scope or race",
                        ));
                    }
                    check_access(
                        &arg,
                        arg.ty.is_suspend_safe_owner()
                            || arg.ty.reference_type().is_some_and(|r| r.mutable),
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
                    .filter(|arg| {
                        arg.ty.reference_type().is_some() && !arg.ty.is_suspend_safe_view()
                    })
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
                for arg in branches
                    .iter()
                    .flat_map(|branch| branch.args.iter().flatten())
                    .flat_map(refs)
                    .filter(|arg| arg.ty == Type::String || arg.ty.is_suspend_safe_owner())
                {
                    state.get_mut(&arg.index).unwrap().invalid = true;
                }
            }
            // A dynamic branch registration snapshots its captures here while
            // the parent body keeps running. Reference captures stay live as
            // scope-tagged loans until the scope's join; allocation-capable
            // entries impose the same task-long storage contract as calls.
            Op::TaskBranch {
                scope,
                entry,
                captures,
                ..
            } => {
                record_child_escapes(entry, captures);
                if alive.iter().any(|index| {
                    types[index].is_buffer_view()
                        && !types[index].is_suspend_safe_view()
                        && state
                            .get(index)
                            .is_some_and(|value| !value.invalid && value.has_root(&owner_roots))
                }) {
                    return Err(type_error(
                        span,
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
                            span,
                            ErrorCode::InvalidCall,
                            "Buffer owners and views cannot enter scope or race",
                        ));
                    }
                    check_access(
                        capture,
                        capture.ty.is_suspend_safe_owner()
                            || capture.ty.reference_type().is_some_and(|r| r.mutable),
                        &state,
                        &live[pc],
                        None,
                        &[],
                    )?;
                }
                for capture in capture_refs
                    .iter()
                    .filter(|c| c.ty.reference_type().is_some())
                {
                    let value = state[&capture.index].clone();
                    if !capture.ty.is_suspend_safe_view() {
                        check_storage(capture, &state)?;
                    }
                    retain_region(
                        &mut state,
                        *scope,
                        (loan, capture.index),
                        value,
                        capture.ty.reference_type().unwrap().mutable,
                    );
                    if functions[entry].uses_arena && !capture.ty.is_suspend_safe_view() {
                        let value = state[&capture.index].clone();
                        retain(
                            &mut state,
                            (loan, capture.index),
                            value,
                            capture.ty.reference_type().unwrap().mutable,
                        );
                    }
                }
                for capture in capture_refs
                    .iter()
                    .filter(|c| c.ty == Type::String || c.ty.is_suspend_safe_owner())
                {
                    state.get_mut(&capture.index).unwrap().invalid = true;
                }
            }
            // Joining ends the scope's child-task loans on every path through
            // the join, including abrupt exits routed here before jumping on.
            Op::TaskJoin { scope, .. } => {
                state.regions.remove(scope);
            }
            Op::Call {
                name, args, result, ..
            } => {
                if functions[name].suspends
                    && alive.iter().any(|index| {
                        types[index].is_buffer_view()
                            && !types[index].is_suspend_safe_view()
                            && state
                                .get(index)
                                .is_some_and(|value| !value.invalid && value.has_root(&owner_roots))
                    })
                {
                    return Err(type_error(
                        span,
                        ErrorCode::InvalidCall,
                        "A borrowed text view cannot remain live across suspension",
                    ));
                }
                let arguments = call_arguments(&functions[name], name, args);
                let noescape = |index: Option<usize>| {
                    index.map_or(functions[name].noescape, |index| {
                        functions[name].parameter_noescape(index)
                    })
                };
                let mut noescape_inputs: Set = arguments
                    .iter()
                    .copied()
                    .filter(|(index, _)| noescape(*index))
                    .flat_map(|(_, arg)| arg.iter().flat_map(refs).map(|slot| slot.index))
                    .collect();
                // An aliased argument passed to an unpromised parameter still
                // escapes, even when another parameter promises noescape.
                for slot in arguments
                    .iter()
                    .copied()
                    .filter(|(index, _)| !noescape(*index))
                    .flat_map(|(_, arg)| arg.iter().flat_map(refs))
                {
                    noescape_inputs.remove(&slot.index);
                }
                let retained_inputs: Set = arguments
                    .iter()
                    .copied()
                    .filter(|(index, _)| {
                        index.map_or(!functions[name].noescape, |index| {
                            functions[name].parameter_retained(index)
                        })
                    })
                    .flat_map(|(_, arg)| arg.iter().flat_map(refs).map(|slot| slot.index))
                    .collect();
                // Returning a view and saving one are distinct effects. Unknown
                // host/indirect calls remain conservative, including wrappers
                // that never request an arena. Text views have a checked ABI.
                if !functions[name].host_text {
                    for arg in args.iter().flatten().flat_map(refs) {
                        if retained_inputs.contains(&arg.index) {
                            record_escape(state.get(&arg.index), false);
                        }
                    }
                }
                let inputs: Vec<_> = args
                    .iter()
                    .flatten()
                    .flat_map(refs)
                    .map(|mut slot| {
                        slot.ty = types[&slot.index].clone();
                        slot
                    })
                    .collect();
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
                        if arg.ty.contains_buffers() || !retained_inputs.contains(&arg.index) {
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
                for mut dst in result.iter().flat_map(refs) {
                    dst.ty = types[&dst.index].clone();
                    let mutable =
                        dst.ty.is_buffer() || dst.ty.reference_type().is_some_and(|r| r.mutable);
                    let mut value = Reference {
                        origins: BTreeSet::from([Origin::new(loan)]),
                        loans: Set::from([loan]),
                        storage: dst.ty.is_buffer() || dst.ty.is_buffer_view(),
                        // Unknown ordinary host references retain their existing ABI
                        // contract. Unknown owner views must remain conservative.
                        interior: !dst.ty.is_buffer()
                            && functions[name].returns_interior
                            && (functions[name].inferred_retained.is_some()
                                || dst.ty.is_buffer_view()),
                        ..Default::default()
                    };
                    for arg in &inputs {
                        if noescape_inputs.contains(&arg.index) && !arg.ty.is_buffer() {
                            continue;
                        }
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
                        } else if reference_buffer(&arg.ty) {
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
                            value.storage |= source.storage;
                            value.interior |= source.interior;
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
