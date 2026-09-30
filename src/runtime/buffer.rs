//! Private layout-driven Copy storage. Handles and payloads share a task arena.
use super::arena::Arena;
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::{alloc::Layout, ptr::NonNull};

/// Stable control block; no element pointers or layout inputs are source-visible.
/// Retired payloads remain charged until the task arena is released.
pub(crate) struct Buffer {
    data: NonNull<u8>,
    len: usize,
    capacity: usize,
    element: Layout,
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
            data: NonNull::new(element.align() as *mut u8).unwrap(),
            len: 0,
            capacity: 0,
            element,
        }
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
        if bytes != 0 {
            value.data = arena.allocate(payload.size(), payload.align())?;
        }
        value.capacity = capacity;
        unsafe {
            owner.as_ptr().write(value);
        }
        Ok(owner)
    }

    pub(crate) fn len(&self) -> u64 {
        self.len as u64
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
        let len = (end - start) as usize;
        let value = Self {
            data: NonNull::new(self.address(start as usize)).unwrap(),
            len,
            capacity: len,
            element: self.element,
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

    pub(crate) fn truncate(&mut self, new_len: u64) -> Result<(), Error> {
        self.len = usize::try_from(new_len)
            .ok()
            .filter(|&n| n <= self.len)
            .ok_or_else(|| bounds_error("Buffer truncate cannot increase initialized length"))?;
        Ok(())
    }

    /// Both handles are live, exclusively accessible, same-element owners.
    pub(crate) unsafe fn replace(destination: *mut Self, source: *mut Self) -> Result<(), Error> {
        if destination == source {
            return Err(bounds_error("Buffer replacement requires distinct owners"));
        }
        if (*destination).element != (*source).element {
            return Err(bounds_error(
                "Buffer replacement requires matching element layouts",
            ));
        }
        let empty = Self::empty((*source).element);
        destination.write(std::mem::replace(&mut *source, empty));
        Ok(())
    }
}

fn located<T: Default>(result: Result<T, Error>, source: u64, start: usize, end: usize) -> T {
    result.unwrap_or_else(|error| {
        super::error::record_error(
            error.with_span(Span::new(start, end).with_source(SourceId::from_raw(source))),
        );
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
    buffer: *mut Buffer,
    index: u64,
    source: u64,
    start: usize,
    end: usize,
) -> *mut u8 {
    located((*buffer).load(index), source, start, end)
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
    buffer: *mut Buffer,
    new_len: u64,
    source: u64,
    start: usize,
    end: usize,
) {
    located((*buffer).truncate(new_len), source, start, end);
}
#[export_name = "__joky_buffer_replace"]
unsafe extern "C" fn replace(
    destination: *mut Buffer,
    source_buffer: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        Buffer::replace(destination, source_buffer),
        source,
        start,
        end,
    );
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

pub(crate) fn symbols() -> [(&'static str, *const u8); 12] {
    [
        ("__joky_buffer_allocate", allocate as *const u8),
        ("__joky_buffer_slice", slice as *const u8),
        ("__joky_buffer_element_load", element_load as *const u8),
        ("__joky_buffer_len", len as *const u8),
        ("__joky_buffer_capacity", capacity as *const u8),
        ("__joky_buffer_capacity_limit", capacity_limit as *const u8),
        ("__joky_buffer_load", load as *const u8),
        ("__joky_buffer_store", store as *const u8),
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
    fn buffer_control_block_is_stable_and_retired_payloads_stay_charged() {
        let mut arena = Arena::default();
        let mut first = allocate_i32(&mut arena, 0).unwrap();
        let second = allocate_i32(&mut arena, 0).unwrap();
        assert_ne!(first, second);
        LIVE.with(|n| assert_eq!(n.get(), 2)); // Empty buffers have no payload allocation.
        let mut next = allocate_i32(&mut arena, 2).unwrap();
        unsafe {
            push_i32(next.as_mut(), -42);
            let identity = first.as_ptr();
            Buffer::replace(first.as_ptr(), next.as_ptr()).unwrap();
            assert_eq!(first.as_ptr(), identity);
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), -42);
            assert_eq!(first.as_ref().capacity(), 2);
            assert_eq!(next.as_ref().len(), 0);
            assert_eq!(next.as_ref().capacity(), 0);
            assert!(Buffer::replace(first.as_ptr(), first.as_ptr()).is_err());
            first.as_mut().truncate(0).unwrap();
            assert!(first.as_ref().load(0).is_err());
            push_i32(first.as_mut(), 42);
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), 42);
            let mut replacement = allocate_i32(&mut arena, 4).unwrap();
            push_i32(replacement.as_mut(), 7);
            Buffer::replace(first.as_ptr(), replacement.as_ptr()).unwrap();
            assert_eq!(first.as_ref().load(0).unwrap().cast::<i32>().read(), 7);
        }
        LIVE.with(|n| assert_eq!(n.get(), 6));
        drop(arena);
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
                assert!(buffer.truncate(3).is_err());
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
            buffer.truncate(0).unwrap();
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
            assert!(Buffer::replace(small.as_ptr(), wide.as_ptr()).is_err());
            assert_eq!(small.as_ref().len(), 1);
            assert_eq!(small.as_ref().load(0).unwrap().cast::<i32>().read(), 42);
            assert_eq!(wide.as_ref().capacity(), 2);
        }
    }
}
