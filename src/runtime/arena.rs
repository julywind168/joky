//! Task-owned, nonmoving allocations. Only generated, checked code sees pointers.
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::{
    alloc::{alloc, dealloc, Layout},
    ptr::NonNull,
};

/// Per-task charged budget: padded payloads plus allocation bookkeeping.
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Default)]
pub(crate) struct Arena {
    allocations: Vec<(NonNull<u8>, Layout)>,
    bytes: usize,
}
impl Arena {
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
        dealloc(pointer.as_ptr(), layout);
        #[cfg(test)]
        LIVE.with(|live| live.set(live.get() - 1));
    }

    pub(crate) fn allocate(&mut self, size: usize, align: usize) -> Result<NonNull<u8>, Error> {
        let size = size.max(1);
        let layout = Layout::from_size_align(size, align).map_err(|_| allocation_error())?;
        #[cfg(test)]
        let limit = LIMIT.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let limit = MAX_BYTES;
        let charge = layout
            .pad_to_align()
            .size()
            .checked_add(std::mem::size_of::<(NonNull<u8>, Layout)>())
            .ok_or_else(allocation_error)?;
        let bytes = self
            .bytes
            .checked_add(charge)
            .filter(|&n| n <= limit)
            .ok_or_else(allocation_error)?;
        self.allocations
            .try_reserve(1)
            .map_err(|_| allocation_error())?;
        let pointer = NonNull::new(unsafe { alloc(layout) }).ok_or_else(allocation_error)?;
        self.allocations.push((pointer, layout));
        self.bytes = bytes;
        #[cfg(test)]
        LIVE.with(|live| live.set(live.get() + 1));
        Ok(pointer)
    }
}
impl Drop for Arena {
    fn drop(&mut self) {
        for (pointer, layout) in self.allocations.drain(..) {
            unsafe {
                dealloc(pointer.as_ptr(), layout);
            }
            #[cfg(test)]
            LIVE.with(|live| live.set(live.get() - 1));
        }
    }
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
