//! Task-owned, nonmoving allocations. Only generated, checked code sees pointers.
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::{
    alloc::{alloc, dealloc, Layout},
    cell::Cell,
    ptr::NonNull,
    rc::Rc,
};

/// Per-task charged budget: padded payloads plus allocation bookkeeping.
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;
pub(crate) struct Arena {
    #[cfg(test)]
    tracked: bool,
    parallel: Option<(
        super::parallel::Storage,
        Option<super::parallel::LocalShard>,
    )>,
    allocations: Vec<(NonNull<u8>, Layout)>,
    bytes: usize,
    external: Rc<Cell<usize>>,
    // Explicit regions share the task ceiling even without a persistent budget.
    task_bytes: Rc<Cell<usize>>,
    budget: Option<std::sync::Arc<Budget>>,
    account: Option<std::sync::Arc<Budget>>,
    // Region payload also counts against the invocation's metadata-aware limit.
    parent_account: Option<std::sync::Arc<Budget>>,
    imported: usize,
    temporary_limit: Option<usize>,
}
impl Default for Arena {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            tracked: true,
            parallel: super::parallel::current().map(|root| (root, None)),
            allocations: Vec::new(),
            bytes: 0,
            external: Default::default(),
            task_bytes: Default::default(),
            budget: None,
            account: None,
            parent_account: None,
            imported: 0,
            temporary_limit: None,
        }
    }
}
/// Shared peak accounting for persistent roots and their active invocations.
#[derive(Default)]
pub(crate) struct Budget {
    pub bytes: Counter,
    pub limit: Counter,
    pub shared: Option<super::parallel::SharedLease>,
}
/// Atomic accounting shared by independently owned domain arenas.
#[derive(Default)]
pub(crate) struct Counter(std::sync::atomic::AtomicUsize);
impl From<usize> for Counter {
    fn from(value: usize) -> Self {
        Self(std::sync::atomic::AtomicUsize::new(value))
    }
}
impl Counter {
    #[cfg(test)]
    pub(crate) fn replace(&self, value: usize) -> usize {
        self.0.swap(value, std::sync::atomic::Ordering::AcqRel)
    }
    pub(crate) fn get(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn set(&self, value: usize) {
        self.0.store(value, std::sync::atomic::Ordering::Release);
    }
}
impl Budget {
    pub(crate) fn charge(&self, amount: usize) -> Result<(), Error> {
        self.bytes
            .0
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |bytes| bytes.checked_add(amount).filter(|n| *n <= self.limit.get()),
            )
            .map_err(|_| {
                Error::new_with_code(
                    ErrorPhase::Runtime,
                    ErrorCode::ResourceLimit,
                    "Instance byte budget exceeded",
                )
            })?;
        if let Some(shared) = &self.shared {
            if let Err(error) = shared.add(amount) {
                self.bytes
                    .0
                    .fetch_sub(amount, std::sync::atomic::Ordering::AcqRel);
                return Err(error);
            }
        }
        Ok(())
    }
    pub(crate) fn refund(&self, amount: usize) {
        if let Some(shared) = &self.shared {
            shared.refund(amount);
        }
        let previous = self
            .bytes
            .0
            .fetch_sub(amount, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous >= amount);
    }
}

impl Arena {
    #[cfg(test)]
    pub(crate) fn untrack(&mut self) {
        if self.tracked {
            LIVE.with(|live| live.set(live.get() - self.allocations.len()));
            self.tracked = false;
        }
    }

    pub(crate) fn with_storage(root: super::parallel::Storage) -> Self {
        let mut arena = Self::default();
        arena.parallel = Some((root, None));
        arena
    }
    pub(crate) fn parallel_root(&self) -> Option<&super::parallel::Storage> {
        self.parallel.as_ref().map(|(root, _)| root)
    }
    pub(crate) fn with_budget(
        budget: std::sync::Arc<Budget>,
        temporary_limit: Option<usize>,
    ) -> Self {
        Self {
            #[cfg(test)]
            tracked: true,
            parallel: None,
            budget: Some(budget),
            account: Some(std::sync::Arc::new(Budget {
                limit: (usize::MAX).into(),
                ..Default::default()
            })),
            parent_account: None,
            temporary_limit,
            allocations: Vec::new(),
            bytes: 0,
            external: Default::default(),
            task_bytes: Default::default(),
            imported: 0,
        }
    }
    /// A separately reclaimed region shares its task's live allocation ceiling.
    pub(crate) fn child(&self) -> Self {
        Self {
            #[cfg(test)]
            tracked: true,
            parallel: self.parallel.as_ref().map(|(root, _)| (root.clone(), None)),
            task_bytes: self.task_bytes.clone(),
            budget: self.budget.clone(),
            account: self.budget.as_ref().map(|_| {
                std::sync::Arc::new(Budget {
                    limit: (usize::MAX).into(),
                    ..Default::default()
                })
            }),
            parent_account: self.parent_account.clone().or_else(|| self.account.clone()),
            imported: self.imported,
            temporary_limit: self.temporary_limit,
            allocations: Vec::new(),
            bytes: 0,
            external: Default::default(),
        }
    }
    /// Release a nonescaping explicit region's parallel allocations.
    /// Local allocations are reclaimed by the ordinary Arena destructor.
    ///
    /// # Safety
    /// This must be a private child region, with no live references or exported
    /// owners. The region must not be used again after this call.
    pub(crate) unsafe fn release_region(&mut self) {
        if let Some((_, Some(shard))) = &mut self.parallel {
            shard.release_region();
        }
    }
    // Buffer metadata holds a nonowning pointer; the arena pins this budget,
    // and transfer only accepts another arena with the identical budget owner.
    pub(crate) fn account_pointer(&self) -> *const Budget {
        self.account
            .as_ref()
            .map_or(std::ptr::null(), std::sync::Arc::as_ptr)
    }
    pub(crate) fn budget_pointer(&self) -> *const Budget {
        self.budget
            .as_ref()
            .map_or(std::ptr::null(), std::sync::Arc::as_ptr)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.parallel.as_ref().map_or(self.bytes, |(_, shard)| {
            shard.as_ref().map_or(0, super::parallel::LocalShard::bytes)
        }) + self.external.get()
    }
    pub(crate) fn pointers_except(&self, excluded: NonNull<u8>) -> Result<Vec<NonNull<u8>>, Error> {
        let mut pointers = Vec::new();
        pointers
            .try_reserve(self.allocations.len())
            .map_err(|_| allocation_error())?;
        pointers.extend(
            self.allocations
                .iter()
                .filter_map(|&(p, _)| (p != excluded).then_some(p)),
        );
        Ok(pointers)
    }
    pub(crate) fn selected_bytes(&self, pointers: &[NonNull<u8>]) -> Result<usize, Error> {
        pointers.iter().try_fold(0usize, |bytes, p| {
            let (_, layout) = self
                .allocations
                .iter()
                .find(|(q, _)| q == p)
                .ok_or_else(allocation_error)?;
            bytes
                .checked_add(
                    layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>(),
                )
                .ok_or_else(allocation_error)
        })
    }
    /// Preflight and reserve before changing the unique arena registration. The
    /// commit cannot allocate, run destructors or change shared peak accounting.
    pub(crate) fn transfer(
        &mut self,
        to: &mut Self,
        pointers: &[NonNull<u8>],
        limit: usize,
    ) -> Result<(), Error> {
        transfer_checkpoint()?;
        if !matches!((&self.budget, &to.budget), (Some(a), Some(b)) if std::sync::Arc::ptr_eq(a, b))
        {
            return Err(allocation_error());
        }
        for (index, p) in pointers.iter().enumerate() {
            if pointers[..index].contains(p) || to.contains(*p) {
                return Err(allocation_error());
            }
        }
        let charge = self.selected_bytes(pointers)?;
        let _bytes = to
            .bytes
            .checked_add(charge)
            .filter(|&n| n <= limit)
            .ok_or_else(allocation_error)?;
        transfer_checkpoint()?;
        to.allocations
            .try_reserve(pointers.len())
            .map_err(|_| allocation_error())?;
        transfer_checkpoint()?;
        self.transfer_reserved(to, pointers);
        Ok(())
    }
    /// Reserve a destination for a subsequent allocation-free transfer.
    pub(crate) fn reserve_transfer(&mut self, count: usize) -> Result<(), Error> {
        transfer_checkpoint()?;
        self.allocations
            .try_reserve(count)
            .map_err(|_| allocation_error())
    }
    /// Move validated registrations without failure, including rollback after
    /// partial import. Source and destination must share a budget; destination
    /// capacity must already suffice and pointers must be distinct source entries.
    pub(crate) fn transfer_reserved(&mut self, to: &mut Self, pointers: &[NonNull<u8>]) {
        debug_assert_eq!(self.budget_pointer(), to.budget_pointer());
        debug_assert!(to.allocations.capacity() - to.allocations.len() >= pointers.len());
        #[cfg(test)]
        if self.tracked != to.tracked {
            LIVE.with(|live| {
                live.set(if to.tracked {
                    live.get() + pointers.len()
                } else {
                    live.get() - pointers.len()
                })
            });
        }
        let mut charge = 0;
        for p in pointers {
            let index = self.allocations.iter().position(|(q, _)| q == p).unwrap();
            let allocation = self.allocations.swap_remove(index);
            charge +=
                allocation.1.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>();
            to.allocations.push(allocation);
        }
        if let (Some(from), Some(target)) = (&self.account, &to.account) {
            from.refund(charge);
            target.bytes.set(target.bytes.get() + charge);
        }
        if let Some(account) = &self.parent_account {
            account.refund(charge);
        }
        if let Some(account) = &to.parent_account {
            account.bytes.set(account.bytes.get() + charge);
        }
        if !Rc::ptr_eq(&self.task_bytes, &to.task_bytes) {
            self.task_bytes.set(self.task_bytes.get() - charge);
            to.task_bytes.set(to.task_bytes.get() + charge);
        }
        self.bytes -= charge;
        to.bytes += charge;
    }
    /// Discard dead invocation allocations without allocating during cleanup.
    ///
    /// # Safety
    /// `live` must contain every reachable allocation. All owners and external
    /// descriptors outside that set must already have completed their cleanup.
    pub(crate) unsafe fn retain_allocations(&mut self, live: &[NonNull<u8>]) {
        let mut index = 0;
        while index < self.allocations.len() {
            let pointer = self.allocations[index].0;
            if live.contains(&pointer) {
                index += 1;
            } else {
                self.release(pointer);
            }
        }
    }

    pub(crate) fn credit_imported(&mut self) {
        self.imported = self.bytes;
        if let Some(account) = &self.account {
            account.limit.set(
                account
                    .bytes
                    .get()
                    .saturating_add(self.temporary_limit.unwrap_or(MAX_BYTES)),
            );
        }
    }
    pub(crate) fn contains(&self, pointer: NonNull<u8>) -> bool {
        self.parallel.as_ref().map_or_else(
            || self.allocations.iter().any(|&(p, _)| p == pointer),
            |(root, _)| root.contains(pointer),
        )
    }

    /// Release a compiler-owned allocation and refund its live-storage charge.
    /// Ordinary arena references must never be passed here.
    ///
    /// # Safety
    /// No live capability may access this allocation after this call.
    pub(crate) unsafe fn release(&mut self, pointer: NonNull<u8>) {
        if let Some((root, _)) = &self.parallel {
            root.release(pointer);
            return;
        }
        let index = self
            .allocations
            .iter()
            .position(|&(p, _)| p == pointer)
            .expect("validated task allocation");
        let (pointer, layout) = self.allocations.swap_remove(index);
        let charge = layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>();
        self.bytes -= charge;
        self.task_bytes.set(self.task_bytes.get() - charge);
        if let Some(account) = &self.parent_account {
            account.refund(charge);
        }
        if let Some(budget) = &self.budget {
            budget.refund(
                layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>(),
            );
        }
        if let Some(account) = &self.account {
            account.refund(
                layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>(),
            );
        }
        dealloc(pointer.as_ptr(), layout);
        #[cfg(test)]
        if self.tracked {
            LIVE.with(|live| live.set(live.get() - 1));
        }
    }

    fn allocation_limit(&self) -> Result<usize, Error> {
        #[cfg(test)]
        let limit = LIMIT.with(Cell::get);
        #[cfg(not(test))]
        let limit = MAX_BYTES;
        limit
            .min(self.temporary_limit.unwrap_or(MAX_BYTES))
            .checked_add(self.imported)
            .ok_or_else(allocation_error)
    }
    pub(crate) fn allocate(&mut self, size: usize, align: usize) -> Result<NonNull<u8>, Error> {
        self.allocate_with_limit(size, align, self.allocation_limit()?)
    }

    /// Initialize a runtime value before registering its parallel finalizer.
    /// Local arenas retain their ordinary explicit-cleanup contract.
    /// # Safety
    /// `finalize` must accept `value` immediately after initialization and may
    /// destroy only thread-safe runtime metadata on any worker, not language
    /// values. Local callers must explicitly clean up any owned fields.
    pub(crate) unsafe fn allocate_initialized<T>(
        &mut self,
        value: T,
        finalize: unsafe fn(*mut u8),
    ) -> Result<NonNull<T>, Error> {
        let limit = self.allocation_limit()?;
        let layout = Layout::new::<T>();
        if let Some((root, shard)) = &mut self.parallel {
            if layout.size() > limit {
                return Err(allocation_error());
            }
            if shard.is_none() {
                *shard = Some(root.shard()?);
            }
            return shard
                .as_mut()
                .unwrap()
                .allocate_initialized(value, finalize);
        }
        let pointer = self
            .allocate_with_limit(layout.size(), layout.align(), limit)?
            .cast::<T>();
        pointer.as_ptr().write(value);
        Ok(pointer)
    }
    /// Charge host-owned bytes against the same peak as language allocations.
    /// The refund handle remains valid even if the interpreter moves its arena.
    pub(crate) fn metadata_lease(&self) -> Option<super::parallel::Lease> {
        self.parallel
            .as_ref()
            .map(|(root, _)| root.accounts().reserve(0).expect("zero reservation"))
    }
    pub(crate) fn reserve_external(&mut self, charge: usize) -> Result<ExternalCharge, Error> {
        let shared = self
            .parallel
            .as_ref()
            .map(|(root, _)| root.accounts().reserve(charge))
            .transpose()?;
        let external = self
            .external
            .get()
            .checked_add(charge)
            .ok_or_else(allocation_error)?;
        let limit = self.allocation_limit()?;
        let task_bytes = self
            .task_bytes
            .get()
            .checked_add(charge)
            .filter(|&n| n <= limit)
            .ok_or_else(allocation_error)?;
        if self.bytes.checked_add(external).is_none_or(|n| n > limit) {
            return Err(allocation_error());
        }
        if let Some(account) = &self.account {
            account.charge(charge)?;
        }
        if let Some(parent) = &self.parent_account {
            if let Err(error) = parent.charge(charge) {
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        if let Some(budget) = &self.budget {
            if let Err(error) = budget.charge(charge) {
                if let Some(parent) = &self.parent_account {
                    parent.refund(charge);
                }
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        self.external.set(external);
        self.task_bytes.set(task_bytes);
        Ok(ExternalCharge {
            _shared: shared,
            bytes: charge,
            external: self.external.clone(),
            task_bytes: self.task_bytes.clone(),
            budget: self.budget.clone(),
            account: self.account.clone(),
            parent_account: self.parent_account.clone(),
        })
    }

    /// Activation-local storage is the interpreter equivalent of native stack
    /// slots. Task allocation fault injection must not disable stack borrowing.
    /// IR separately bounds each function's local layout to 64 KiB.
    pub(crate) fn allocate_local(
        &mut self,
        size: usize,
        align: usize,
    ) -> Result<NonNull<u8>, Error> {
        #[cfg(test)]
        let limit = LOCAL_LIMIT.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let limit = MAX_BYTES;
        self.allocate_with_limit(size, align, limit)
    }

    fn allocate_with_limit(
        &mut self,
        size: usize,
        align: usize,
        limit: usize,
    ) -> Result<NonNull<u8>, Error> {
        if let Some((root, shard)) = &mut self.parallel {
            if size > limit {
                return Err(allocation_error());
            }
            if shard.is_none() {
                *shard = Some(root.shard()?);
            }
            return shard.as_mut().unwrap().allocate(size, align);
        }
        let layout = Layout::from_size_align(size.max(1), align).map_err(|_| allocation_error())?;
        let charge = layout
            .pad_to_align()
            .size()
            .checked_add(std::mem::size_of::<(NonNull<u8>, Layout)>())
            .ok_or_else(allocation_error)?;
        let bytes = self
            .bytes
            .checked_add(charge)
            .filter(|&n| {
                n.checked_add(self.external.get())
                    .is_some_and(|total| total <= limit)
            })
            .ok_or_else(allocation_error)?;
        let task_bytes = self
            .task_bytes
            .get()
            .checked_add(charge)
            .filter(|&n| n <= limit)
            .ok_or_else(allocation_error)?;
        self.allocations
            .try_reserve(1)
            .map_err(|_| allocation_error())?;
        if let Some(account) = &self.account {
            account.charge(charge)?;
        }
        if let Some(parent) = &self.parent_account {
            if let Err(error) = parent.charge(charge) {
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        if let Some(budget) = &self.budget {
            if let Err(error) = budget.charge(charge) {
                if let Some(parent) = &self.parent_account {
                    parent.refund(charge);
                }
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        let Some(pointer) = NonNull::new(unsafe { alloc(layout) }) else {
            if let Some(parent) = &self.parent_account {
                parent.refund(charge);
            }
            if let Some(budget) = &self.budget {
                budget.refund(charge);
            }
            if let Some(account) = &self.account {
                account.refund(charge);
            }
            return Err(allocation_error());
        };
        self.allocations.push((pointer, layout));
        self.bytes = bytes;
        self.task_bytes.set(task_bytes);
        #[cfg(test)]
        if self.tracked {
            LIVE.with(|live| live.set(live.get() + 1));
        }
        Ok(pointer)
    }
}
pub(crate) struct ExternalCharge {
    _shared: Option<super::parallel::Lease>,
    bytes: usize,
    external: Rc<Cell<usize>>,
    task_bytes: Rc<Cell<usize>>,
    budget: Option<std::sync::Arc<Budget>>,
    account: Option<std::sync::Arc<Budget>>,
    parent_account: Option<std::sync::Arc<Budget>>,
}
impl Drop for ExternalCharge {
    fn drop(&mut self) {
        self.external.set(self.external.get() - self.bytes);
        self.task_bytes.set(self.task_bytes.get() - self.bytes);
        if let Some(parent) = &self.parent_account {
            parent.refund(self.bytes);
        }
        if let Some(budget) = &self.budget {
            budget.refund(self.bytes);
        }
        if let Some(account) = &self.account {
            account.refund(self.bytes);
        }
    }
}
impl Drop for Arena {
    fn drop(&mut self) {
        self.task_bytes.set(self.task_bytes.get() - self.bytes);
        if let Some(parent) = &self.parent_account {
            parent.refund(self.bytes);
        }
        if let Some(budget) = &self.budget {
            budget.refund(self.bytes);
        }
        if let Some(account) = &self.account {
            debug_assert_eq!(
                account.bytes.get(),
                self.bytes,
                "Unreleased owning descriptors"
            );
            account.refund(self.bytes);
        }
        for (pointer, layout) in self.allocations.drain(..) {
            unsafe {
                dealloc(pointer.as_ptr(), layout);
            }
            #[cfg(test)]
            if self.tracked {
                LIVE.with(|live| live.set(live.get() - 1));
            }
        }
    }
}
#[cfg(test)]
thread_local! {
    pub(crate) static TRANSFER_FAIL: Cell<Option<usize>> = const { Cell::new(None) };
}
fn transfer_checkpoint() -> Result<(), Error> {
    #[cfg(test)]
    if TRANSFER_FAIL.with(|fault| match fault.get() {
        Some(0) => {
            fault.set(None);
            true
        }
        Some(n) => {
            fault.set(Some(n - 1));
            false
        }
        None => false,
    }) {
        return Err(allocation_error());
    }
    Ok(())
}
fn allocation_error() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Task arena allocation failed or exceeded its 64 MiB budget",
    )
}

#[export_name = "__joky_arena_new"]
extern "C" fn new() -> *mut Arena {
    let pointer = unsafe { alloc(Layout::new::<Arena>()) }.cast::<Arena>();
    if pointer.is_null() {
        super::error::record_error(allocation_error());
    } else {
        unsafe {
            pointer.write(Arena::default());
        }
    }
    pointer
}
#[export_name = "__joky_arena_alloc"]
unsafe extern "C" fn allocate(
    arena: *mut Arena,
    size: usize,
    align: usize,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    match (*arena).allocate(size, align) {
        Ok(pointer) => pointer.as_ptr(),
        Err(error) => {
            super::error::record_error(
                error.with_span(
                    Span::new(start, end).with_source(crate::SourceId::from_raw(source)),
                ),
            );
            std::ptr::null_mut()
        }
    }
}
#[export_name = "__joky_arena_drop"]
unsafe extern "C" fn release(arena: *mut Arena) {
    if !arena.is_null() {
        std::ptr::drop_in_place(arena);
        dealloc(arena.cast(), Layout::new::<Arena>());
    }
}
pub(crate) fn symbols() -> [(&'static str, *const u8); 5] {
    [
        ("__joky_arena_new", new as *const u8),
        ("__joky_arena_alloc", allocate as *const u8),
        ("__joky_arena_drop", release as *const u8),
        (
            "__joky_explicit_arena_new",
            super::explicit_arena::new_native as *const u8,
        ),
        (
            "__joky_explicit_arena_alloc",
            super::explicit_arena::allocate_native as *const u8,
        ),
    ]
}
#[cfg(test)]
thread_local! { pub(crate) static LIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
thread_local! { pub(crate) static LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_BYTES) }; }

#[cfg(test)]
thread_local! { pub(crate) static LOCAL_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_BYTES) }; }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_transfer_preserves_task_counters_and_imported_credit() {
        let budget = std::sync::Arc::new(Budget {
            bytes: (0).into(),
            limit: (16384).into(),
            ..Default::default()
        });
        let mut storage = Arena::with_budget(budget.clone(), None);
        let mut invocation = Arena::with_budget(budget.clone(), Some(1024));
        let pointer = storage.allocate(256, 8).unwrap();
        unsafe { pointer.cast::<u64>().as_ptr().write(42) };
        let external = storage.reserve_external(128).unwrap();
        let original = budget.bytes.get();
        for checkpoint in 0..3 {
            TRANSFER_FAIL.with(|fault| fault.set(Some(checkpoint)));
            assert!(storage
                .transfer(&mut invocation, &[pointer], usize::MAX)
                .is_err());
            assert!(storage.contains(pointer));
            assert!(!invocation.contains(pointer));
            assert_eq!(storage.task_bytes.get(), original);
            assert_eq!(invocation.task_bytes.get(), 0);
            assert_eq!(budget.bytes.get(), original);
        }
        storage
            .transfer(&mut invocation, &[pointer], usize::MAX)
            .unwrap();
        assert_eq!(budget.bytes.get(), original);
        assert_eq!(storage.task_bytes.get(), 128);
        assert_eq!(invocation.task_bytes.get(), invocation.bytes);
        invocation.credit_imported();
        let payload = 1024 - std::mem::size_of::<(NonNull<u8>, Layout)>();
        invocation.allocate(payload, 1).unwrap();
        assert!(invocation.allocate(1, 1).is_err());
        let transferred = budget.bytes.get();
        storage.reserve_transfer(1).unwrap();
        invocation.transfer_reserved(&mut storage, &[pointer]);
        assert_eq!(budget.bytes.get(), transferred);
        assert_eq!(storage.task_bytes.get(), storage.bytes + 128);
        assert_eq!(invocation.task_bytes.get(), invocation.bytes);
        assert_eq!(unsafe { pointer.cast::<u64>().as_ptr().read() }, 42);
        drop(invocation);
        assert_eq!(budget.bytes.get(), original);
        drop(external);
        drop(storage);
        assert_eq!(budget.bytes.get(), 0);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn arena_region_transfers_keep_shared_counters_and_accounts_balanced() {
        let task = super::super::parallel::TaskBudget::new(1 << 20);
        let root = super::super::parallel::Storage::new(vec![task.clone()]).unwrap();
        let baseline = task.used();
        let budget = std::sync::Arc::new(Budget {
            bytes: (0).into(),
            limit: (16384).into(),
            shared: Some(root.accounts().shared_lease()),
        });
        let mut parent = Arena::with_budget(budget.clone(), Some(1024));
        parent.credit_imported();
        let mut child = parent.child();
        let first = parent.allocate(64, 8).unwrap();
        let second = child.allocate(64, 8).unwrap();
        let charged = budget.bytes.get();
        let account = parent.account.as_ref().unwrap().clone();
        parent.transfer(&mut child, &[first], usize::MAX).unwrap();
        assert_eq!(account.bytes.get(), charged);
        assert_eq!(parent.task_bytes.get(), charged);
        child.transfer(&mut parent, &[second], usize::MAX).unwrap();
        assert_eq!(account.bytes.get(), charged);
        assert_eq!(child.task_bytes.get(), charged);
        assert_eq!(budget.bytes.get(), charged);
        assert_eq!(task.used(), baseline + charged);
        drop(child);
        assert_eq!(account.bytes.get(), parent.bytes);
        assert_eq!(parent.task_bytes.get(), parent.bytes);
        drop(parent);
        assert_eq!(account.bytes.get(), 0);
        assert_eq!(budget.bytes.get(), 0);
        assert_eq!(task.used(), baseline);
        drop((budget, root));
        assert_eq!(task.used(), 0);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn arena_shared_budget_rejection_rolls_back_only_the_new_charge() {
        let task = super::super::parallel::TaskBudget::new(1 << 20);
        let root = super::super::parallel::Storage::new(vec![task.clone()]).unwrap();
        let baseline = task.used();
        let held = root.accounts().reserve((1 << 20) - baseline - 5).unwrap();
        let budget = Budget {
            bytes: (0).into(),
            limit: (10).into(),
            shared: Some(root.accounts().shared_lease()),
        };
        budget.charge(5).unwrap();
        // Local admission succeeds, but the shared parent refuses the addition.
        assert_eq!(
            budget.charge(1).unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        assert_eq!(budget.bytes.get(), 5);
        assert_eq!(task.used(), 1 << 20);
        drop(held);
        // Local rejection must not charge the now-available parent either.
        assert_eq!(
            budget.charge(6).unwrap_err().code(),
            ErrorCode::ResourceLimit
        );
        assert_eq!(budget.bytes.get(), 5);
        assert_eq!(task.used(), baseline + 5);
        budget.refund(5);
        assert_eq!(budget.bytes.get(), 0);
        drop((budget, root));
        assert_eq!(task.used(), 0);
    }

    #[test]
    fn released_allocations_refund_budget_without_moving_remaining_objects() {
        let mut arena = Arena::default();
        let first = arena.allocate(8, 8).unwrap();
        let middle = arena.allocate(1024, 64).unwrap();
        let last = arena.allocate(8, 8).unwrap();
        unsafe {
            first.cast::<u64>().as_ptr().write(20);
            last.cast::<u64>().as_ptr().write(22);
        }
        let charged = arena.bytes;
        unsafe {
            arena.release(middle);
        }
        assert_eq!(
            charged - arena.bytes,
            1024 + std::mem::size_of::<(NonNull<u8>, Layout)>()
        );
        assert!(!arena.contains(middle));
        let next = arena.allocate(1024, 64).unwrap();
        assert_eq!(arena.bytes, charged);
        assert_eq!(
            unsafe { first.cast::<u64>().as_ptr().read() + last.cast::<u64>().as_ptr().read() },
            42
        );
        unsafe {
            arena.release(first);
            arena.release(last);
            arena.release(next);
        }
        assert_eq!(arena.bytes, 0);
        assert!(arena.allocations.is_empty());
        LIVE.with(|n| assert_eq!(n.get(), 0));
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn arena_stable_storage_alignment_zero_sizes_and_failure() {
        let mut arena = Arena::default();
        let first = arena.allocate(8, 8).unwrap();
        unsafe {
            first.as_ptr().cast::<u64>().write(42);
        }
        for _ in 0..1000 {
            let p = arena.allocate(64, 64).unwrap();
            assert_eq!(p.as_ptr() as usize % 64, 0);
        }
        assert_eq!(unsafe { first.as_ptr().cast::<u64>().read() }, 42);
        let a = arena.allocate(0, 1).unwrap();
        let b = arena.allocate(0, 1).unwrap();
        assert_ne!(a, b);
        assert!(arena.allocate(usize::MAX, 8).is_err());
        assert!(arena.allocate(1, 3).is_err());
        assert!(arena.allocate(MAX_BYTES, 8).is_err());
        assert_eq!(unsafe { first.as_ptr().cast::<u64>().read() }, 42);
        drop(arena);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
