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
#[derive(Default)]
pub(crate) struct Arena {
    allocations: Vec<(NonNull<u8>, Layout)>,
    bytes: usize,
    external: Rc<Cell<usize>>,
    budget: Option<Rc<Budget>>,
    account: Option<Rc<Budget>>,
    imported: usize,
    temporary_limit: Option<usize>,
}
/// Shared peak accounting for persistent roots and their active invocations.
#[derive(Default)]
pub(crate) struct Budget {
    pub bytes: Cell<usize>,
    pub limit: Cell<usize>,
}
impl Budget {
    pub(crate) fn charge(&self, amount: usize) -> Result<(), Error> {
        let bytes = self
            .bytes
            .get()
            .checked_add(amount)
            .filter(|&n| n <= self.limit.get())
            .ok_or_else(|| {
                Error::new_with_code(
                    ErrorPhase::Runtime,
                    ErrorCode::ResourceLimit,
                    "Instance byte budget exceeded",
                )
            })?;
        self.bytes.set(bytes);
        Ok(())
    }
    pub(crate) fn refund(&self, amount: usize) {
        self.bytes.set(self.bytes.get() - amount);
    }
}

impl Arena {
    pub(crate) fn with_budget(budget: Rc<Budget>, temporary_limit: Option<usize>) -> Self {
        Self {
            budget: Some(budget),
            account: Some(Rc::new(Budget {
                limit: Cell::new(usize::MAX),
                ..Default::default()
            })),
            temporary_limit,
            allocations: Vec::new(),
            bytes: 0,
            external: Default::default(),
            imported: 0,
        }
    }
    // Buffer metadata holds a nonowning pointer; the arena pins this budget,
    // and transfer only accepts another arena with the identical budget owner.
    pub(crate) fn account_pointer(&self) -> *const Budget {
        self.account.as_ref().map_or(std::ptr::null(), Rc::as_ptr)
    }
    pub(crate) fn budget_pointer(&self) -> *const Budget {
        self.budget.as_ref().map_or(std::ptr::null(), Rc::as_ptr)
    }
    pub(crate) fn bytes(&self) -> usize {
        self.bytes + self.external.get()
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
        if !matches!((&self.budget, &to.budget), (Some(a), Some(b)) if Rc::ptr_eq(a, b)) {
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
        self.allocations.iter().any(|&(p, _)| p == pointer)
    }

    /// Release a compiler-owned allocation and refund its live-storage charge.
    /// Ordinary arena references must never be passed here.
    ///
    /// # Safety
    /// No live capability may access this allocation after this call.
    pub(crate) unsafe fn release(&mut self, pointer: NonNull<u8>) {
        let index = self
            .allocations
            .iter()
            .position(|&(p, _)| p == pointer)
            .expect("validated task allocation");
        let (pointer, layout) = self.allocations.swap_remove(index);
        self.bytes -= layout.pad_to_align().size() + std::mem::size_of::<(NonNull<u8>, Layout)>();
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
        LIVE.with(|live| live.set(live.get() - 1));
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
    /// Charge host-owned bytes against the same peak as language allocations.
    /// The refund handle remains valid even if the interpreter moves its arena.
    pub(crate) fn reserve_external(&mut self, charge: usize) -> Result<ExternalCharge, Error> {
        let external = self
            .external
            .get()
            .checked_add(charge)
            .ok_or_else(allocation_error)?;
        let limit = self.allocation_limit()?;
        if self.bytes.checked_add(external).is_none_or(|n| n > limit) {
            return Err(allocation_error());
        }
        if let Some(account) = &self.account {
            account.charge(charge)?;
        }
        if let Some(budget) = &self.budget {
            if let Err(error) = budget.charge(charge) {
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        self.external.set(external);
        Ok(ExternalCharge {
            bytes: charge,
            external: self.external.clone(),
            budget: self.budget.clone(),
            account: self.account.clone(),
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
        self.allocations
            .try_reserve(1)
            .map_err(|_| allocation_error())?;
        if let Some(account) = &self.account {
            account.charge(charge)?;
        }
        if let Some(budget) = &self.budget {
            if let Err(error) = budget.charge(charge) {
                if let Some(account) = &self.account {
                    account.refund(charge);
                }
                return Err(error);
            }
        }
        let Some(pointer) = NonNull::new(unsafe { alloc(layout) }) else {
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
        #[cfg(test)]
        LIVE.with(|live| live.set(live.get() + 1));
        Ok(pointer)
    }
}
pub(crate) struct ExternalCharge {
    bytes: usize,
    external: Rc<Cell<usize>>,
    budget: Option<Rc<Budget>>,
    account: Option<Rc<Budget>>,
}
impl Drop for ExternalCharge {
    fn drop(&mut self) {
        self.external.set(self.external.get() - self.bytes);
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
            LIVE.with(|live| live.set(live.get() - 1));
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
pub(crate) fn symbols() -> [(&'static str, *const u8); 3] {
    [
        ("__joky_arena_new", new as *const u8),
        ("__joky_arena_alloc", allocate as *const u8),
        ("__joky_arena_drop", release as *const u8),
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
