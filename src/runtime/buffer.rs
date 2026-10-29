//! Private typed storage with task-registered, independently reclaimable owners.
use super::arena::Arena;
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::{alloc::Layout, cell::Cell, ptr::NonNull};

/// Stable control block; no element pointers or layout inputs are source-visible.
/// Replaced payloads and explicitly released owners refund their arena charge.
pub(crate) enum DynDispatch {
    Native {
        table: usize,
        indirect: bool,
    },
    Interpreted {
        table: std::sync::Arc<crate::backends::interp::DynMethodTable>,
        indirect: bool,
    },
}

pub(crate) struct Buffer {
    // Local provenance only. Remote headers point at a loan on their own worker.
    origin: *const Buffer,
    loan: *const super::parallel::scoped::Loan,
    owner_task: u64,
    pub(crate) dynamic: Option<DynDispatch>,
    resource: Option<super::resource::OwnedResource>,
    data: NonNull<u8>,
    len: usize,
    capacity: usize,
    element: Layout,
    cleanup: Option<ElementCleanup>,
    // Only terminal cleanup follows a failed replacement. Null owner lanes
    // alone cannot distinguish it from a live enum with a nullary variant.
    failed_store: Option<usize>,
    metadata_budget: *const super::arena::Budget,
    metadata_account: *const super::arena::Budget,
    metadata_charge: usize,
    metadata_parallel: Option<super::parallel::Lease>,
    failed_descriptor: bool,
    // Intrusive release continuation. These bytes are charged with the header;
    // terminal traversal needs no allocation, even after budget exhaustion.
    release_parent: *mut Self,
    release_owner: usize,
    release_hook: usize,
    release_active: bool,
}

/// Nonowning cursor, retained by the task arena. Checked source loans keep the
/// payload stable. Shared copies refer to the same position; advancing changes
/// only this header, never the elements or previously returned addresses.
pub(crate) struct BufferIterator {
    data: NonNull<u8>,
    len: usize,
    stride: usize,
    position: Cell<usize>,
}
impl BufferIterator {
    pub(crate) fn advance(&self) -> bool {
        let position = self.position.get();
        if position < self.len {
            self.position.set(position + 1);
            true
        } else {
            // Buffer lengths are bounded by isize::MAX, including ZSTs.
            self.position.set(self.len + 1);
            false
        }
    }
    pub(crate) fn current(&self) -> Result<*mut u8, Error> {
        let position = self.position.get();
        if position == 0 || position > self.len {
            return Err(bounds_error(
                "Iterator current requires a successful advance",
            ));
        }
        // Construction inherits the buffer's checked initialized layout.
        Ok(unsafe { self.data.as_ptr().add((position - 1) * self.stride) })
    }
}

/// Two columns advance together; neither cursor nor entry owns either payload.
pub(crate) struct BufferPairIterator {
    keys: BufferIterator,
    values: BufferIterator,
}
pub(crate) struct BufferEntry {
    pub(crate) key: *mut u8,
    pub(crate) value: *mut u8,
}
impl BufferPairIterator {
    pub(crate) fn new(
        arena: &mut Arena,
        keys: &Buffer,
        values: &Buffer,
    ) -> Result<NonNull<Self>, Error> {
        if keys.len != values.len {
            return Err(bounds_error(
                "Paired iterator columns have different lengths",
            ));
        }
        let layout = Layout::new::<Self>();
        let result = arena
            .allocate(layout.size(), layout.align())?
            .cast::<Self>();
        let cursor = |buffer: &Buffer| BufferIterator {
            data: buffer.data,
            len: buffer.len,
            stride: buffer.element.size(),
            position: Cell::new(0),
        };
        // All metadata is initialized before publishing this task-local descriptor.
        unsafe {
            result.as_ptr().write(Self {
                keys: cursor(keys),
                values: cursor(values),
            });
        }
        Ok(result)
    }
    pub(crate) fn advance(&self) -> bool {
        let next = self.keys.advance();
        self.values.advance();
        next
    }
    /// # Safety
    /// The cursor has just advanced successfully, column is 0 or 1, and the
    /// source loan keeps both initialized columns stable on the task thread.
    /// Generated pair bindings capture both addresses before executing user code.
    pub(crate) unsafe fn project(&self, column: usize) -> *mut u8 {
        let cursor = if column == 0 {
            &self.keys
        } else {
            &self.values
        };
        let position = cursor.position.get();
        debug_assert!(column < 2 && position > 0 && position <= cursor.len);
        cursor.data.as_ptr().add((position - 1) * cursor.stride)
    }
    pub(crate) fn current(&self, arena: &mut Arena) -> Result<NonNull<BufferEntry>, Error> {
        let entry = BufferEntry {
            key: self.keys.current()?,
            value: self.values.current()?,
        };
        let layout = Layout::new::<BufferEntry>();
        let result = arena
            .allocate(layout.size(), layout.align())?
            .cast::<BufferEntry>();
        // Snapshots preserve entry identity across subsequent shared advances.
        unsafe {
            result.as_ptr().write(entry);
        }
        Ok(result)
    }
}

pub(crate) struct OwningGraph {
    pub allocations: Vec<NonNull<u8>>,
    pub buffers: Vec<NonNull<Buffer>>,
    pub metadata: usize,
}

#[derive(Clone)]
pub(crate) enum ElementCallback {
    Native(super::cleanup::Callback),
    Interpreted(super::cleanup::InterpretedCallback),
    Portable(super::cleanup::PortableCallback),
}
impl ElementCallback {
    pub(crate) fn portable(&self) -> Option<super::cleanup::PortableCallback> {
        match self {
            Self::Native(callback) => Some(super::cleanup::PortableCallback::Native(*callback)),
            Self::Portable(callback) => Some(callback.clone()),
            Self::Interpreted(_) => None,
        }
    }
}
#[derive(Clone)]
pub(crate) struct ElementHook {
    pub before: usize,
    pub offset: usize,
    pub indirect: bool,
    pub guards: Vec<(usize, i32)>,
    pub callback: ElementCallback,
}
#[derive(Clone, Default)]
pub(crate) struct ElementCleanup {
    pub owners: Vec<usize>,
    pub hooks: Vec<ElementHook>,
}
impl ElementCleanup {
    /// Logical descriptor entries, excluding allocator capacity and pinned code.
    pub(crate) fn metadata_bytes(&self) -> Option<usize> {
        self.owners
            .len()
            .checked_mul(std::mem::size_of::<usize>())
            .and_then(|n| {
                n.checked_add(
                    self.hooks
                        .len()
                        .checked_mul(std::mem::size_of::<ElementHook>())?,
                )
            })
            .and_then(|n| {
                self.hooks.iter().try_fold(n, |n, hook| {
                    n.checked_add(
                        hook.guards
                            .len()
                            .checked_mul(std::mem::size_of::<(usize, i32)>())?,
                    )
                })
            })
    }

    /// Build cleanup for an initialized element, testing outer tags first.
    ///
    /// # Safety
    /// Data must address a live element with the descriptor's byte layout,
    /// including valid Int32 tag and buffer lanes. Its uniquely owned buffers,
    /// hook bytes and callback code must remain live on this task thread.
    pub(crate) unsafe fn entry(&self, data: *mut u8) -> super::cleanup::Entry {
        super::cleanup::Entry {
            keep_alive: None,
            storage_size: 0,
            storage: Some((data, self.clone())),
            owners: vec![],
            hooks: vec![],
        }
    }

    /// Run current children after their enclosing hooks, which may replace them.
    /// The caller must uphold the same live storage contract as `entry`.
    pub(crate) unsafe fn run(&self, data: *mut u8, arena: &mut Arena) -> Result<(), Error> {
        let mut error = None;
        let mut hooks = self.hooks.iter().peekable();
        for (index, offset) in self.owners.iter().enumerate() {
            if *offset == usize::MAX {
                continue;
            }
            while hooks.peek().is_some_and(|hook| hook.before == index) {
                if let Some(callback) = hooks.next().unwrap().bind(data) {
                    if let Err(next) = callback.run(arena) {
                        error.get_or_insert(next);
                    }
                }
            }
            let owner = data.add(*offset).cast::<*mut Buffer>().read_unaligned();
            if !owner.is_null() {
                if let Err(next) = Buffer::release(arena, owner) {
                    error.get_or_insert(next);
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Detach the current cleanup when an owner moves out of this storage.
    /// The caller must uphold the same live storage contract as `entry`.
    pub(crate) unsafe fn snapshot(&self, data: *mut u8) -> super::cleanup::Entry {
        super::cleanup::Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners: self
                .owners
                .iter()
                .map(|offset| {
                    if *offset == usize::MAX {
                        return None;
                    }
                    NonNull::new(data.add(*offset).cast::<*mut Buffer>().read_unaligned())
                        .map(NonNull::as_ptr)
                })
                .collect(),
            hooks: self
                .hooks
                .iter()
                .filter_map(|hook| hook.bind(data).map(|callback| (hook.before, callback)))
                .collect(),
        }
    }
}

impl ElementHook {
    unsafe fn bind(&self, data: *mut u8) -> Option<super::cleanup::Hook> {
        if !self
            .guards
            .iter()
            .all(|(offset, tag)| data.add(*offset).cast::<i32>().read_unaligned() == *tag)
        {
            return None;
        }
        let pointer = data.add(self.offset);
        let data = if self.indirect {
            pointer.cast::<*mut u8>().read_unaligned()
        } else {
            pointer
        };
        Some(match &self.callback {
            ElementCallback::Native(callback) => super::cleanup::Hook::Native {
                data,
                callback: *callback,
            },
            ElementCallback::Interpreted(callback) => super::cleanup::Hook::Interpreted {
                data,
                callback: callback.clone(),
            },
            ElementCallback::Portable(callback) => super::cleanup::Hook::Portable {
                data,
                callback: callback.clone(),
            },
        })
    }
}

fn capacity_error() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Buffer capacity exceeds the target address range",
    )
}
fn bounds_error(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::RuntimeError, message)
}

impl Buffer {
    fn empty(element: Layout) -> Self {
        Self {
            // No bytes are accessed for empty or zero-sized storage.
            origin: std::ptr::null(),
            loan: std::ptr::null(),
            owner_task: 0,
            data: NonNull::new(element.align() as *mut u8).unwrap(),
            len: 0,
            capacity: 0,
            element,
            cleanup: None,
            failed_store: None,
            metadata_budget: std::ptr::null(),
            metadata_account: std::ptr::null(),
            metadata_charge: 0,
            metadata_parallel: None,
            failed_descriptor: false,
            release_parent: std::ptr::null_mut(),
            release_owner: 0,
            release_hook: 0,
            release_active: false,
            dynamic: None,
            resource: None,
        }
    }

    pub(crate) fn resource_ref(&self) -> Result<&super::resource::OwnedResource, Error> {
        self.resource
            .as_ref()
            .ok_or_else(|| bounds_error("Expected a registered resource owner"))
    }

    pub(crate) fn resource_mut(&mut self) -> Result<&mut super::resource::OwnedResource, Error> {
        self.resource
            .as_mut()
            .ok_or_else(|| bounds_error("Expected a registered resource owner"))
    }

    /// External resources reuse only the managed control block and arena accounting.
    pub(crate) fn external(
        arena: &mut Arena,
        resource: super::resource::OwnedResource,
    ) -> Result<NonNull<Self>, Error> {
        let mut owner = Self::allocate(arena, 0, 0, 1)?;
        // No fallible step follows adoption. Failed allocation drops resource here.
        unsafe {
            owner.as_mut().resource = Some(resource);
        }
        Ok(owner)
    }

    pub(crate) fn capacity_limit(stride: usize) -> u64 {
        // Zero-sized elements still have a bounded logical capacity and length.
        (isize::MAX as usize / stride.max(1)) as u64
    }

    pub(crate) fn allocate(
        arena: &mut Arena,
        capacity: u64,
        size: usize,
        align: usize,
    ) -> Result<NonNull<Self>, Error> {
        let element = Layout::from_size_align(size, align)
            .map_err(|_| capacity_error())?
            .pad_to_align();
        if capacity > Self::capacity_limit(element.size()) {
            return Err(capacity_error());
        }
        let capacity = usize::try_from(capacity).map_err(|_| capacity_error())?;
        let bytes = element
            .size()
            .checked_mul(capacity)
            .ok_or_else(capacity_error)?;
        let payload = Layout::from_size_align(bytes, align).map_err(|_| capacity_error())?;
        let layout = Layout::new::<Self>();
        let owner = arena
            .allocate(layout.size(), layout.align())?
            .cast::<Self>();
        // Publish only after both allocations succeed. Failures remain charged
        // to the arena and are released together with the root task.
        let mut value = Self::empty(element);
        value.origin = owner.as_ptr();
        value.owner_task = super::parallel::pool::identity().unwrap_or(0);
        if bytes != 0 {
            value.data = arena.allocate(payload.size(), payload.align())?;
        }
        value.capacity = capacity;
        value.metadata_budget = arena.budget_pointer();
        value.metadata_account = arena.account_pointer();
        value.metadata_parallel = arena.metadata_lease();
        unsafe {
            owner.as_ptr().write(value);
        }
        if let Some(root) = arena.parallel_root() {
            // A later failure may precede language cleanup registration. The
            // allocation owns this thread-safe accounting field independently.
            root.finalize_with(owner.cast(), Self::finalize_parallel_metadata);
        }
        Ok(owner)
    }

    unsafe fn finalize_parallel_metadata(pointer: *mut u8) {
        (*pointer.cast::<Self>()).metadata_parallel.take();
    }

    pub(crate) fn len(&self) -> u64 {
        self.len as u64
    }

    /// Copy an initialized byte prefix into independently owned task storage.
    pub(crate) fn copy_bytes(arena: &mut Arena, bytes: &[u8]) -> Result<NonNull<Self>, Error> {
        let mut owner = Self::allocate(arena, bytes.len() as u64, 1, 1)?;
        // No fallible operation or source callback occurs after allocation.
        unsafe {
            if !bytes.is_empty() {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    owner.as_ref().data.as_ptr(),
                    bytes.len(),
                );
            }
            owner.as_mut().len = bytes.len();
        }
        Ok(owner)
    }

    /// Borrow only initialized UInt8 storage; object layouts are never reinterpreted.
    pub(crate) fn bytes(&self) -> Result<&[u8], Error> {
        if self.element.size() != 1
            || self.element.align() != 1
            || self
                .cleanup
                .as_ref()
                .is_some_and(|cleanup| !cleanup.owners.is_empty() || !cleanup.hooks.is_empty())
            || self.failed_store.is_some()
        {
            return Err(bounds_error("Text requires plain initialized byte storage"));
        }
        // Buffer allocation and initialization prove this prefix remains live.
        Ok(unsafe { std::slice::from_raw_parts(self.data.as_ptr(), self.len) })
    }
    pub(crate) fn capacity(&self) -> u64 {
        self.capacity as u64
    }

    fn address(&self, index: usize) -> *mut u8 {
        // Allocation proves index * stride fits isize; ZSTs have offset zero.
        unsafe { self.data.as_ptr().add(index * self.element.size()) }
    }

    pub(crate) fn load(&self, index: u64) -> Result<*mut u8, Error> {
        let index = usize::try_from(index)
            .ok()
            .filter(|&i| i < self.len)
            .ok_or_else(|| bounds_error("Buffer index is outside the initialized prefix"))?;
        Ok(self.address(index))
    }

    pub(crate) fn iterator(&self, arena: &mut Arena) -> Result<NonNull<BufferIterator>, Error> {
        let layout = Layout::new::<BufferIterator>();
        let iterator = arena
            .allocate(layout.size(), layout.align())?
            .cast::<BufferIterator>();
        // Arena allocations are stable and retained until task cleanup. This
        // descriptor has no destructor and never takes payload ownership.
        unsafe {
            iterator.as_ptr().write(BufferIterator {
                data: self.data,
                len: self.len,
                stride: self.element.size(),
                position: Cell::new(0),
            });
        }
        Ok(iterator)
    }

    /// A borrowed header snapshots an initialized range, never owns its payload.
    /// Checked code keeps the parent buffer loan alive until the last view use.
    pub(crate) fn slice(
        &self,
        arena: &mut Arena,
        start: u64,
        end: u64,
    ) -> Result<NonNull<Self>, Error> {
        if start > end || end > self.len() {
            return Err(bounds_error(
                "Buffer slice is outside the initialized prefix",
            ));
        }
        // The parent allocation already proved every offset fits usize/isize.
        unsafe {
            let mut view = Self::view(
                arena,
                self.address(start as usize),
                (end - start) as usize,
                self.element,
            )?;
            view.as_mut().origin = self.origin;
            view.as_mut().loan = self.loan;
            Ok(view)
        }
    }

    /// A current-task owner or a received, bounded loan. Both origin pointers
    /// and loan pointers are local; only the immutable original lease is forwarded.
    pub(crate) fn scoped_range(&self, element: Layout) -> Option<super::parallel::scoped::Source> {
        self.scoped_elements(element, std::iter::empty())
    }
    pub(crate) fn scoped_elements(
        &self,
        element: Layout,
        owners: impl Iterator<Item = usize>,
    ) -> Option<super::parallel::scoped::Source> {
        let task = super::parallel::pool::identity()?;
        if let Some(loan) = unsafe { self.loan.as_ref() } {
            if self.element != element {
                return None;
            }
            return loan.source(self.data, self.len, self.capacity, element);
        }
        let origin = unsafe { self.origin.as_ref()? };
        if task == 0
            || origin.owner_task != task
            || origin.origin != self.origin
            || self.element != element
            || origin.element != element
            || self.capacity < self.len
            || self.capacity > origin.capacity
            || origin.resource.is_some()
            || origin.dynamic.is_some()
            || origin.failed_store.is_some()
            || origin.release_active
            || origin.failed_descriptor
            || origin.cleanup.as_ref().is_some_and(|c| !c.hooks.is_empty())
            || origin.metadata_parallel.is_none()
        {
            return None;
        }
        if !origin
            .cleanup
            .as_ref()
            .into_iter()
            .flat_map(|c| c.owners.iter().copied())
            .eq(owners)
        {
            return None;
        }
        if !super::parallel::scoped::contains_range(
            origin.data,
            origin.len,
            self.data,
            self.len,
            element,
        ) {
            return None;
        }
        Some(super::parallel::scoped::Source {
            data: self.data,
            len: self.len,
            capacity: self.capacity,
            origin: None,
        })
    }
    /// # Safety
    /// Called only by a scoped input holding its source lease. The source stays
    /// immutable and live until this worker and all derived views are cleaned.
    /// `loan` belongs to this worker and outlives its execution and view cleanup.
    pub(crate) unsafe fn scoped_view(
        arena: &mut Arena,
        data: NonNull<u8>,
        len: usize,
        capacity: usize,
        element: Layout,
        loan: *const super::parallel::scoped::Loan,
    ) -> Result<NonNull<Self>, Error> {
        let mut view = Self::view(arena, data.as_ptr(), len, element)?;
        view.as_mut().loan = loan;
        // Observable source capacity is metadata only. Read bounds and transport
        // eligibility still use the initialized length, including copied tables.
        view.as_mut().capacity = capacity;
        Ok(view)
    }
    /// Exclusive owned-packet installation, never a borrowed view handoff.
    pub(crate) unsafe fn adopt_task(owner: *mut Self) {
        (*owner).origin = owner;
        (*owner).owner_task = super::parallel::pool::identity().unwrap_or(0);
        if let Some(root) = super::parallel::current() {
            root.adopt_allocation((*owner).data);
        }
    }

    /// Create a nonowning header retained by the root task arena.
    ///
    /// # Safety
    /// data must be nonnull, aligned and valid for len initialized contiguous
    /// elements of the supplied layout, including a valid sentinel for ZSTs.
    /// Checked borrows keep the payload live and enforce access permissions for
    /// every use of the header, on this task's thread. The header owns no payload.
    unsafe fn view(
        arena: &mut Arena,
        data: *mut u8,
        len: usize,
        element: Layout,
    ) -> Result<NonNull<Self>, Error> {
        let value = Self {
            origin: std::ptr::null(),
            loan: std::ptr::null(),
            owner_task: 0,
            data: NonNull::new(data).unwrap(),
            len,
            capacity: len,
            element,
            cleanup: None,
            failed_store: None,
            metadata_budget: std::ptr::null(),
            metadata_account: std::ptr::null(),
            metadata_charge: 0,
            metadata_parallel: None,
            failed_descriptor: false,
            release_parent: std::ptr::null_mut(),
            release_owner: 0,
            release_hook: 0,
            release_active: false,
            dynamic: None,
            resource: None,
        };
        let layout = Layout::new::<Self>();
        let view = arena
            .allocate(layout.size(), layout.align())?
            .cast::<Self>();
        unsafe {
            view.as_ptr().write(value);
        }
        Ok(view)
    }

    pub(crate) fn initialize_next(&self) -> Result<*mut u8, Error> {
        if self.len == self.capacity {
            return Err(bounds_error("Buffer initialization exceeds capacity"));
        }
        Ok(self.address(self.len))
    }

    /// Caller must have checked initialize_next and written every scalar field.
    /// No source call, suspension or pointer escape exists between those steps.
    pub(crate) unsafe fn commit_next(&mut self) {
        self.len += 1;
    }

    /// Initialize a zero-sized element prefix without iterating its logical length.
    pub(crate) fn initialize_empty(&mut self, count: u64) -> Result<(), Error> {
        let count = usize::try_from(count).map_err(|_| bounds_error("Buffer length overflow"))?;
        if self.element.size() != 0 || self.len != 0 || count > self.capacity {
            return Err(bounds_error("Invalid empty element initialization"));
        }
        self.len = count;
        Ok(())
    }

    /// Cleanup view of a detached element, used until its new local snapshot is
    /// installed. Its parent storage remains alive throughout this atomic transfer.
    ///
    /// # Safety
    /// data must address a detached initialized element of this buffer's layout.
    /// Its bytes, unique child owners and callback code must remain live until
    /// the returned entry runs or is replaced, on the task's execution thread.
    pub(crate) unsafe fn element_entry(&self, data: *mut u8) -> Option<super::cleanup::Entry> {
        self.cleanup.as_ref().map(|cleanup| cleanup.entry(data))
    }

    pub(crate) fn reserve_metadata(&mut self, charge: usize) -> Result<(), Error> {
        if let Some(lease) = &mut self.metadata_parallel {
            lease.resize(charge)?;
            self.metadata_charge = charge;
            return Ok(());
        }
        // Compiler-owned buffers remain registered with an arena pinning this
        // budget. Ordinary tasks have no shared instance budget and stay unchanged.
        if let Some(budget) = unsafe { self.metadata_budget.as_ref() } {
            let account = unsafe { &*self.metadata_account };
            if charge > self.metadata_charge {
                let additional = charge - self.metadata_charge;
                account.charge(additional)?;
                if let Err(error) = budget.charge(additional) {
                    account.refund(additional);
                    return Err(error);
                }
            } else {
                let released = self.metadata_charge - charge;
                budget.refund(released);
                account.refund(released);
            }
            self.metadata_charge = charge;
        }
        Ok(())
    }
    pub(crate) fn configure(&mut self, cleanup: ElementCleanup) -> Result<(), Error> {
        let charge = cleanup
            .metadata_bytes()
            .ok_or_else(|| bounds_error("Cleanup descriptor size overflow"))?;
        self.reserve_metadata(charge)?;
        self.cleanup = Some(cleanup);
        Ok(())
    }
    unsafe fn clear_metadata(&mut self) {
        self.cleanup.take();
        self.metadata_parallel.take();
        if let Some(budget) = self.metadata_budget.as_ref() {
            budget.refund(self.metadata_charge);
            (*self.metadata_account).refund(self.metadata_charge);
        }
        self.metadata_charge = 0;
    }

    /// Move the initialized prefix into empty, preallocated storage. All checks
    /// precede mutation; no source hook, allocation or suspension occurs here.
    ///
    /// # Safety
    /// Both pointers must identify live, exclusively borrowed buffers in the
    /// same task arena, with compatible compiler-installed element descriptors.
    /// No element view may remain live; execution stays on the task thread.
    pub(crate) unsafe fn transfer(source: *mut Self, destination: *mut Self) -> Result<(), Error> {
        if source == destination
            || (*source).element != (*destination).element
            || (*destination).len != 0
            || (*source).len > (*destination).capacity
        {
            return Err(bounds_error(
                "Buffer transfer requires distinct matching storage with enough empty capacity",
            ));
        }
        std::ptr::copy_nonoverlapping(
            (*source).data.as_ptr(),
            (*destination).data.as_ptr(),
            (*source).len * (*source).element.size(),
        );
        (*destination).len = (*source).len;
        (*source).len = 0;
        Ok(())
    }

    /// Exchange complete initialized elements without transferring cleanup responsibility.
    /// Checked code holds exclusive access to the buffer and all its elements.
    pub(crate) fn swap_elements(&mut self, first: u64, second: u64) -> Result<(), Error> {
        let first_data = self.load(first)?;
        let second_data = self.load(second)?;
        if first != second && self.element.size() != 0 {
            // Valid distinct indices address disjoint full element strides.
            // The untyped swap preserves padding and initialization state.
            unsafe {
                std::ptr::swap_nonoverlapping(first_data, second_data, self.element.size());
            }
        }
        Ok(())
    }

    pub(crate) fn take_last(&mut self) -> Result<*mut u8, Error> {
        if self.len == 0 {
            return Err(bounds_error("Cannot take_last from an empty array"));
        }
        self.len -= 1;
        Ok(self.address(self.len))
    }

    /// Old elements are detached before callbacks; subsequent failures cannot
    /// retry their hooks. The caller stops before writing a replacement on error.
    ///
    /// # Safety
    /// This buffer and all initialized child owners belong to arena, with no
    /// outstanding views. Installed callbacks and their code owners remain live
    /// on this task thread. The returned address must be fully written before
    /// any source call or suspension; on error, only terminal cleanup may follow.
    pub(crate) unsafe fn prepare_store(
        &mut self,
        arena: &mut Arena,
        index: u64,
    ) -> Result<*mut u8, Error> {
        let data = self.load(index)?;
        if let Some(cleanup) = &self.cleanup {
            let entry = cleanup.entry(data);
            // Hooks observe the original bytes. Clear owner lanes afterward so
            // terminal cleanup skips the detached element even if a hook failed.
            let result = entry.run(arena);
            for offset in &cleanup.owners {
                data.add(*offset)
                    .cast::<*mut Buffer>()
                    .write_unaligned(std::ptr::null_mut());
            }
            if result.is_err() {
                self.failed_store = Some(index as usize);
            }
            result?;
        }
        Ok(data)
    }

    /// Drop the removed suffix in reverse index order, continuing after failures.
    ///
    /// # Safety
    /// Initialized child handles must be unique owners in arena, all views must
    /// have ended, and installed callbacks/code must remain live on this task's
    /// thread. No source access may follow a failed destructor.
    pub(crate) unsafe fn truncate_owned(
        &mut self,
        arena: &mut Arena,
        new_len: u64,
    ) -> Result<(), Error> {
        let new_len = usize::try_from(new_len)
            .ok()
            .filter(|n| *n <= self.len)
            .ok_or_else(|| bounds_error("Buffer truncate cannot increase initialized length"))?;
        // Copy buffers (including enormous logical ZST arrays) have no element
        // destructors. Shrinking/releasing them must stay constant time.
        if self.cleanup.is_none() {
            self.len = new_len;
            return Ok(());
        }
        let mut error = None;
        while self.len > new_len {
            self.len -= 1;
            if let Some(cleanup) = &self.cleanup {
                let data = self.address(self.len);
                if self.failed_store == Some(self.len) {
                    continue;
                }
                if let Err(next) = cleanup.entry(data).run(arena) {
                    error.get_or_insert(next);
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    unsafe fn validate_owner(arena: &Arena, owner: *mut Self) -> Result<(), Error> {
        let header = NonNull::new(owner).ok_or_else(|| bounds_error("Invalid buffer owner"))?;
        if !arena.contains(header.cast()) {
            return Err(bounds_error(
                "Buffer owner is not registered with this task",
            ));
        }
        if (*owner).capacity != 0 && (*owner).element.size() != 0 && !arena.contains((*owner).data)
        {
            return Err(bounds_error(
                "Buffer payload is not registered with this task",
            ));
        }
        Ok(())
    }

    /// Commit the descriptor accounting alongside its allocation registrations.
    /// Both accounts are pinned by live arenas and the destination is preflighted.
    pub(crate) unsafe fn migrate_metadata(buffers: &[NonNull<Self>], arena: &Arena) {
        let target = arena.account_pointer();
        for buffer in buffers {
            let buffer = &mut *buffer.as_ptr();
            let old = &*buffer.metadata_account;
            old.refund(buffer.metadata_charge);
            (*target)
                .bytes
                .set((*target).bytes.get() + buffer.metadata_charge);
            buffer.metadata_account = target;
        }
    }

    /// Discover only allocations reachable through compiler-authored owning
    /// lanes, including every private Map array and StringBuilder byte array.
    /// This enumerates storage, not transfer eligibility; callers must separately
    /// check external resource and callback qualifications.
    /// No callback runs while collecting the graph.
    pub(crate) unsafe fn owning_allocations(
        arena: &Arena,
        roots: impl IntoIterator<Item = *mut Self>,
    ) -> Result<OwningGraph, Error> {
        let mut pending = Vec::new();
        for owner in roots {
            pending
                .try_reserve(1)
                .map_err(|_| bounds_error("Owner graph allocation failed"))?;
            if !owner.is_null() {
                pending.push(owner);
            }
        }
        let mut selected = Vec::new();
        let mut metadata = 0usize;
        let mut buffers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while let Some(owner) = pending.pop() {
            Self::validate_owner(arena, owner)?;
            seen.try_reserve(2)
                .map_err(|_| bounds_error("Owner graph allocation failed"))?;
            selected
                .try_reserve(2)
                .map_err(|_| bounds_error("Owner graph allocation failed"))?;
            let header = NonNull::new_unchecked(owner.cast());
            if !seen.insert(header) {
                return Err(bounds_error("Aliased persistent owner"));
            }
            selected.push(header);
            buffers
                .try_reserve(1)
                .map_err(|_| bounds_error("Owner graph allocation failed"))?;
            buffers.push(NonNull::new_unchecked(owner));
            let buffer = &*owner;
            metadata = metadata
                .checked_add(buffer.metadata_charge)
                .ok_or_else(|| bounds_error("Owner metadata size overflow"))?;
            if buffer.capacity != 0 && buffer.element.size() != 0 {
                if !seen.insert(buffer.data) {
                    return Err(bounds_error("Aliased persistent payload"));
                }
                selected.push(buffer.data);
            }
            if let Some(cleanup) = &buffer.cleanup {
                for index in 0..buffer.len {
                    if buffer.failed_store == Some(index) {
                        continue;
                    }
                    pending
                        .try_reserve(cleanup.owners.len())
                        .map_err(|_| bounds_error("Owner graph allocation failed"))?;
                    let data = buffer.address(index);
                    for &offset in &cleanup.owners {
                        if offset != usize::MAX {
                            let child = data.add(offset).cast::<*mut Self>().read();
                            if !child.is_null() {
                                pending.push(child);
                            }
                        }
                    }
                }
            }
        }
        Ok(OwningGraph {
            allocations: selected,
            buffers,
            metadata,
        })
    }

    /// Validate the concrete control blocks as well as the static value type.
    /// Only audited TCP resources may cross; reject local callbacks, accounting
    /// pointers, active resource leases and in-progress cleanup.
    pub(crate) unsafe fn validate_parallel_owner(
        arena: &Arena,
        owner: *mut Self,
    ) -> Result<(), Error> {
        Self::validate_parallel_value(arena, owner, false)
    }

    pub(crate) unsafe fn has_user_cleanup(
        arena: &Arena,
        owners: impl IntoIterator<Item = *mut Self>,
    ) -> Result<bool, Error> {
        Ok(Self::owning_allocations(arena, owners)?
            .buffers
            .iter()
            .any(|p| {
                p.as_ref()
                    .cleanup
                    .as_ref()
                    .is_some_and(|c| !c.hooks.is_empty())
            }))
    }

    /// The caller pins qualified code and guarantees worker-side disposal when
    /// admitting portable callbacks. Ordinary buffer packets remain hook-free.
    pub(crate) unsafe fn validate_parallel_value(
        arena: &Arena,
        owner: *mut Self,
        callbacks: bool,
    ) -> Result<(), Error> {
        if arena.parallel_root().is_none() {
            return Err(bounds_error("Owner has no parallel storage root"));
        }
        let graph = Self::owning_allocations(arena, [owner])?;
        for pointer in graph.buffers {
            let buffer = pointer.as_ref();
            if buffer.resource.as_ref().is_some_and(|r| !r.parallel_safe())
                || !buffer.metadata_budget.is_null()
                || !buffer.metadata_account.is_null()
                || buffer.metadata_parallel.is_none()
                || buffer.release_active
                || buffer.failed_descriptor
                || buffer.failed_store.is_some()
                || buffer.cleanup.as_ref().is_some_and(|cleanup| {
                    cleanup
                        .hooks
                        .iter()
                        .any(|h| !callbacks || h.callback.portable().is_none())
                })
            {
                return Err(bounds_error("Buffer has Local cleanup or resource state"));
            }
        }
        Ok(())
    }

    /// Deep-copy a validated hook-free graph, preserving capacity and descriptors.
    /// No destination owner refers to source storage, even on partial failure.
    ///
    /// # Safety
    /// Roots must be live, uniquely owned buffers registered in `source`. Both
    /// arenas and their budget accounts must remain live throughout the copy.
    pub(crate) unsafe fn copy_owners(
        source: &Arena,
        roots: &[*mut Self],
        destination: &mut Arena,
    ) -> Result<Vec<*mut Self>, Error> {
        let graph = Self::owning_allocations(source, roots.iter().copied())?;
        let _scratch = destination.reserve_external(
            graph.allocations.capacity() * std::mem::size_of::<NonNull<u8>>()
                + graph.buffers.capacity() * std::mem::size_of::<NonNull<Self>>()
                + (graph.buffers.len() + roots.len()) * std::mem::size_of::<*mut Self>(),
        )?;
        let mut copies: Vec<*mut Self> = Vec::new();
        copies
            .try_reserve_exact(graph.buffers.len())
            .map_err(|_| capacity_error())?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(roots.len())
            .map_err(|_| capacity_error())?;
        let copied = (|| {
            for original in &graph.buffers {
                let original = original.as_ref();
                if original.resource.is_some()
                    || original.failed_store.is_some()
                    || original.failed_descriptor
                    || original.release_active
                    || original
                        .cleanup
                        .as_ref()
                        .is_some_and(|c| !c.hooks.is_empty())
                {
                    return Err(bounds_error(
                        "Snapshot requires a live hook-free owning graph",
                    ));
                }
                let copy = Self::allocate(
                    destination,
                    original.capacity as u64,
                    original.element.size(),
                    original.element.align(),
                )?
                .as_ptr();
                copies.push(copy);
                if let Some(cleanup) = &original.cleanup {
                    let mut owners = Vec::new();
                    owners
                        .try_reserve_exact(cleanup.owners.len())
                        .map_err(|_| capacity_error())?;
                    owners.extend_from_slice(&cleanup.owners);
                    (*copy).configure(ElementCleanup {
                        owners,
                        hooks: Vec::new(),
                    })?;
                }
                if original.element.size() != 0 {
                    std::ptr::copy_nonoverlapping(
                        original.data.as_ptr(),
                        (*copy).data.as_ptr(),
                        original.len * original.element.size(),
                    );
                }
                // Until every allocation succeeds, each copied header cleans up
                // only itself. Never follow an original child during unwinding.
                if let Some(cleanup) = &original.cleanup {
                    for index in 0..original.len {
                        for &offset in &cleanup.owners {
                            if offset != usize::MAX {
                                (*copy)
                                    .address(index)
                                    .add(offset)
                                    .cast::<*mut Self>()
                                    .write_unaligned(std::ptr::null_mut());
                            }
                        }
                    }
                }
                (*copy).len = original.len;
            }
            Ok::<_, Error>(())
        })();
        if let Err(error) = copied {
            for copy in copies {
                let _ = Self::release(destination, copy);
            }
            return Err(error);
        }
        let translated = |pointer: *mut Self| {
            if pointer.is_null() {
                pointer
            } else {
                copies[graph
                    .buffers
                    .iter()
                    .position(|p| p.as_ptr() == pointer)
                    .unwrap()]
            }
        };
        // Linking cannot fail. All ownership lanes were validated by discovery.
        for (original, &copy) in graph.buffers.iter().zip(&copies) {
            let original = original.as_ref();
            if let Some(cleanup) = &original.cleanup {
                for index in 0..original.len {
                    for &offset in &cleanup.owners {
                        if offset != usize::MAX {
                            let child = original
                                .address(index)
                                .add(offset)
                                .cast::<*mut Self>()
                                .read_unaligned();
                            (*copy)
                                .address(index)
                                .add(offset)
                                .cast::<*mut Self>()
                                .write_unaligned(translated(child));
                        }
                    }
                }
            }
        }
        result.extend(roots.iter().map(|&root| translated(root)));
        Ok(result)
    }

    /// Consume an unborrowed owner. Stored references still point at independent
    /// arena objects; their task-long shared loans are not released here.
    pub(crate) unsafe fn release(arena: &mut Arena, owner: *mut Self) -> Result<(), Error> {
        Self::validate_owner(arena, owner)?;
        let mut current = owner;
        let mut error = None;
        while !current.is_null() {
            let buffer = &mut *current;
            if buffer.cleanup.is_none() || buffer.len == 0 && !buffer.release_active {
                let parent = buffer.release_parent;
                if buffer.capacity != 0 && buffer.element.size() != 0 {
                    arena.release(buffer.data);
                }
                buffer.resource.take();
                buffer.dynamic.take();
                buffer.clear_metadata();
                arena.release(NonNull::new_unchecked(current.cast()));
                current = parent;
                continue;
            }
            if !buffer.release_active {
                buffer.len -= 1;
                if buffer.failed_store == Some(buffer.len) {
                    continue;
                }
                buffer.release_owner = 0;
                buffer.release_hook = 0;
                buffer.release_active = true;
            }
            let cleanup = buffer.cleanup.as_ref().unwrap();
            if buffer.release_owner == cleanup.owners.len() {
                buffer.release_active = false;
                continue;
            }
            let offset = cleanup.owners[buffer.release_owner];
            if offset == usize::MAX {
                buffer.release_owner += 1;
                continue;
            }
            let data = buffer.address(buffer.len);
            if let Some(hook) = cleanup
                .hooks
                .get(buffer.release_hook)
                .filter(|hook| hook.before == buffer.release_owner)
            {
                let callback = hook.bind(data);
                buffer.release_hook += 1;
                if let Some(callback) = callback {
                    if let Err(next) = callback.run(arena) {
                        error.get_or_insert(next);
                    }
                }
                continue;
            }
            let child = data.add(offset).cast::<*mut Self>().read_unaligned();
            buffer.release_owner += 1;
            if !child.is_null() {
                if let Err(next) = Self::validate_owner(arena, child) {
                    error.get_or_insert(next);
                    continue;
                }
                (*child).release_parent = current;
                current = child;
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Exchange owned contents while retaining each stable header's identity.
    /// # Safety
    /// Both pointers are live, exclusively borrowed owners with matching layouts
    /// on the same task. No element or slice loans remain live during the swap.
    pub(crate) unsafe fn swap(left: *mut Self, right: *mut Self) {
        std::ptr::swap(left, right);
        // Provenance points to the header that now owns the payload. Keeping
        // the exchanged pointers could follow a retired temporary after replace.
        (*left).origin = left;
        (*right).origin = right;
    }

    /// Both handles are live, exclusively accessible, same-element owners.
    /// Transfer the source payload, releasing the old payload and source header.
    pub(crate) unsafe fn replace(
        arena: &mut Arena,
        destination: *mut Self,
        source: *mut Self,
    ) -> Result<(), Error> {
        Self::validate_owner(arena, destination)?;
        Self::validate_owner(arena, source)?;
        if destination == source {
            return Err(bounds_error("Buffer replacement requires distinct owners"));
        }
        if (*destination).element != (*source).element {
            return Err(bounds_error(
                "Buffer replacement requires matching element layouts",
            ));
        }
        if let Err(error) = (*destination).truncate_owned(arena, 0) {
            let _ = Self::release(arena, source);
            return Err(error);
        }
        (*destination).resource.take();
        (*destination).clear_metadata();
        // The parallel allocation finalizer still runs for the retired header.
        // Leave it inert so it cannot refund/drop the lease moved to destination.
        let value = source.replace(Self::empty((*source).element));
        if (*destination).capacity != 0 && (*destination).element.size() != 0 {
            arena.release((*destination).data);
        }
        destination.write(value);
        (*destination).origin = destination;
        arena.release(NonNull::new_unchecked(source.cast()));
        Ok(())
    }
}

#[cfg(test)]
#[path = "buffer/scoped_tests.rs"]
mod scoped_tests;

/// Project a checked fixed-array element or construct a borrowed slice header.
///
/// # Safety
/// base identifies length initialized elements of the compiler-validated size
/// and alignment (total size fits isize), with a nonnull aligned ZST sentinel.
/// Its source borrow covers all returned accesses and outlives them. Only the
/// owning task thread may access the arena; no ownership of base is transferred.
pub(crate) unsafe fn fixed_view(
    arena: &mut Arena,
    base: *mut u8,
    length: u64,
    size: usize,
    align: usize,
    from: u64,
    to: Option<u64>,
) -> Result<*mut u8, Error> {
    if to.map_or(from >= length, |to| from > to || to > length) {
        return Err(bounds_error("FixedArray index or slice is out of bounds"));
    }
    let data = base.add(from as usize * size);
    match to {
        None => Ok(data),
        Some(to) => Buffer::view(
            arena,
            data,
            (to - from) as usize,
            Layout::from_size_align(size, align).unwrap(),
        )
        .map(|view| view.as_ptr().cast()),
    }
}

/// # Safety
/// Same validated layout, source-borrow lifetime and task-thread contract as
/// fixed_view; arena must point to the live current task arena.
#[export_name = "__joky_place_view"]
unsafe extern "C" fn place_view(
    arena: *mut Arena,
    base: *mut u8,
    length: u64,
    size: usize,
    align: usize,
    from: u64,
    to: u64,
    slice: usize,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located(
        fixed_view(
            &mut *arena,
            base,
            length,
            size,
            align,
            from,
            (slice != 0).then_some(to),
        ),
        source,
        start,
        end,
    )
}

fn located<T: Default>(result: Result<T, Error>, source: u64, start: usize, end: usize) -> T {
    result.unwrap_or_else(|error| {
        let error = if error.span().is_some() {
            error
        } else {
            error.with_span(Span::new(start, end).with_source(SourceId::from_raw(source)))
        };
        super::error::record_error(error);
        T::default()
    })
}

#[export_name = "__joky_buffer_allocate"]
unsafe extern "C" fn allocate(
    arena: *mut Arena,
    capacity: u64,
    size: usize,
    align: usize,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        Buffer::allocate(&mut *arena, capacity, size, align).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
#[export_name = "__joky_buffer_len"]
unsafe extern "C" fn len(buffer: *const Buffer) -> u64 {
    (*buffer).len()
}
#[export_name = "__joky_buffer_capacity"]
unsafe extern "C" fn capacity(buffer: *const Buffer) -> u64 {
    (*buffer).capacity()
}
#[export_name = "__joky_buffer_capacity_limit"]
extern "C" fn capacity_limit(stride: usize) -> u64 {
    Buffer::capacity_limit(stride)
}
#[export_name = "__joky_buffer_load"]
unsafe extern "C" fn load(
    buffer: *const Buffer,
    index: u64,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located((*buffer).load(index), source, start, end)
}
#[export_name = "__joky_buffer_store"]
unsafe extern "C" fn store(
    arena: *mut Arena,
    buffer: *mut Buffer,
    index: u64,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located(
        (*buffer).prepare_store(&mut *arena, index),
        source,
        start,
        end,
    )
}

#[export_name = "__joky_dyn_pack"]
unsafe extern "C" fn dyn_pack(owner: *mut Buffer, table: usize, indirect: usize) {
    (*owner).dynamic = Some(DynDispatch::Native {
        table,
        indirect: indirect != 0,
    });
}
#[export_name = "__joky_dyn_data"]
unsafe extern "C" fn dyn_data(owner: *mut Buffer) -> *mut u8 {
    let pointer = (*owner).data.as_ptr();
    match (*owner).dynamic {
        Some(DynDispatch::Native { indirect: true, .. }) => pointer.cast::<*mut u8>().read(),
        _ => pointer,
    }
}
#[export_name = "__joky_dyn_table"]
unsafe extern "C" fn dyn_table(owner: *mut Buffer) -> usize {
    match (*owner).dynamic {
        Some(DynDispatch::Native { table, .. }) => table,
        _ => unreachable!("native dynamic object"),
    }
}

#[export_name = "__joky_buffer_initialize_next"]
unsafe extern "C" fn initialize_next(
    buffer: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located((*buffer).initialize_next(), source, start, end)
}
#[export_name = "__joky_buffer_commit_next"]
unsafe extern "C" fn commit_next(buffer: *mut Buffer) {
    (*buffer).commit_next();
}
#[export_name = "__joky_buffer_truncate"]
unsafe extern "C" fn truncate(
    arena: *mut Arena,
    buffer: *mut Buffer,
    new_len: u64,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        (*buffer).truncate_owned(&mut *arena, new_len),
        source,
        start,
        end,
    );
}
#[export_name = "__joky_buffer_replace"]
unsafe extern "C" fn replace(
    arena: *mut Arena,
    destination: *mut Buffer,
    source_buffer: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        Buffer::replace(&mut *arena, destination, source_buffer),
        source,
        start,
        end,
    );
}

/// Create a cursor over initialized buffer storage.
/// # Safety
/// arena and buffer are live on the owning task thread; arena is exclusive.
/// Checked source loans must keep the payload stable for all uses of the result.
/// The returned descriptor belongs to arena and must never be released as an owner.
#[export_name = "__joky_buffer_iter"]
unsafe extern "C" fn iter(
    arena: *mut Arena,
    buffer: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut BufferIterator {
    located(
        (*buffer).iterator(&mut *arena).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
/// # Safety
/// iterator points to a live BufferIterator on its owning task thread. Its
/// descriptor and borrowed payload remain live; concurrent access is forbidden.
#[export_name = "__joky_buffer_iter_advance"]
unsafe extern "C" fn iter_advance(iterator: *const BufferIterator) -> u8 {
    (*iterator).advance() as u8
}
/// # Safety
/// iterator and its initialized payload remain live on their owning task thread.
/// The caller uses the element's checked layout and enforces shared/exclusive
/// source loans for all accesses through the returned address.
#[export_name = "__joky_buffer_iter_current"]
unsafe extern "C" fn iter_current(
    iterator: *const BufferIterator,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located((*iterator).current(), source, start, end)
}

/// # Safety
/// Both initialized columns have compiler-checked element layouts and live on
/// the exclusive arena's task thread. Source loans keep their payloads stable.
#[export_name = "__joky_buffer_pair_iter"]
unsafe extern "C" fn pair_iter(
    arena: *mut Arena,
    keys: *const Buffer,
    values: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut BufferPairIterator {
    located(
        BufferPairIterator::new(&mut *arena, &*keys, &*values).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
/// # Safety
/// The cursor and columns are live on the owning task thread; checked loans
/// exclude conflicting exclusive entries and serialize cursor access.
#[export_name = "__joky_buffer_pair_advance"]
unsafe extern "C" fn pair_advance(cursor: *const BufferPairIterator) -> u8 {
    (*cursor).advance() as u8
}
/// # Safety
/// Cursor, columns and exclusive arena are live on one task thread. Returned
/// metadata is arena-owned; source loans retain the referenced initialized items.
#[export_name = "__joky_buffer_pair_current"]
unsafe extern "C" fn pair_current(
    arena: *mut Arena,
    cursor: *const BufferPairIterator,
    source: u64,
    start: usize,
    end: usize,
) -> *mut BufferEntry {
    located(
        (*cursor).current(&mut *arena).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
/// # Safety
/// The live cursor, selected column and source loans obey BufferPairIterator::project.
#[export_name = "__joky_buffer_pair_project"]
unsafe extern "C" fn pair_project(cursor: *const BufferPairIterator, column: usize) -> *mut u8 {
    (*cursor).project(column)
}
/// # Safety
/// Entry and its key remain live on the owning task thread. The caller uses
/// the checked key layout and grants only shared access to the returned storage.
#[export_name = "__joky_buffer_entry_key"]
unsafe extern "C" fn entry_key(entry: *const BufferEntry) -> *mut u8 {
    (*entry).key
}
/// # Safety
/// Entry and its value remain live on the owning task thread. The caller uses
/// the checked value layout and enforces the source loan's access permissions.
#[export_name = "__joky_buffer_entry_value"]
unsafe extern "C" fn entry_value(entry: *const BufferEntry) -> *mut u8 {
    (*entry).value
}

#[export_name = "__joky_buffer_slice"]
unsafe extern "C" fn slice(
    arena: *mut Arena,
    buffer: *const Buffer,
    from: u64,
    to: u64,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        (*buffer).slice(&mut *arena, from, to).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}
#[export_name = "__joky_buffer_element_load"]
unsafe extern "C" fn element_load(
    element: *mut u8,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located(
        NonNull::new(element)
            .map(NonNull::as_ptr)
            .ok_or_else(|| bounds_error("Invalid buffer element view")),
        source,
        start,
        end,
    )
}

#[export_name = "__joky_buffer_release"]
unsafe extern "C" fn release(
    arena: *mut Arena,
    owner: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(Buffer::release(&mut *arena, owner), source, start, end);
}

#[export_name = "__joky_buffer_transfer"]
unsafe extern "C" fn transfer(
    source_buffer: *mut Buffer,
    destination: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        Buffer::transfer(source_buffer, destination),
        source,
        start,
        end,
    );
}
/// # Safety
/// buffer must be a live compiler-created buffer with exclusive element access
/// on its task thread. Its payload and child owners remain live for this call.
#[export_name = "__joky_buffer_swap_elements"]
unsafe extern "C" fn swap_elements(
    buffer: *mut Buffer,
    first: u64,
    second: u64,
    source: u64,
    start: usize,
    end: usize,
) {
    located((*buffer).swap_elements(first, second), source, start, end);
}
#[export_name = "__joky_buffer_take_last"]
unsafe extern "C" fn take_last(
    buffer: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located((*buffer).take_last(), source, start, end)
}
#[export_name = "__joky_buffer_cleanup_new"]
unsafe extern "C" fn cleanup_new(buffer: *mut Buffer, charge: usize) {
    if let Some(buffer) = buffer.as_mut() {
        match buffer.reserve_metadata(charge) {
            Ok(()) => {
                buffer.failed_descriptor = false;
                buffer.cleanup = Some(ElementCleanup::default());
            }
            Err(error) => {
                buffer.failed_descriptor = true;
                super::error::record_error(error);
            }
        }
    }
}
#[export_name = "__joky_buffer_cleanup_owner"]
unsafe extern "C" fn cleanup_owner(buffer: *mut Buffer, offset: usize) {
    if let Some(buffer) = buffer.as_mut().filter(|buffer| !buffer.failed_descriptor) {
        buffer.cleanup.as_mut().unwrap().owners.push(offset);
    }
}
#[export_name = "__joky_buffer_cleanup_hook"]
unsafe extern "C" fn cleanup_hook(
    buffer: *mut Buffer,
    before: usize,
    offset: usize,
    indirect: usize,
    callback: super::cleanup::Callback,
) {
    let Some(buffer) = buffer.as_mut().filter(|buffer| !buffer.failed_descriptor) else {
        return;
    };
    buffer.cleanup.as_mut().unwrap().hooks.push(ElementHook {
        before,
        offset,
        indirect: indirect != 0,
        guards: Vec::new(),
        callback: ElementCallback::Native(callback),
    });
}

/// Append an enclosing enum tag condition to the last element hook.
///
/// # Safety
/// Buffer is null or a live, uniquely owned control block whose last hook has
/// been installed. Offset addresses an Int32 tag in its element layout. The
/// descriptor and callback code must remain live; callers serialize task access.
#[export_name = "__joky_buffer_cleanup_guard"]
unsafe extern "C" fn cleanup_guard(buffer: *mut Buffer, offset: usize, tag: i32) {
    if let Some(buffer) = buffer.as_mut().filter(|buffer| !buffer.failed_descriptor) {
        buffer
            .cleanup
            .as_mut()
            .unwrap()
            .hooks
            .last_mut()
            .unwrap()
            .guards
            .push((offset, tag));
    }
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 33] {
    [
        ("__joky_buffer_pair_iter", pair_iter as *const u8),
        ("__joky_buffer_pair_advance", pair_advance as *const u8),
        ("__joky_buffer_pair_current", pair_current as *const u8),
        ("__joky_buffer_pair_project", pair_project as *const u8),
        ("__joky_buffer_entry_key", entry_key as *const u8),
        ("__joky_buffer_entry_value", entry_value as *const u8),
        ("__joky_buffer_iter", iter as *const u8),
        ("__joky_buffer_iter_advance", iter_advance as *const u8),
        ("__joky_buffer_iter_current", iter_current as *const u8),
        ("__joky_place_view", place_view as *const u8),
        ("__joky_buffer_transfer", transfer as *const u8),
        ("__joky_buffer_take_last", take_last as *const u8),
        ("__joky_buffer_swap_elements", swap_elements as *const u8),
        ("__joky_buffer_cleanup_new", cleanup_new as *const u8),
        ("__joky_buffer_cleanup_owner", cleanup_owner as *const u8),
        ("__joky_buffer_cleanup_hook", cleanup_hook as *const u8),
        ("__joky_buffer_cleanup_guard", cleanup_guard as *const u8),
        ("__joky_buffer_allocate", allocate as *const u8),
        ("__joky_buffer_release", release as *const u8),
        ("__joky_buffer_slice", slice as *const u8),
        ("__joky_buffer_element_load", element_load as *const u8),
        ("__joky_buffer_len", len as *const u8),
        ("__joky_buffer_capacity", capacity as *const u8),
        ("__joky_buffer_capacity_limit", capacity_limit as *const u8),
        ("__joky_buffer_load", load as *const u8),
        ("__joky_buffer_store", store as *const u8),
        ("__joky_dyn_pack", dyn_pack as *const u8),
        ("__joky_dyn_data", dyn_data as *const u8),
        ("__joky_dyn_table", dyn_table as *const u8),
        (
            "__joky_buffer_initialize_next",
            initialize_next as *const u8,
        ),
        ("__joky_buffer_commit_next", commit_next as *const u8),
        ("__joky_buffer_truncate", truncate as *const u8),
        ("__joky_buffer_replace", replace as *const u8),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::LIVE;

    fn allocate_i32(arena: &mut Arena, n: u64) -> Result<NonNull<Buffer>, Error> {
        Buffer::allocate(arena, n, 4, std::mem::align_of::<i32>())
    }
    unsafe fn push_i32(buffer: &mut Buffer, value: i32) {
        buffer.initialize_next().unwrap().cast::<i32>().write(value);
        buffer.commit_next();
    }

    #[test]
    fn element_swaps_validate_both_indices_before_mutation() {
        let mut arena = Arena::default();
        let mut owner = allocate_i32(&mut arena, 2).unwrap();
        unsafe {
            let buffer = owner.as_mut();
            push_i32(buffer, 20);
            push_i32(buffer, 22);
            assert!(buffer.swap_elements(0, u64::MAX).is_err());
            assert!(buffer.swap_elements(2, 0).is_err());
            assert_eq!(buffer.load(0).unwrap().cast::<i32>().read(), 20);
            assert_eq!(buffer.load(1).unwrap().cast::<i32>().read(), 22);
            buffer.swap_elements(0, 0).unwrap();
            buffer.swap_elements(0, 1).unwrap();
            assert_eq!(buffer.load(0).unwrap().cast::<i32>().read(), 22);
            assert_eq!(buffer.load(1).unwrap().cast::<i32>().read(), 20);
            assert_eq!(buffer.len(), 2);
            Buffer::release(&mut arena, owner.as_ptr()).unwrap();
        }
        LIVE.with(|count| assert_eq!(count.get(), 0));
    }

    #[test]
    fn zero_sized_copy_buffers_release_without_visiting_logical_elements() {
        let mut arena = Arena::default();
        let capacity = Buffer::capacity_limit(0);
        let mut owner = Buffer::allocate(&mut arena, capacity, 0, 1).unwrap();
        unsafe {
            owner.as_mut().len = capacity as usize;
            Buffer::release(&mut arena, owner.as_ptr()).unwrap();
        }
        LIVE.with(|count| assert_eq!(count.get(), 0));
    }

    #[test]
    fn buffer_control_block_is_stable_and_replaced_storage_is_reclaimed() {
        let mut arena = Arena::default();
        let mut first = allocate_i32(&mut arena, 0).unwrap();
        let second = allocate_i32(&mut arena, 0).unwrap();
        assert_ne!(first, second);
        LIVE.with(|n| assert_eq!(n.get(), 2)); // Empty buffers have no payload allocation.
        let mut next = allocate_i32(&mut arena, 2).unwrap();
        unsafe {
            push_i32(next.as_mut(), -42);
            let identity = first.as_ptr();
            Buffer::replace(&mut arena, first.as_ptr(), next.as_ptr()).unwrap();
            assert_eq!(first.as_ptr(), identity);
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), -42);
            assert_eq!(first.as_ref().capacity(), 2);
            assert!(!arena.contains(next.cast()));
            assert!(Buffer::replace(&mut arena, first.as_ptr(), first.as_ptr()).is_err());
            first.as_mut().truncate_owned(&mut arena, 0).unwrap();
            assert!(first.as_ref().load(0).is_err());
            push_i32(first.as_mut(), 42);
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), 42);
            let mut replacement = allocate_i32(&mut arena, 4).unwrap();
            push_i32(replacement.as_mut(), 7);
            Buffer::replace(&mut arena, first.as_ptr(), replacement.as_ptr()).unwrap();
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), 7);
        }
        LIVE.with(|n| assert_eq!(n.get(), 3));
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn buffer_release_validates_task_ownership_and_cleans_empty_and_aligned_storage() {
        let mut arena = Arena::default();
        let mut other = Arena::default();
        for (capacity, size, align) in [(0, 4, 4), (1024, 0, 8), (2, 17, 64)] {
            let owner = Buffer::allocate(&mut arena, capacity, size, align).unwrap();
            let live = LIVE.with(|n| n.get());
            unsafe {
                assert!(Buffer::release(&mut other, owner.as_ptr()).is_err());
                LIVE.with(|n| assert_eq!(n.get(), live));
                assert_eq!(owner.as_ref().capacity(), capacity);
                Buffer::release(&mut arena, owner.as_ptr()).unwrap();
                assert!(Buffer::release(&mut arena, owner.as_ptr()).is_err());
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        drop(arena);
        drop(other);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn buffer_capacity_limit_matches_checked_target_layout() {
        let limit = Buffer::capacity_limit(4);
        assert!(Layout::array::<i32>(usize::try_from(limit).unwrap()).is_ok());
        assert!(Layout::array::<i32>(usize::try_from(limit + 1).unwrap()).is_err());
        let mut arena = Arena::default();
        for count in [limit + 1, u64::MAX] {
            assert!(allocate_i32(&mut arena, count).is_err());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }

    #[test]
    fn buffer_layout_alignment_and_initialization_are_checked_before_publication() {
        let mut arena = Arena::default();
        for (size, align) in [(usize::MAX, 8), (8, 0), (8, 3)] {
            assert!(Buffer::allocate(&mut arena, 2, size, align).is_err());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        for (size, align) in [(1, 1), (3, 4), (8, 8), (17, 16)] {
            let mut owner = Buffer::allocate(&mut arena, 2, size, align).unwrap();
            unsafe {
                let buffer = owner.as_mut();
                for i in 0..2 {
                    let data = buffer.initialize_next().unwrap();
                    assert_eq!(data as usize % align, 0);
                    assert_eq!(buffer.len(), i);
                    assert!(buffer.load(i).is_err());
                    data.write(42 + i as u8);
                    buffer.commit_next();
                    assert_eq!(buffer.load(i).unwrap().read(), 42 + i as u8);
                }
                assert!(buffer.initialize_next().is_err());
                assert!(buffer.load(2).is_err());
                assert!(buffer.load(u64::MAX).is_err());
                assert!(buffer.truncate_owned(&mut arena, 3).is_err());
                assert_eq!(buffer.len(), 2);
            }
        }
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn buffer_zero_sized_capacity_is_bounded_without_payload_allocation() {
        let mut arena = Arena::default();
        let limit = Buffer::capacity_limit(0);
        assert!(limit < u64::MAX);
        for count in [limit + 1, u64::MAX] {
            assert!(Buffer::allocate(&mut arena, count, 0, 1).is_err());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        let mut owner = Buffer::allocate(&mut arena, limit, 0, 1).unwrap();
        LIVE.with(|n| assert_eq!(n.get(), 1));
        unsafe {
            let buffer = owner.as_mut();
            assert!(buffer.load(0).is_err());
            buffer.initialize_next().unwrap();
            buffer.commit_next();
            assert_eq!(buffer.len(), 1);
            assert!(buffer.load(0).is_ok());
            assert!(buffer.load(1).is_err());
            buffer.truncate_owned(&mut arena, 0).unwrap();
            assert!(buffer.load(0).is_err());
        }
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn buffer_replace_rejects_layout_mismatch_without_consuming_either_owner() {
        let mut arena = Arena::default();
        let mut small = allocate_i32(&mut arena, 1).unwrap();
        let wide = Buffer::allocate(&mut arena, 2, 8, 8).unwrap();
        unsafe {
            push_i32(small.as_mut(), 42);
            assert!(Buffer::replace(&mut arena, small.as_ptr(), wide.as_ptr()).is_err());
            assert_eq!(small.as_ref().len(), 1);
            assert_eq!(small.as_ref().load(0).unwrap().cast::<i32>().read(), 42);
            assert_eq!(wide.as_ref().capacity(), 2);
        }
    }
}
