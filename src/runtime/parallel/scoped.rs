//! Private, nonescaping inputs for compiler-certified structured tasks. Ordinary
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

#[derive(Clone, Debug)]
pub(crate) struct Shape {
    element: Layout,
    plain: bool,
}
impl Shape {
    pub(crate) fn of(ty: &Type) -> Option<Self> {
        let reference = ty.reference_type()?;
        if reference.mutable {
            return None;
        }
        let element = match &reference.target {
            Type::String => Type::UInt8,
            Type::BufferSlice(element) => (**element).clone(),
            Type::Record(record)
                if record.array && !record.indirect && record.fields.len() == 1 =>
            {
                reference.target.array_element_type()?.clone()
            }
            _ => reference.target.clone(),
        };
        use crate::core::types::walk::{TypeEdges, TypeWalk};
        // Copy alone is insufficient: references, callable values and phantom
        // capability arguments must not cross in a shared element. Drop requires
        // an owning record, so the owner-free restriction also excludes hooks.
        if !TypeWalk::new(&element, TypeEdges::Fields).all(|ty| match ty {
            Type::Record(r) => !r.indirect && r.owners.is_empty() && r.closure.is_none(),
            Type::Tuple(_) | Type::FixedArray(_) | Type::Unit | Type::Bool | Type::Rune => true,
            _ => ty.integer().is_some() || ty.is_float(),
        }) {
            return None;
        }
        let layout = element.storage_layout().ok()?;
        let mut shape = Self::checked(layout.size, layout.align).ok()?;
        shape.plain = !matches!(&reference.target, Type::String | Type::BufferSlice(_))
            && !matches!(&reference.target, Type::Record(r) if r.array);
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
        })
    }
    pub(crate) fn encode(&self, words: &mut Vec<u64>) {
        // Physical evidence from the checked compiler, not a user-supplied type
        // certificate. Admission above proves the complete pointer-free type tree.
        words.extend([
            if self.plain { 100 } else { 99 },
            self.element.size() as u64,
            self.element.align() as u64,
        ]);
    }
    pub(crate) fn decode(words: &[u64]) -> Result<Self, Error> {
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
    /// Compiler layout proves a live shared reference. String, Slice and the
    /// single-field Array wrapper all name a Buffer header directly.
    pub(crate) unsafe fn buffer(&self, word: u64) -> *const Buffer {
        word as *const Buffer
    }
    pub(crate) unsafe fn eligible(&self, word: u64) -> bool {
        self.source(word).is_some()
    }
    unsafe fn source(&self, word: u64) -> Option<(NonNull<u8>, usize, Option<Arc<Scope>>)> {
        if self.plain {
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
                return Some((data, 1, Some(origin)));
            }
            super::current()?
                .owns_range(data, self.element.size(), self.element.align())
                .then_some((data, 1, None))
        } else {
            let source = self.buffer(word).as_ref()?.scoped_range(self.layout())?;
            Some((source.data, source.len, source.origin.cloned()))
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

pub(crate) struct Source<'a> {
    pub(crate) data: NonNull<u8>,
    pub(crate) len: usize,
    pub(crate) origin: Option<&'a Arc<Scope>>,
}

pub(crate) struct Scope {
    parent: u64,
    pub(crate) identity: u64,
    _root: Storage,
    _code: Arc<crate::vm::parallel::Code>,
    _charge: Option<Lease>,
}
impl Scope {
    pub(crate) fn new(identity: u64) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            parent: pool::identity().ok_or_else(invalid)?,
            identity,
            _root: super::current().ok_or_else(invalid)?,
            _code: pool::code().ok_or_else(invalid)?,
            _charge: super::reserve_current(
                std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>(),
            )?,
        }))
    }
}
struct Range {
    data: NonNull<u8>,
    len: usize,
    element: Layout,
    scope: Arc<Scope>,
    origin: Arc<Scope>,
    _charge: Option<Lease>,
    plain: bool,
}
// SAFETY: capture requires compiler-certified shared pointer-free storage owned by
// this parent task or received under a bounded loan. Each hop retains the original
// lease directly, without foreign headers or a recursive lease chain. Local
// cancellation drains this subtree before dropping its frames.
// The static group and worker-root failure barrier retain the source owner until
// every reader is cleaned. Only payload bytes cross; Buffer/Rc/frame metadata do
// not. Buffer inputs install a worker-local header; plain inputs keep the
// certified payload address, with provenance retained separately by the Job.
unsafe impl Send for Range {}

/// Stable, worker-local provenance for installed headers and their slices.
/// The job retains this allocation through all descendant and local view cleanup.
pub(crate) struct Loan {
    task: u64,
    range: Range,
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
    pub(crate) fn source(
        &self,
        data: NonNull<u8>,
        len: usize,
        element: Layout,
    ) -> Option<Source<'_>> {
        if pool::identity() != Some(self.task) || element != self.range.element {
            return None;
        }
        if !contains_range(self.range.data, self.range.len, data, len, element) {
            return None;
        }
        Some(Source {
            data,
            len,
            origin: Some(&self.range.origin),
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
    pub(crate) unsafe fn borrow(shape: Shape, word: u64, scope: Arc<Scope>) -> Result<Self, Error> {
        if pool::identity() != Some(scope.parent) {
            return Err(invalid());
        }
        let (data, len, origin) = shape.source(word).ok_or_else(invalid)?;
        let range = Range {
            data,
            len,
            element: shape.layout(),
            origin: origin.unwrap_or_else(|| scope.clone()),
            plain: shape.plain,
            scope,
            _charge: super::reserve_current(
                std::mem::size_of::<Loan>() + 2 * std::mem::size_of::<usize>(),
            )?,
        };
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
            Self::Borrowed(borrowed) => {
                let range = borrowed.range;
                if pool::parent_identity() != Some(range.scope.parent) || range.scope.identity == 0
                {
                    return Err(invalid());
                }
                // Keep provenance through execution AND all local view cleanup.
                // Arena headers contain only this worker-local pointer; storing an
                // owning Arc in them would cycle through Scope's shared Storage.
                loans.push(Rc::new(Loan {
                    task: pool::identity().ok_or_else(invalid)?,
                    range,
                }));
                let loan = &**loans.last().unwrap();
                let range = &loan.range;
                let word = if range.plain {
                    range.data.as_ptr() as u64
                } else {
                    unsafe {
                        Buffer::scoped_view(arena, range.data, range.len, range.element, loan)?
                    }
                    .as_ptr() as u64
                };
                #[cfg(test)]
                observe(range, word as usize, false);
                Ok(vec![word])
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct Observation {
    pub(crate) plain: bool,
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
    fn scoped_borrow_aggregate_shapes_check_fields_and_phantom_arguments() {
        let registry = crate::stdlib::module_registry();
        let types = registry.types();
        let source = r#"
            import joky/array.{type Array}
            import joky/fixed_array.{type FixedArray}
            struct Plain { flag: Bool, payload: (UInt8, Float64), row: FixedArray<Int32, 2> }
            struct Empty { value: Unit }
            struct Pointer { value: Ptr }
            struct Reference { value: &Int32 }
            struct Callback { value: fn(Int32) -> Int32 }
            struct Owner { value: String }
            struct Phantom<T> { value: Int32 }
            enum Choice { One(Int32), Empty }
            fn accepted(a: &Array<Plain>, b: &Array<Empty>, c: &Array<FixedArray<Int32, 0>>, d: &Plain, e: &Empty, f: &FixedArray<Int32, 3>, g: &Int32) {}
            fn rejected(a: &Array<Pointer>, b: &Array<Reference>, c: &Array<Callback>, d: &Array<Owner>, e: &Array<Phantom<Ptr>>, f: &Array<Choice>, g: &Pointer, h: &Reference, i: &Callback, j: &Owner, k: &Phantom<Ptr>, l: &Choice, m: &mut Plain) {}
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
            Type::String,
            Type::BufferSlice(Arc::new(Type::Int32)),
            Type::BufferSlice(Arc::new(Type::Bool)),
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
            Type::BufferSlice(Arc::new(Type::String)),
            Type::BufferSlice(Arc::new(Type::Named("domain.Handle".into()))),
            Type::BufferSlice(Arc::new(Type::Named("admission.Token".into()))),
        ] {
            assert!(Shape::of(&Type::reference(target, false)).is_none());
        }
        for words in [
            vec![],
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
                    let (a, b) = join { | reader(&pair.a) | task.yield_now() }
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
