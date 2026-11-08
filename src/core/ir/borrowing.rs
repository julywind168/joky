//! Control-flow reference and buffer ownership analysis. Proven nonescaping parameters
//! are excluded from conservative call dependencies; no address inequality is assumed.
mod liveness;
mod transfer;
use super::graph::GraphOp;
use super::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
type Set = BTreeSet<usize>;

#[derive(Default)]
struct Escapes {
    returned: Set,
    retained: Set,
    arena_retained: BTreeMap<usize, Set>,
    returns_interior: bool,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Projection {
    Field(String),
    Pattern(usize),
    Entry(usize),
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
    /// Explicit arena lifetimes are distinct from allocation alias identities.
    arenas: Set,
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
            || !self.arenas.is_disjoint(roots)
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
/// Retained loans and their exclusivity, keyed by retention site and source slot.
type RegionLoans = BTreeMap<(usize, usize), (Reference, bool)>;

#[derive(Clone, Default, PartialEq, Eq)]
struct State {
    // Most instructions preserve reference slots; CFG snapshots share them until a write.
    slots: std::rc::Rc<BTreeMap<usize, Reference>>,
    // Stored loans live until the root arena is released, even if their values die.
    retained: RegionLoans,
    arena_retained: BTreeMap<usize, RegionLoans>,
    // Child-task loans end at their scope's join; nested scopes keep separate entries.
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
        std::rc::Rc::make_mut(&mut self.slots)
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
            } else if slot.ty.reference_type().is_some() || slot.ty.is_owner() {
                Some(slot)
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn operands(op: &Op) -> (Vec<Slot>, Vec<Slot>) {
    match op {
        Op::Place(op) => match op {
            PlaceOp::View {
                reference,
                dst,
                scratch,
                ..
            } => (
                std::iter::once(reference)
                    .chain(scratch)
                    .flat_map(refs)
                    .collect(),
                refs(dst),
            ),
            PlaceOp::Copy { dst, src } | PlaceOp::MoveInit { dst, src, .. } => {
                (refs(src), refs(dst))
            }
            PlaceOp::Begin(dst) | PlaceOp::Allocate(dst) => (vec![], refs(dst)),
            PlaceOp::Read { reference, dst, .. } => (refs(reference), refs(dst)),
            PlaceOp::Write {
                reference, value, ..
            } => (
                [reference, value].into_iter().flat_map(refs).collect(),
                vec![],
            ),
            PlaceOp::End(value) => (refs(value), vec![]),
            PlaceOp::WriteNext { reference, value }
            | PlaceOp::Store {
                reference, value, ..
            } => (
                std::iter::once(reference)
                    .chain(value)
                    .flat_map(refs)
                    .collect(),
                vec![],
            ),
            PlaceOp::Load { reference, dst, .. } => {
                (refs(reference), dst.iter().flat_map(refs).collect())
            }
            _ => (vec![], vec![]),
        },
        Op::OutboxEmit { bytes, .. } => (refs(bytes), vec![]),
        Op::Assert { message, .. } | Op::Panic { message } => (refs(message), vec![]),
        Op::Closure(op) => match op.as_ref() {
            closures::ClosureOp::End(slot) if slot.ty.is_scoped_suspending_closure() => {
                // This environment owns no captures and has no destructor. Its
                // lexical end must not prolong loans after a consuming call.
                (vec![], refs(slot))
            }
            closures::ClosureOp::BorrowCapture { dst, reference } => (refs(reference), refs(dst)),
            closures::ClosureOp::Access(slot)
            | closures::ClosureOp::End(slot)
            | closures::ClosureOp::Consume(slot) => (refs(slot), vec![]),
            closures::ClosureOp::Move { dst, src } if src.ty.is_callable_parameter() => {
                (refs(src), refs(dst))
            }
            _ => (vec![], vec![]),
        },
        Op::DomainOwner {
            packet,
            value,
            decode,
            ..
        } => {
            if *decode {
                (refs(packet), refs(value))
            } else {
                ([packet, value].into_iter().flat_map(refs).collect(), vec![])
            }
        }
        Op::Text { dst, source } => (
            match source {
                TextSource::Literal(_) => vec![],
                TextSource::Clone(src) => refs(src),
                // Float scalars carry no borrow; only the owned result matters.
                TextSource::Float(_) => vec![],
            },
            refs(dst),
        ),
        Op::Buffer(super::BufferOp::Storage {
            args, dst, scratch, ..
        }) => (
            args.iter().chain(scratch).flat_map(refs).collect(),
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
        Op::Buffer(super::BufferOp::Borrow {
            dst, src, place, ..
        }) => (
            std::iter::once(src).chain(place).flat_map(refs).collect(),
            refs(dst),
        ),
        Op::DynPack { owner, .. } => (refs(owner), vec![]),
        Op::Buffer(super::BufferOp::Access { src, .. }) => (refs(src), vec![]),
        Op::Buffer(super::BufferOp::End(owner)) => {
            refs(owner).into_iter().partition(|s| s.ty.is_owner())
        }
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
        | Op::EntryProject { dst, src, .. }
        | Op::Copy(dst, src) => (refs(src), refs(dst)),
        Op::Alloc { dst, value, .. } | Op::Local { dst, value, .. } => {
            (value.iter().flat_map(refs).collect(), refs(dst))
        }
        Op::ArenaNew { dst } => (vec![], refs(dst)),
        Op::ArenaAlloc {
            dst, arena, value, ..
        } => (
            std::iter::once(arena).chain(value).flat_map(refs).collect(),
            refs(dst),
        ),
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
        Op::Concurrent {
            branches, result, ..
        } => (
            branches
                .iter()
                .flat_map(|branch| branch.args.iter().flatten())
                .flat_map(refs)
                .collect(),
            result
                .iter()
                .chain(branches.iter().filter_map(|b| b.result.as_ref()))
                .flat_map(refs)
                .collect(),
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
pub(super) fn call_arguments<'a>(
    info: &FunctionInfo,
    name: &str,
    args: &'a [Option<Slot>],
) -> Vec<(Option<usize>, &'a Option<Slot>)> {
    let mut callable_parameter = None;
    let mut callable_count = 0;
    let mut unknown = false;
    let mut minimum_slots = 0;
    for (index, ty) in info.signature.params.iter().enumerate() {
        minimum_slots += ty.parameter_slots();
        let mut callable = false;
        for lane in ty.scalar_types() {
            if lane.is_callable_parameter() {
                callable = true;
            } else if matches!(lane, Type::Parameter(_) | Type::Projection(_)) {
                unknown = true;
            }
        }
        if callable {
            callable_parameter = Some(index);
            callable_count += 1;
        }
    }
    // Abstract callable values occupy one lane, but a concrete closure passed
    // while checking a template includes its capture lanes too. One flexible
    // source parameter can absorb all extra lanes without guessing boundaries.
    // Multiple flexible parameters remain conservative unless none expands.
    let extra = args.len().checked_sub(minimum_slots);
    let known_layout = !unknown && (extra == Some(0) || (extra.is_some() && callable_count == 1));
    if known_layout {
        info.signature
            .params
            .iter()
            .enumerate()
            .flat_map(|(index, ty)| {
                let width = ty.parameter_slots()
                    + if callable_parameter == Some(index) {
                        extra.unwrap_or(0)
                    } else {
                        0
                    };
                std::iter::repeat_n(Some(index), width)
            })
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

/// Task branches use the same parameter retention contract as ordinary calls.
/// Being able to allocate does not itself extend a non-retaining input loan.
fn retained_child_arguments(info: &FunctionInfo, name: &str, args: &[Option<Slot>]) -> Vec<Slot> {
    if !info.uses_arena && !args.iter().flatten().any(|arg| writable_buffer(&arg.ty)) {
        return Vec::new();
    }
    call_arguments(info, name, args)
        .into_iter()
        .filter(|(index, _)| index.map_or(!info.noescape, |i| info.parameter_retained(i)))
        .flat_map(|(_, arg)| arg.iter().flat_map(refs))
        .filter(|arg| {
            arg.ty.reference_type().is_some()
                && !arg.ty.contains_buffers()
                && !arg.ty.contains_place_storage()
        })
        .collect()
}

fn merge(target: &mut Reference, value: &Reference) -> bool {
    let before = (
        target.origins.len(),
        target.arenas.len(),
        target.loans.len(),
        target.ancestors.len(),
        target.invalid,
        target.storage,
        target.interior,
    );
    target.origins.extend(value.origins.iter().cloned());
    target.arenas.extend(&value.arenas);
    target.loans.extend(&value.loans);
    // An ancestry permission must hold on every incoming path.
    target
        .ancestors
        .retain(|loan| value.ancestors.contains(loan));
    target.invalid |= value.invalid;
    target.storage |= value.storage;
    target.interior |= value.interior;
    // Unions only grow, ancestry intersection only shrinks, and flags only set.
    before
        != (
            target.origins.len(),
            target.arenas.len(),
            target.loans.len(),
            target.ancestors.len(),
            target.invalid,
            target.storage,
            target.interior,
        )
}
fn union(values: impl Iterator<Item = Reference>) -> Reference {
    let mut result = Reference::default();
    for value in values {
        result.origins.extend(value.origins);
        result.arenas.extend(value.arenas);
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
            .and_modify(|old| {
                merge(old, contents);
            })
            .or_insert_with(|| contents.clone());
    }
}
fn reference_buffer(ty: &Type) -> bool {
    ty.buffer_elements()
        .iter()
        .any(|element| element.contains_references())
}
fn writable_buffer(ty: &Type) -> bool {
    reference_buffer(ty) && exclusive_access(ty)
}
/// Moving an owner or using an exclusive reference requires exclusive access;
/// moving a shared-reference lane does not modify its referent.
fn exclusive_access(ty: &Type) -> bool {
    ty.is_owner() || ty.reference_type().is_some_and(|r| r.mutable)
}
fn retain(loans: &mut RegionLoans, key: (usize, usize), value: Reference, mutable: bool) -> bool {
    if let Some((old, exclusive)) = loans.get_mut(&key) {
        let changed = merge(old, &value) | (mutable && !*exclusive);
        *exclusive |= mutable;
        changed
    } else {
        loans.insert(key, (value, mutable));
        true
    }
}
fn retain_region(
    state: &mut State,
    scope: u32,
    key: (usize, usize),
    value: Reference,
    mutable: bool,
) -> bool {
    retain(state.regions.entry(scope).or_default(), key, value, mutable)
}
fn explicit_arena(ty: &Type) -> bool {
    ty.is_explicit_arena()
        || ty
            .reference_type()
            .is_some_and(|reference| reference.target.is_explicit_arena())
}

fn retain_arena(state: &mut State, arena: usize, key: (usize, usize), value: Reference) -> bool {
    retain(
        state.arena_retained.entry(arena).or_default(),
        key,
        value,
        false,
    )
}

fn move_arena(state: &mut State, old: &Set, new: usize) {
    let mut region = RegionLoans::new();
    for root in old {
        if let Some(retained) = state.arena_retained.remove(root) {
            for (key, (value, mutable)) in retained {
                if let Some((previous, _)) = region.get_mut(&key) {
                    merge(previous, &value);
                } else {
                    region.insert(key, (value, mutable));
                }
            }
        }
    }
    // No external loan survives an owner move. Internal allocation dependencies
    // follow the moved region, including values stored in its own objects.
    for value in state
        .contents
        .values_mut()
        .chain(region.values_mut().map(|(value, _)| value))
    {
        if !value.arenas.is_disjoint(old) {
            value.arenas.retain(|root| !old.contains(root));
            value.arenas.insert(new);
        }
    }
    if !region.is_empty() {
        state.arena_retained.insert(new, region);
    }
}

fn join(to: &mut State, from: &State) -> bool {
    if to == from {
        return false;
    }
    let mut changed = false;
    if !std::rc::Rc::ptr_eq(&to.slots, &from.slots) {
        for (&slot, value) in from.slots.iter() {
            if to.get(&slot) == Some(value) {
                continue;
            }
            match to.entry(slot) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(value.clone());
                    changed = true;
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    changed |= merge(entry.get_mut(), value);
                }
            }
        }
    }
    for (&key, (value, mutable)) in &from.retained {
        changed |= retain(&mut to.retained, key, value.clone(), *mutable);
    }
    for (&scope, region) in &from.regions {
        for (&key, (value, mutable)) in region {
            changed |= retain_region(to, scope, key, value.clone(), *mutable);
        }
    }
    for (&arena, region) in &from.arena_retained {
        for (&key, (value, _)) in region {
            changed |= retain_arena(to, arena, key, value.clone());
        }
    }
    for (&key, value) in &from.contents {
        match to.contents.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(value.clone());
                changed = true;
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                changed |= merge(entry.get_mut(), value);
            }
        }
    }
    changed
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
        info.inferred_arena_retained.clear();
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
                    .any(|s| !s.ty.is_owner())
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
        for callee in body.ops.iter().flat_map(Op::callees) {
            callers
                .entry(callee.clone())
                .or_default()
                .insert(name.clone());
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
        let mut parameter_roots = Vec::new();
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
            parameter_roots.push(roots);
        }
        let mut arena_effects = Vec::new();
        for (source, roots) in parameter_roots.iter().enumerate() {
            for (destination, arena_roots) in parameter_roots.iter().enumerate() {
                let destination_ty = &info.signature.params[destination];
                if explicit_arena(destination_ty)
                    && !destination_ty.is_explicit_arena()
                    && arena_roots.iter().any(|root| {
                        escaped
                            .arena_retained
                            .get(root)
                            .is_some_and(|sources| !roots.is_disjoint(sources))
                    })
                {
                    rejected.insert(source);
                    arena_effects.push((source, destination));
                }
            }
        }
        let info = functions.get_mut(&name).unwrap();
        let previous = (
            info.inferred_noescape.len(),
            info.inferred_retained.as_ref().unwrap().len(),
            info.returns_interior,
            info.inferred_arena_retained.len(),
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
        for effect in arena_effects {
            if !info.inferred_arena_retained.contains(&effect) {
                info.inferred_arena_retained.push(effect);
            }
        }
        if previous
            != (
                info.inferred_noescape.len(),
                stored.len(),
                info.returns_interior,
                info.inferred_arena_retained.len(),
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
    analyze(plan, info, functions, types_registry, None).map(|_| ())
}

/// A def can inspect its old owner and the live-out aliases before writing.
/// Keep every mentioned slot through its last potential read *or write*.
/// Overlapping backward jumps form intervals: inside one, execution may
/// revisit any earlier mention as far back as the start of the interval.
struct SlotLifetime {
    last: HashMap<usize, usize>,
    floor: Vec<usize>,
}

impl SlotLifetime {
    fn new(plan: &FunctionPlan, next: &[Vec<usize>]) -> Self {
        let n = next.len();
        let mut last = HashMap::new();
        let mut back_end = vec![0; n + 1];
        for (pc, successors) in next.iter().enumerate() {
            for &target in successors.iter().filter(|&&s| s <= pc) {
                back_end[target] = back_end[target].max(pc);
            }
            let (uses, defs) = operands(&plan.ops[pc]);
            for slot in uses.into_iter().chain(defs) {
                last.insert(slot.index, pc);
            }
            // These transfer operands are implicit in their enclosing group.
            match &plan.ops[pc] {
                Op::Graph(op) => {
                    if let GraphOp::Seal { root, .. } = op.as_ref() {
                        last.insert(root.index, pc);
                    }
                }
                Op::Buffer(super::BufferOp::Extract { src, .. }) => {
                    for slot in refs(src) {
                        last.insert(slot.index, pc);
                    }
                }
                _ => {}
            }
        }
        for slot in plan.result.iter().flat_map(refs) {
            last.insert(slot.index, n);
        }
        let mut start = 0;
        let mut end = 0;
        let floor = back_end
            .into_iter()
            .enumerate()
            .map(|(pc, back)| {
                if pc > end {
                    start = pc;
                }
                end = end.max(back);
                start
            })
            .collect();
        Self { last, floor }
    }

    fn prune(&self, state: &mut State, target: usize, live: &Set) {
        let keep = |slot: &usize| {
            live.contains(slot)
                || self
                    .last
                    .get(slot)
                    .is_some_and(|last| *last >= self.floor[target])
        };
        if state.slots.keys().any(|slot| !keep(slot)) {
            std::rc::Rc::make_mut(&mut state.slots).retain(|slot, _| keep(slot));
        }
    }
}

fn analyze(
    plan: &FunctionPlan,
    info: &FunctionInfo,
    functions: &HashMap<String, FunctionInfo>,
    types_registry: &TypeRegistry,
    escaped: Option<&RefCell<Escapes>>,
) -> Result<usize, Error> {
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
    let explicit_allocation_roots: Set = plan
        .ops
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| matches!(op, Op::ArenaAlloc { .. }).then_some(plan.slot_count + pc))
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
    let mut arena_parameter_roots = Set::new();
    let mut argument_roots = Set::new();
    let mut index = 0;
    for (parameter, ty) in info.signature.params.iter().enumerate() {
        for slot in refs(&Slot {
            index,
            ty: resolved(ty)?,
        }) {
            if !slot.ty.is_owner() && info.parameter_noescape(parameter) {
                argument_roots.insert(slot.index);
            }
            if slot.ty.is_owner() {
                owner_roots.insert(slot.index);
            }
            if explicit_arena(&slot.ty) && !slot.ty.is_explicit_arena() {
                arena_parameter_roots.insert(slot.index);
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
                    arenas: if explicit_arena(&slot.ty) && !slot.ty.is_explicit_arena() {
                        Set::from([slot.index])
                    } else {
                        Set::new()
                    },
                    loans: Set::from([slot.index]),
                    storage: slot.ty.is_owner()
                        || slot.ty.is_buffer_view()
                        || slot
                            .ty
                            .reference_type()
                            .is_some_and(|r| r.target.is_place()),
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
        if let Op::Concurrent { branches, .. } = op {
            for dst in branches
                .iter()
                .filter_map(|b| b.result.as_ref())
                .filter(|s| s.ty.is_owner())
            {
                owner_roots.insert(plan.slot_count + n + dst.index);
            }
        }

        if matches!(
            op,
            Op::Text { .. }
                | Op::DomainOwner { decode: true, .. }
                | Op::Place(PlaceOp::Begin(_) | PlaceOp::Allocate(_) | PlaceOp::Read { .. })
        ) || matches!(
            op,
            Op::ArenaNew { .. } | Op::Place(PlaceOp::Copy { .. } | PlaceOp::MoveInit { .. })
        ) || matches!(op, Op::Buffer(super::BufferOp::Move { dst, .. }) if dst.ty.is_explicit_arena())
            || matches!(op, Op::Call { result: Some(dst), .. } | Op::Concurrent { result: Some(dst), .. } | Op::Buffer(super::BufferOp::Storage { dst: Some(dst), .. }) if dst.ty.is_owner())
            || matches!(op, Op::Buffer(super::BufferOp::Swap { .. }))
            || matches!(op, Op::Buffer(super::BufferOp::Detach { .. }))
        {
            owner_roots.insert(plan.slot_count + pc);
        }
    }
    if types.is_empty() {
        return Ok(0);
    }
    // Diverging bodies can still declare a result slot without defining it.
    for slot in plan.result.iter().flat_map(refs) {
        types.entry(slot.index).or_insert(slot.ty);
    }
    // Explicit caps make adversarial control-flow analyses fail closed.
    if types.len() > 65_536 || n > 100_000 {
        return Err(type_error(
            &plan.span,
            ErrorCode::ResourceLimit,
            "Reference analysis exceeds its slot or instruction limit; split the function into smaller functions",
        ));
    }
    let next = liveness::successors(plan);
    let live = liveness::solve(plan, &next, &types)?;
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
                    | Op::Buffer(super::BufferOp::Borrow {
                        reservation: true,
                        ..
                    })
                    | Op::Project {
                        reservation: true,
                        ..
                    }
            )
            .then_some(plan.slot_count + pc)
        })
        .collect();
    let facts = transfer::Facts {
        plan,
        functions,
        types: &types,
        local_roots: &local_roots,
        owner_roots: &owner_roots,
        local_places: &local_places,
        argument_roots: &argument_roots,
        arena_parameter_roots: &arena_parameter_roots,
        explicit_allocation_roots: &explicit_allocation_roots,
        reservations: &reservations,
        construction: &construction,
        escaped,
    };
    let lifetime = SlotLifetime::new(plan, &next);
    // A straight-line function never revisits an input snapshot. Move its
    // state along the edge instead of retaining a copy at every instruction.
    let straight = next
        .iter()
        .enumerate()
        .all(|(pc, successors)| successors.as_slice() == [pc + 1] || successors.is_empty());
    let mut states = vec![None; n + 1];
    states[0] = Some(initial);
    let mut queue = VecDeque::from([0]);
    let mut queued = vec![false; n + 1];
    queued[0] = true;
    let flow_limit = 2_000_000usize + 32 * n;
    let mut budget = flow_limit;
    while let Some(pc) = queue.pop_front() {
        queued[pc] = false;
        let mut state = if straight {
            states[pc].take()
        } else {
            states[pc].clone()
        }
        .expect("queued reference state");
        let input = &state;
        let cost = 1
            + live[pc].len()
            + input
                .slots
                .values()
                .chain(input.contents.values())
                .chain(input.retained.values().map(|(r, _)| r))
                .chain(
                    input
                        .regions
                        .values()
                        .flat_map(|r| r.values().map(|(r, _)| r)),
                )
                .chain(
                    input
                        .arena_retained
                        .values()
                        .flat_map(|r| r.values().map(|(r, _)| r)),
                )
                .map(|r| {
                    1 + r
                        .origins
                        .iter()
                        .map(|origin| 1 + origin.path.len())
                        .sum::<usize>()
                        + r.loans.len()
                        + r.ancestors.len()
                        + r.arenas.len()
                })
                .sum::<usize>();
        budget = budget.checked_sub(cost).ok_or_else(|| {
            type_error(
                plan.ops.spans().get(pc).unwrap_or(&plan.span),
                ErrorCode::ResourceLimit,
                "Reference flow analysis exceeds its compilation budget; split the function into smaller functions",
            )
        })?;
        let instruction = transfer::Instruction {
            facts: &facts,
            pc,
            live: &live[pc],
        };
        if pc == n {
            instruction.check_result(&state)?;
            continue;
        }
        let alive: Set = next[pc]
            .iter()
            .flat_map(|&next| live[next].iter().copied())
            .collect();
        instruction.apply(&mut state, &alive)?;
        let mut outgoing = Some(state);
        for (i, &target) in next[pc].iter().enumerate() {
            let mut state = if i + 1 == next[pc].len() {
                outgoing.take().unwrap()
            } else {
                outgoing.as_ref().unwrap().clone()
            };
            lifetime.prune(&mut state, target, &live[target]);
            let changed = if let Some(existing) = &mut states[target] {
                join(existing, &state)
            } else {
                states[target] = Some(state);
                true
            };
            if changed && !queued[target] {
                queue.push_back(target);
                queued[target] = true;
            }
        }
    }
    Ok(flow_limit - budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};

    #[test]
    fn short_borrow_flow_work_scales_with_statement_count() {
        let work = |length| {
            let calls = "t = t + get(&p)\n".repeat(length);
            let source = format!(
                "fn get(p:&Int32) @noescape -> Int32 {{ *p }}
                fn f(p:Int32)->Int32 {{ var t=0\n{calls} t }}"
            );
            let ast = crate::parser::parse(&source).unwrap();
            let registry = TypeRegistry::new().with_prelude(false);
            let module = PreparedModule::new(&ast, &registry, &BindingRegistry::default())
                .unwrap()
                .plan(&HashMap::new())
                .unwrap();
            let plan = &module.bodies["f"];
            analyze(
                plan,
                &module.functions["f"],
                &module.functions,
                &plan.type_registry,
                None,
            )
            .unwrap()
        };
        let small = work(100);
        let large = work(200);
        assert!(
            large > small && large <= 3 * small,
            "100 statements cost {small}, 200 cost {large}"
        );
    }
}
