//! Private shared views and exclusive resource loans for structured tasks. Ordinary
//! packets and public TaskValue inputs never accept references.
use super::{
    packet::{invalid, Packet, Shape as OwnedShape},
    pool, Lease, Storage,
};
use crate::{
    runtime::{arena::Arena, buffer::Buffer, cleanup::Cleanups},
    Error, Type,
};
use std::{alloc::Layout, ptr::NonNull, rc::Rc, sync::Arc};
mod graph;

#[derive(Clone, Debug)]
pub(crate) struct Shape {
    element: Layout,
    plain: bool,
    shared_vm: bool,
    exclusive_queue: bool,
    graph: Option<Arc<graph::Element>>,
}
impl Shape {
    pub(crate) fn of(ty: &Type) -> Option<Self> {
        if ty.contains_compact_enum() {
            return None;
        }
        let reference = ty.reference_type()?;
        if reference.mutable {
            // First exclusive protocol: one Mailbox owner, no aggregate state,
            // arbitrary wrappers, shared queue views or general mutable data.
            let target = match &reference.target {
                Type::Record(r) if r.name == "joky/outbox.Mailbox" && r.fields.len() == 1 => {
                    &r.fields[0].ty
                }
                target => target,
            };
            return matches!(target, Type::Resource(name) if name.as_ref() == "outbox.QueueOwner")
                .then(Self::queue);
        }
        let mut target = &reference.target;
        if target.is_owner() && !target.is_inline_owner() {
            while let Type::Record(r) = target {
                target = &r.fields.first()?.ty;
            }
        }
        let element = match target {
            Type::String => &Type::UInt8,
            Type::Buffer(element) | Type::BufferSlice(element) => element,
            _ => target,
        };
        use crate::core::types::walk::{TypeEdges, TypeWalk};
        // Include phantom arguments and zero-sized fields. References, handles,
        // closures and capabilities remain local. List/Map/Set use the same
        // finite field graph; query protocols require separate call-graph evidence.
        if !TypeWalk::new(&reference.target, TypeEdges::Fields).all(|ty| match ty {
            Type::Record(r) => !r.indirect && r.closure.is_none(),
            Type::Enum(e) => !e.indirect && !e.variants.is_empty(),
            Type::Tuple(_)
            | Type::FixedArray(_)
            | Type::Unit
            | Type::Bool
            | Type::Rune
            | Type::String
            | Type::Buffer(_) => true,
            Type::BufferSlice(_) => std::ptr::eq(ty, &reference.target),
            Type::Resource(name) => name.as_ref() == "vm.Owner",
            _ => ty.integer().is_some() || ty.is_float(),
        }) {
            return None;
        }
        let layout = element.storage_layout().ok()?;
        let mut shape = Self::checked(layout.size, layout.align).ok()?;
        if matches!(target, Type::Resource(name) if name.as_ref() == "vm.Owner") {
            return Some(Self::vm());
        }
        shape.plain = !matches!(
            target,
            Type::String | Type::Buffer(_) | Type::BufferSlice(_)
        );
        if TypeWalk::new(element, TypeEdges::Fields)
            .any(|ty| ty.is_owner_lane() || matches!(ty, Type::Enum(_)))
        {
            let graph = graph::Element::of(element)?;
            // An owning phantom argument does not add a physical owner lane.
            if graph.structured() {
                shape.graph = Some(Arc::new(graph));
            }
        }
        Some(shape)
    }
    fn checked(size: usize, align: usize) -> Result<Self, Error> {
        if size > 64 * 1024 * 1024
            || !align.is_power_of_two()
            || align > std::mem::align_of::<u64>()
            || !size.is_multiple_of(align)
        {
            return Err(invalid());
        }
        Ok(Self {
            element: Layout::from_size_align(size, align).map_err(|_| invalid())?,
            plain: false,
            shared_vm: false,
            exclusive_queue: false,
            graph: None,
        })
    }
    fn vm() -> Self {
        Self {
            element: Layout::new::<u64>(),
            plain: false,
            shared_vm: true,
            exclusive_queue: false,
            graph: None,
        }
    }
    fn queue() -> Self {
        Self {
            exclusive_queue: true,
            shared_vm: false,
            ..Self::vm()
        }
    }
    pub(crate) fn encode(&self, words: &mut Vec<u64>) {
        if self.exclusive_queue {
            words.push(104);
        } else if self.shared_vm {
            words.push(103);
        } else if let Some(graph) = &self.graph {
            words.push(if self.plain { 102 } else { 101 });
            graph.encode(words);
        } else {
            words.extend([
                if self.plain { 100 } else { 99 },
                self.element.size() as u64,
                self.element.align() as u64,
            ]);
        }
    }
    pub(crate) fn decode(words: &[u64]) -> Result<Self, Error> {
        if words == [104] {
            return Ok(Self::queue());
        }
        if words == [103] {
            return Ok(Self::vm());
        }
        if let [kind @ (101 | 102), rest @ ..] = words {
            let mut rest = rest;
            let graph = graph::Element::decode(&mut rest, 0, &mut 0)?;
            if !rest.is_empty() || !graph.structured() {
                return Err(invalid());
            }
            return Ok(Self {
                element: graph.layout,
                plain: *kind == 102,
                shared_vm: false,
                exclusive_queue: false,
                graph: Some(Arc::new(graph)),
            });
        }
        let [kind @ (99 | 100), size, align] = *words else {
            return Err(invalid());
        };
        let mut shape = Self::checked(
            usize::try_from(size).map_err(|_| invalid())?,
            usize::try_from(align).map_err(|_| invalid())?,
        )?;
        shape.plain = kind == 100;
        Ok(shape)
    }
    pub(crate) fn layout(&self) -> Layout {
        self.element
    }
    /// Compiler layout proves a live scoped reference. String, Slice and the
    /// single-field Array wrapper all name a Buffer header directly.
    pub(crate) unsafe fn buffer(&self, word: u64) -> *const Buffer {
        word as *const Buffer
    }
    /// # Safety
    /// `word` is a live initialized scoped reference of this compiler-certified
    /// shape on the current worker. Its source remains frozen through capture.
    pub(crate) unsafe fn eligible(&self, word: u64) -> bool {
        self.source(word).is_some_and(|source| {
            self.graph
                .as_ref()
                .is_none_or(|graph| graph.eligible(source.data, source.len, &mut 0))
        })
    }
    unsafe fn source(&self, word: u64) -> Option<Source> {
        if self.shared_vm || self.exclusive_queue {
            if self.exclusive_queue {
                if !super::control::ExclusiveQueue::eligible(word) {
                    return None;
                }
            } else {
                super::control::SharedVm::capture(word).ok()?;
            }
            // The transport holds the proxy identity, never a foreign header.
            Some(Source {
                data: NonNull::<u64>::dangling().cast(),
                len: 0,
                capacity: 0,
                origin: None,
            })
        } else if self.plain {
            let data = NonNull::new(word as *mut u8)?;
            if !(word as usize).is_multiple_of(self.element.align()) {
                return None;
            }
            if let Some(origin) = ACTIVE_LOANS.with(|active| {
                // The poll guard holds the immutable loan slice alive. No foreign
                // header, Rc or frame is accessed; each loan belongs to this task.
                (&*active.get()).iter().find_map(|loan| {
                    let bytes = loan.range.len.checked_mul(loan.range.element.size())?;
                    let offset = (word as usize).checked_sub(loan.range.data.as_ptr() as usize)?;
                    (pool::identity() == Some(loan.task)
                        && offset
                            .checked_add(self.element.size())
                            .is_some_and(|end| end <= bytes))
                    .then(|| loan.range.origin.clone())
                })
            }) {
                return Some(Source {
                    data,
                    len: 1,
                    capacity: 1,
                    origin: Some(origin),
                });
            }
            super::current()?
                .owns_range(data, self.element.size(), self.element.align())
                .then_some(Source {
                    data,
                    len: 1,
                    capacity: 1,
                    origin: None,
                })
        } else {
            let buffer = self.buffer(word).as_ref()?;
            let source = if let Some(graph) = &self.graph {
                buffer.scoped_elements(self.layout(), graph.owners())?
            } else {
                buffer.scoped_range(self.layout())?
            };
            Some(source)
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum InputShape {
    Owned(OwnedShape),
    Borrowed(Shape),
}
impl InputShape {
    pub(crate) fn of(ty: &Type) -> Result<Self, Error> {
        if let Some(shape) = Shape::of(ty) {
            Ok(Self::Borrowed(shape))
        } else {
            OwnedShape::of(ty).map(Self::Owned)
        }
    }
    pub(crate) fn inputs(&self) -> usize {
        match self {
            Self::Owned(s) => s.inputs(),
            Self::Borrowed(_) => 1,
        }
    }
}

pub(crate) struct Source {
    pub(crate) data: NonNull<u8>,
    pub(crate) len: usize,
    pub(crate) capacity: usize,
    pub(crate) origin: Option<Arc<Scope>>,
}

pub(crate) struct Scope {
    parent: u64,
    pub(crate) identity: u64,
    _root: Storage,
    _code: Arc<dyn std::any::Any + Send + Sync>,
    _charge: Option<Lease>,
}
impl Scope {
    pub(crate) fn new(identity: u64) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            parent: pool::identity().ok_or_else(invalid)?,
            identity,
            _root: super::current().ok_or_else(invalid)?,
            _code: pool::code_owner().ok_or_else(invalid)?,
            _charge: super::reserve_current(
                std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>(),
            )?,
        }))
    }
}
struct Range {
    data: NonNull<u8>,
    len: usize,
    capacity: usize,
    element: Layout,
    scope: Arc<Scope>,
    origin: Arc<Scope>,
    _charge: Option<Lease>,
    plain: bool,
    snapshot: Option<graph::Snapshot>,
    shared_vm: Option<super::control::SharedVm>,
    exclusive_queue: Option<super::control::ExclusiveQueue>,
}
// SAFETY: capture requires compiler-certified storage owned by this parent task
// or received under a bounded loan. Owning lanes are traversed only on the sender,
// replaced by zero in snapshots, and reconstructed as receiver-local views.
// Pointer-free leaves alone retain shared data addresses. Each hop retains the original
// lease directly, without foreign headers or a recursive lease chain. Local
// cancellation drains this subtree before dropping its frames.
// The static group and worker-root failure barrier retain the source owner until
// every reader is cleaned. Exclusive Mailbox inputs carry a control proxy and
// a unique return guard instead of a source address. Only payload bytes or
// checked proxy identities cross; Buffer/Rc/frame metadata do
// not. Each installed node has separate worker-local provenance held by the Job.
unsafe impl Send for Range {}

/// Stable, worker-local provenance for installed headers and their slices.
/// The job retains this allocation through all descendant and local view cleanup.
pub(crate) struct Loan {
    task: u64,
    range: Range,
    resource: Option<crate::runtime::resource::OwnedResource>,
}
thread_local! {
    static ACTIVE_LOANS: std::cell::Cell<*const [Rc<Loan>]> = const {
        std::cell::Cell::new(std::ptr::slice_from_raw_parts(NonNull::<Rc<Loan>>::dangling().as_ptr(), 0))
    };
}
/// Ordinary references have no Buffer header. Poll-local lookup supplies their
/// received provenance without changing the language reference representation.
pub(crate) struct LoanGuard<'a> {
    previous: *const [Rc<Loan>],
    _loans: std::marker::PhantomData<&'a [Rc<Loan>]>,
}
impl<'a> LoanGuard<'a> {
    pub(crate) fn enter(loans: &'a [Rc<Loan>]) -> Self {
        Self {
            previous: ACTIVE_LOANS.with(|active| active.replace(loans)),
            _loans: Default::default(),
        }
    }
}
impl Drop for LoanGuard<'_> {
    fn drop(&mut self) {
        ACTIVE_LOANS.with(|active| active.set(self.previous));
    }
}
impl Loan {
    pub(crate) fn resource(&self) -> Option<&crate::runtime::resource::OwnedResource> {
        (pool::identity() == Some(self.task))
            .then_some(self.resource.as_ref())
            .flatten()
    }
    pub(crate) fn source(
        &self,
        data: NonNull<u8>,
        len: usize,
        capacity: usize,
        element: Layout,
    ) -> Option<Source> {
        if pool::identity() != Some(self.task) || element != self.range.element {
            return None;
        }
        if capacity < len
            || capacity > self.range.capacity
            || !contains_range(self.range.data, self.range.len, data, len, element)
        {
            return None;
        }
        Some(Source {
            data,
            len,
            capacity,
            origin: Some(self.range.origin.clone()),
        })
    }
}

/// Zero-sized elements share a sentinel; their logical length still cannot grow.
/// Nonzero elements must remain aligned to, and inside, the received element range.
pub(crate) fn contains_range(
    source: NonNull<u8>,
    source_len: usize,
    data: NonNull<u8>,
    len: usize,
    element: Layout,
) -> bool {
    if element.size() == 0 {
        return data == source && len <= source_len;
    }
    let Some(offset) = (data.as_ptr() as usize).checked_sub(source.as_ptr() as usize) else {
        return false;
    };
    let Some(bytes) = len.checked_mul(element.size()) else {
        return false;
    };
    let Some(end) = offset.checked_add(bytes) else {
        return false;
    };
    offset % element.size() == 0
        && source_len
            .checked_mul(element.size())
            .is_some_and(|limit| end <= limit)
}

pub(crate) struct Borrowed {
    range: Range,
}
pub(crate) enum Input {
    Owned(Box<Packet>),
    Borrowed(Borrowed),
}
impl Input {
    pub(crate) fn owned(packet: Packet) -> Self {
        // Packet already charges its storage, including this boxed body.
        Self::Owned(Box::new(packet))
    }
    /// # Safety
    /// The checked reference has this shape and stays live until `scope` drains.
    /// Shared data is frozen; an exclusive source is inaccessible during its loan. Capture executes on the source task's worker.
    pub(crate) unsafe fn borrow(shape: Shape, word: u64, scope: Arc<Scope>) -> Result<Self, Error> {
        if pool::identity() != Some(scope.parent) {
            return Err(invalid());
        }
        let _metadata =
            super::reserve_current(shape.graph.as_ref().map_or(0, |graph| graph.bytes()))?;
        if !shape.eligible(word) {
            return Err(invalid());
        }
        let range = Range::capture(&shape, word, scope)?;
        #[cfg(test)]
        observe(&range, word as usize, true);
        Ok(Self::Borrowed(Borrowed { range }))
    }
    pub(crate) fn inputs(&self) -> usize {
        match self {
            Self::Owned(p) => p.shape.inputs(),
            Self::Borrowed(_) => 1,
        }
    }
    pub(crate) fn install(
        self,
        arena: &mut Arena,
        cleanups: &mut Cleanups,
        base: usize,
        loans: &mut Vec<Rc<Loan>>,
    ) -> Result<Vec<u64>, Error> {
        match self {
            Self::Owned(packet) => packet.install(arena, cleanups, base, None),
            Self::Borrowed(borrowed) => Ok(vec![borrowed.range.install(arena, loans, true)?]),
        }
    }
}

impl Range {
    unsafe fn capture(shape: &Shape, word: u64, scope: Arc<Scope>) -> Result<Self, Error> {
        let Source {
            data,
            len,
            capacity,
            origin,
        } = shape.source(word).ok_or_else(invalid)?;
        // Prepay the Rc allocation and the receiving Job's loan-vector growth.
        // Four pointer slots per node cover both the initial capacity and growth.
        let charge = super::reserve_current(
            std::mem::size_of::<Loan>()
                + 2 * std::mem::size_of::<usize>()
                + 4 * std::mem::size_of::<Rc<Loan>>()
                + if shape.shared_vm || shape.exclusive_queue {
                    crate::runtime::resource::OwnedResource::event_bytes()
                        + std::mem::size_of::<usize>() // Box<Arc<Proxy>> token
                } else {
                    0
                },
        )?;
        let snapshot = shape
            .graph
            .as_ref()
            .filter(|g| g.structured())
            .map(|g| g.capture(data, len, &scope))
            .transpose()?;
        Ok(Self {
            data,
            len,
            capacity,
            element: shape.layout(),
            origin: origin.unwrap_or_else(|| scope.clone()),
            plain: shape.plain,
            scope,
            _charge: charge,
            snapshot,
            exclusive_queue: if shape.exclusive_queue {
                Some(super::control::ExclusiveQueue::capture(word)?)
            } else {
                None
            },
            shared_vm: if shape.shared_vm {
                Some(super::control::SharedVm::capture(word)?)
            } else {
                None
            },
        })
    }
    fn install(
        mut self,
        arena: &mut Arena,
        loans: &mut Vec<Rc<Loan>>,
        observed: bool,
    ) -> Result<u64, Error> {
        if pool::parent_identity() != Some(self.scope.parent) || self.scope.identity == 0 {
            return Err(invalid());
        }
        let copied = self.snapshot.is_some();
        let data = match self.snapshot.take() {
            Some(snapshot) => snapshot.install(arena, self.element, self.len, loans)?,
            None => self.data,
        };
        // Each node's loan is stable and local; arena headers contain no Arc/Rc.
        // Parent scope fences also cover copied intermediate pointer tables.
        let original = self.data;
        self.data = data;
        let resource = if let Some(queue) = self.exclusive_queue.take() {
            Some(queue.install()?)
        } else {
            self.shared_vm.take().map(|vm| vm.install()).transpose()?
        };
        loans.push(Rc::new(Loan {
            task: pool::identity().ok_or_else(invalid)?,
            range: self,
            resource,
        }));
        let loan = &**loans.last().unwrap();
        let range = &loan.range;
        let word = if range.plain {
            data.as_ptr() as u64
        } else {
            unsafe {
                Buffer::scoped_view(arena, data, range.len, range.capacity, range.element, loan)?
            }
            .as_ptr() as u64
        };
        #[cfg(test)]
        if observed {
            observe_installed(range, original, word as usize, copied);
        }
        #[cfg(not(test))]
        let _ = (observed, original, copied);
        Ok(word)
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Observation {
    pub(crate) plain: bool,
    pub(crate) copied: bool,
    pub(crate) parent: u64,
    pub(crate) task: u64,
    pub(crate) scope: u64,
    pub(crate) data: usize,
    pub(crate) len: usize,
    pub(crate) header: usize,
    pub(crate) origin_parent: u64,
    pub(crate) origin_scope: u64,
    pub(crate) thread: std::thread::ThreadId,
    pub(crate) source: bool,
}
#[cfg(test)]
static OBSERVATIONS: std::sync::Mutex<Vec<Observation>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
fn observe(range: &Range, header: usize, source: bool) {
    OBSERVATIONS.lock().unwrap().push(Observation {
        plain: range.plain,
        copied: range.snapshot.is_some(),
        parent: range.scope.parent,
        task: pool::identity().unwrap(),
        scope: range.scope.identity,
        data: range.data.as_ptr() as usize,
        len: range.len,
        header,
        origin_parent: range.origin.parent,
        origin_scope: range.origin.identity,
        source,
        thread: std::thread::current().id(),
    });
}
#[cfg(test)]
fn observe_installed(range: &Range, original: NonNull<u8>, header: usize, copied: bool) {
    observe(range, header, false);
    let mut observations = OBSERVATIONS.lock().unwrap();
    let observation = observations
        .iter_mut()
        .rev()
        .find(|o| !o.source && o.task == pool::identity().unwrap() && o.header == header)
        .unwrap();
    observation.data = original.as_ptr() as usize;
    observation.copied = copied;
}
#[cfg(test)]
pub(crate) fn observations(parent: u64) -> Vec<Observation> {
    OBSERVATIONS
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.parent == parent)
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) fn tree_observations(parent: u64) -> Vec<Observation> {
    let mut pending = vec![parent];
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    while let Some(task) = pending.pop() {
        if !seen.insert(task) {
            continue;
        }
        let observations = observations(task);
        pending.extend(observations.iter().filter(|o| !o.source).map(|o| o.task));
        result.extend(observations);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_borrow_graph_descriptors_reject_corrupt_fields_and_depth() {
        let vm = Shape::of(&Type::reference(
            Type::tuple(vec![Type::Resource("vm.Owner".into()), Type::Int32]),
            false,
        ))
        .unwrap();
        let mut encoded = Vec::new();
        vm.encode(&mut encoded);
        assert!(Shape::decode(&encoded).is_ok());
        // A VM field occupies an aligned owner lane, never scalar payload bytes.
        for bad in [vec![102, 4, 4, 1, 0, 11], vec![102, 16, 8, 1, 4, 11]] {
            assert!(Shape::decode(&bad).is_err());
        }
        let shape = Shape::of(&Type::reference(
            Type::BufferSlice(Arc::new(Type::String)),
            false,
        ))
        .unwrap();
        let mut valid = Vec::new();
        shape.encode(&mut valid);
        assert!(Shape::decode(&valid).is_ok());
        for length in 0..valid.len() {
            assert!(Shape::decode(&valid[..length]).is_err());
        }
        // Root size/alignment, field count/offset/kind, and child metadata.
        for (index, value) in [
            (1, 0),
            (2, 16),
            (3, u64::MAX),
            (4, 1),
            (5, 10),
            (7, 0),
            (8, u64::MAX),
            (9, 1),
        ] {
            let mut bad = valid.clone();
            bad[index] = value;
            assert!(Shape::decode(&bad).is_err(), "{index}={value}");
        }
        let mut deep = vec![101];
        for _ in 0..34 {
            deep.extend([8, 8, 1, 0, 9]);
        }
        deep.extend([1, 1, 1, 0, 1]);
        assert!(Shape::decode(&deep).is_err());
    }
    #[test]
    fn scoped_borrow_aggregate_shapes_check_fields_and_phantom_arguments() {
        let registry = crate::stdlib::module_registry();
        let types = registry.types();
        let source = r#"
            import joky/array.{type Array}
            import joky/fixed_array.{type FixedArray}
            import joky/map.{type Map}
            import joky/list.{type List}
            import joky/set.{type Set}
            struct Tracked { text: String }

            struct Plain { flag: Bool, payload: (UInt8, Float64), row: FixedArray<Int32, 2> }
            struct Empty { value: Unit }
            struct Pointer { value: Ptr }
            struct Reference { value: &Int32 }
            struct Callback { value: fn(Int32) -> Int32 }
            struct Owner { value: String }
            struct Phantom<T> { value: Int32 }
            enum Choice { One(Int32), Empty, Hidden(Ptr) }
            enum OwnedChoice { Text(String), Empty }
            enum HiddenDrop { Empty, Value(Tracked) }
            enum PhantomChoice<T> { Empty }
            fn accepted(a: &Array<Plain>, b: &Array<Empty>, c: &Array<FixedArray<Int32, 0>>, d: &Plain, e: &Empty, f: &FixedArray<Int32, 3>, g: &Int32, h: &Array<Owner>, i: &Owner, j: &Array<Array<Owner>>, k: &Phantom<String>, l: &OwnedChoice, m: &Array<OwnedChoice>, n: &Map<Int32, String>, o: &List<String>, p: &Set<Int32>, n1: &Tracked, o1: &Array<Tracked>, p1: &Map<Int32, Tracked>, s1: &HiddenDrop) {}
            fn rejected(a: &Array<Pointer>, b: &Array<Reference>, c: &Array<Callback>, e: &Array<Phantom<Ptr>>, f: &Array<Choice>, g: &Pointer, h: &Reference, i: &Callback, k: &Phantom<Ptr>, l: &Choice, m: &mut Plain, q: &List<Pointer>, r: &Set<Phantom<Ptr>>, t: &PhantomChoice<Ptr>, u: &map.Iter<Int32, String>, v: &map.Entry<Int32, String>) {}
        "#;
        let ast = types
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let prepared = crate::core::prepared::PreparedModule::new(&ast, types, registry.metadata())
            .unwrap_or_else(|e| panic!("{e}"));
        let imports = registry
            .import_layouts(prepared.ast(), &Default::default())
            .unwrap();
        let plan = prepared.plan(&imports).unwrap_or_else(|e| panic!("{e}"));
        for ty in &crate::core::ir::parallel::scoped::signature(&plan, "accepted").params {
            let shape = Shape::of(ty).unwrap_or_else(|| panic!("{ty}"));
            let mut encoded = Vec::new();
            shape.encode(&mut encoded);
            assert_eq!(Shape::decode(&encoded).unwrap().layout(), shape.layout());
        }
        for ty in &crate::core::ir::parallel::scoped::signature(&plan, "rejected").params {
            assert!(Shape::of(ty).is_none(), "{ty}");
        }
    }

    #[test]
    fn scoped_borrow_zero_sized_ranges_keep_logical_bounds() {
        let sentinel = NonNull::<u8>::dangling();
        let other = NonNull::<u64>::dangling().cast::<u8>();
        let unit = Layout::new::<()>();
        assert!(contains_range(sentinel, 3, sentinel, 3, unit));
        assert!(contains_range(sentinel, 3, sentinel, 0, unit));
        assert!(!contains_range(sentinel, 3, sentinel, 4, unit));
        assert!(!contains_range(sentinel, 3, other, 0, unit));
        assert!(!contains_range(sentinel, 0, sentinel, 1, unit));
    }

    #[test]
    fn scoped_borrow_shapes_keep_packets_mutation_and_nominal_capabilities_closed() {
        for target in [
            Type::Resource("vm.Owner".into()),
            Type::String,
            Type::BufferSlice(Arc::new(Type::Int32)),
            Type::BufferSlice(Arc::new(Type::Bool)),
            Type::BufferSlice(Arc::new(Type::String)),
            Type::BufferSlice(Arc::new(Type::Unit)),
            Type::BufferSlice(Arc::new(Type::tuple(vec![Type::UInt8, Type::Float64]))),
        ] {
            let ty = Type::reference(target.clone(), false);
            let shape = Shape::of(&ty).unwrap();
            let mut words = Vec::new();
            shape.encode(&mut words);
            assert_eq!(Shape::decode(&words).unwrap().layout(), shape.layout());
            assert!(OwnedShape::of(&ty).is_err());
            assert!(Shape::of(&Type::reference(target, true)).is_none());
        }
        for target in [
            Type::Resource("vm.MigrationOwner".into()),
            Type::Resource("tcp.Stream".into()),
            Type::Resource("outbox.QueueOwner".into()),
            Type::Resource("custom.Owner".into()),
            Type::BufferSlice(Arc::new(Type::Named("domain.Handle".into()))),
            Type::BufferSlice(Arc::new(Type::Named("admission.Token".into()))),
        ] {
            assert!(Shape::of(&Type::reference(target, false)).is_none());
        }
        let queue = Type::reference(Type::Resource("outbox.QueueOwner".into()), true);
        let shape = Shape::of(&queue).unwrap();
        let mut words = Vec::new();
        shape.encode(&mut words);
        assert_eq!(words, [104]);
        assert!(Shape::decode(&words).unwrap().exclusive_queue);
        assert!(OwnedShape::of(&queue).is_err());
        for words in [
            vec![],
            vec![103, 0],
            vec![104, 0],
            vec![99],
            vec![99, 0],
            vec![99, 6],
            vec![99, 13],
            vec![99, 1, 0],
            vec![99, 1, 2],
            vec![99, 16, 16],
            vec![99, u64::MAX, 1],
            vec![99, 4, 4, 0],
        ] {
            assert!(Shape::decode(&words).is_err());
        }
    }
}

#[cfg(test)]
mod plain_tests {
    use super::*;
    use crate::runtime::cps::CpsFrame;
    use crate::vm::{InstanceValue, TaskValue, VmEngine, VmMode};

    #[test]
    fn scoped_borrow_plain_provenance_rejects_foreign_wide_and_expired_sources() {
        unsafe extern "C" fn inspect(frame: *mut CpsFrame) -> i32 {
            let input = observations(pool::parent_identity().unwrap())
                .into_iter()
                .find(|o| !o.source && o.task == pool::identity().unwrap())
                .unwrap();
            let shape = Shape::of(&Type::reference(Type::Int32, false)).unwrap();
            let data = input.data as u64;
            let mut bits = u64::from(shape.eligible(data));
            bits |= u64::from(!shape.eligible(data + 1)) << 1;
            bits |= u64::from(!shape.eligible(data + 4)) << 2;
            let wider = Shape::of(&Type::reference(Type::Int64, false)).unwrap();
            bits |= u64::from(!wider.eligible(data)) << 3;
            bits |= u64::from(!shape.eligible(0)) << 4;
            let root = super::super::current().unwrap();
            bits |= u64::from(!root.owns_range(NonNull::new_unchecked(data as *mut u8), 4, 4)) << 5;
            let mut arena = Arena::default();
            let local = arena.allocate(4, 4).unwrap();
            local.cast::<i32>().as_ptr().write(42);
            bits |= u64::from(shape.eligible(local.as_ptr() as u64)) << 6;
            arena.release(local);
            bits |= u64::from(!shape.eligible(local.as_ptr() as u64)) << 7;
            let unowned = 42i32;
            bits |= u64::from(!shape.eligible((&unowned as *const i32) as u64)) << 8;
            (*frame).result = bits;
            0
        }
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_parallel_suspending_fn(
                    "inspect",
                    crate::FunctionSignature::new(vec![], Type::Int32),
                    inspect,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
            vm.load_source(
                r#"
                import joky/task
                extern fn inspect() @suspends -> Int32
                struct Pair { a: Int32, b: Int32 }
                fn reader(value: &Int32) @suspends -> Int32 { inspect() + *value }
                fn root() @suspends -> Int32 {
                    let pair = Pair { a: 7, b: 9 }
                    let (a, b) = join { reader(&pair.a), task.yield_now() }
                    a
                }
            "#,
            )
            .unwrap();
            assert!(vm.parallel_signature("reader").is_err());
            let budget = crate::runtime::parallel::TaskBudget::new(4 << 20);
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(3, 16).unwrap();
            let task = vm
                .prepare_parallel_task("root", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            task.wait().unwrap();
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(518)),
                "{mode:?}"
            );
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
        }
    }
}
