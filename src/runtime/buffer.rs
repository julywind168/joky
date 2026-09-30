//! Private Int32 storage. Handles and payloads belong to the same task arena.
use super::arena::Arena;
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::{alloc::Layout, ptr::NonNull};

/// Moving a language owner never moves this control block. No user-visible
/// layout or element addresses are exposed, and retired payloads stay charged.
pub(crate) struct Buffer {
    data: NonNull<i32>,
    len: usize,
    capacity: usize,
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
    fn empty() -> Self {
        Self {
            data: NonNull::dangling(),
            len: 0,
            capacity: 0,
        }
    }

    pub(crate) fn capacity_limit() -> u64 {
        (isize::MAX as usize / std::mem::size_of::<i32>()) as u64
    }

    pub(crate) fn allocate(arena: &mut Arena, capacity: u64) -> Result<NonNull<Self>, Error> {
        let capacity = usize::try_from(capacity).map_err(|_| capacity_error())?;
        let payload = Layout::array::<i32>(capacity).map_err(|_| capacity_error())?;
        let layout = Layout::new::<Self>();
        let owner = arena
            .allocate(layout.size(), layout.align())?
            .cast::<Self>();
        // Even on payload failure the control block remains charged and is
        // released with the arena. Publish no handle until initialization ends.
        let data = if capacity == 0 {
            NonNull::dangling()
        } else {
            arena
                .allocate(payload.size(), payload.align())?
                .cast::<i32>()
        };
        unsafe {
            owner.as_ptr().write(Self {
                data,
                len: 0,
                capacity,
            });
        }
        Ok(owner)
    }

    pub(crate) fn len(&self) -> u64 {
        self.len as u64
    }
    pub(crate) fn capacity(&self) -> u64 {
        self.capacity as u64
    }

    fn index(&self, index: u64) -> Result<usize, Error> {
        usize::try_from(index)
            .ok()
            .filter(|&i| i < self.len)
            .ok_or_else(|| bounds_error("Buffer index is outside the initialized prefix"))
    }

    pub(crate) fn load(&self, index: u64) -> Result<i32, Error> {
        let index = self.index(index)?;
        // Layout::array proved the complete allocation fits isize; index < len
        // proves both the address offset and initialization before this read.
        Ok(unsafe { self.data.as_ptr().add(index).read() })
    }

    pub(crate) fn store(&mut self, index: u64, value: i32) -> Result<(), Error> {
        let index = self.index(index)?;
        unsafe {
            self.data.as_ptr().add(index).write(value);
        }
        Ok(())
    }

    pub(crate) fn initialize_next(&mut self, value: i32) -> Result<(), Error> {
        if self.len == self.capacity {
            return Err(bounds_error("Buffer initialization exceeds capacity"));
        }
        unsafe {
            self.data.as_ptr().add(self.len).write(value);
        }
        self.len += 1;
        Ok(())
    }

    pub(crate) fn truncate(&mut self, new_len: u64) -> Result<(), Error> {
        let new_len = usize::try_from(new_len)
            .ok()
            .filter(|&n| n <= self.len)
            .ok_or_else(|| bounds_error("Buffer truncate cannot increase initialized length"))?;
        self.len = new_len;
        Ok(())
    }

    /// Both pointers must identify live control blocks in the current arena,
    /// with exclusive access. The shared IR also consumes the source owner.
    pub(crate) unsafe fn replace(destination: *mut Self, source: *mut Self) -> Result<(), Error> {
        if destination == source {
            return Err(bounds_error("Buffer replacement requires distinct owners"));
        }
        let value = std::mem::replace(&mut *source, Self::empty());
        destination.write(value);
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
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        Buffer::allocate(&mut *arena, capacity).map(NonNull::as_ptr),
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
extern "C" fn capacity_limit() -> u64 {
    Buffer::capacity_limit()
}
#[export_name = "__joky_buffer_load"]
unsafe extern "C" fn load(
    buffer: *const Buffer,
    index: u64,
    source: u64,
    start: usize,
    end: usize,
) -> i32 {
    located((*buffer).load(index), source, start, end)
}
#[export_name = "__joky_buffer_store"]
unsafe extern "C" fn store(
    buffer: *mut Buffer,
    index: u64,
    value: i32,
    source: u64,
    start: usize,
    end: usize,
) {
    located((*buffer).store(index, value), source, start, end);
}
#[export_name = "__joky_buffer_initialize_next"]
unsafe extern "C" fn initialize_next(
    buffer: *mut Buffer,
    value: i32,
    source: u64,
    start: usize,
    end: usize,
) {
    located((*buffer).initialize_next(value), source, start, end);
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
    buffer: *mut Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(Buffer::replace(destination, buffer), source, start, end);
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 9] {
    [
        ("__joky_buffer_allocate", allocate as *const u8),
        ("__joky_buffer_len", len as *const u8),
        ("__joky_buffer_capacity", capacity as *const u8),
        ("__joky_buffer_capacity_limit", capacity_limit as *const u8),
        ("__joky_buffer_load", load as *const u8),
        ("__joky_buffer_store", store as *const u8),
        (
            "__joky_buffer_initialize_next",
            initialize_next as *const u8,
        ),
        ("__joky_buffer_truncate", truncate as *const u8),
        ("__joky_buffer_replace", replace as *const u8),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::LIVE;

    #[test]
    fn buffer_control_block_is_stable_and_retired_payloads_stay_charged() {
        let mut arena = Arena::default();
        let mut first = Buffer::allocate(&mut arena, 0).unwrap();
        let second = Buffer::allocate(&mut arena, 0).unwrap();
        assert_ne!(first, second);
        LIVE.with(|n| assert_eq!(n.get(), 2)); // Empty buffers have no payload allocation.
        let mut next = Buffer::allocate(&mut arena, 2).unwrap();
        unsafe {
            next.as_mut().initialize_next(-42).unwrap();
            let identity = first.as_ptr();
            Buffer::replace(first.as_ptr(), next.as_ptr()).unwrap();
            assert_eq!(first.as_ptr(), identity);
            assert_eq!(first.as_ref().load(0).unwrap(), -42);
            assert_eq!(first.as_ref().capacity(), 2);
            assert_eq!(next.as_ref().len(), 0);
            assert_eq!(next.as_ref().capacity(), 0);
            assert!(Buffer::replace(first.as_ptr(), first.as_ptr()).is_err());
            first.as_mut().truncate(0).unwrap();
            assert!(first.as_ref().load(0).is_err());
            first.as_mut().initialize_next(42).unwrap();
            assert_eq!(first.as_ref().load(0).unwrap(), 42);
            let mut replacement = Buffer::allocate(&mut arena, 4).unwrap();
            replacement.as_mut().initialize_next(7).unwrap();
            Buffer::replace(first.as_ptr(), replacement.as_ptr()).unwrap();
            assert_eq!(first.as_ref().load(0).unwrap(), 7);
        }
        LIVE.with(|n| assert_eq!(n.get(), 6));
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn buffer_capacity_limit_matches_checked_target_layout() {
        let limit = Buffer::capacity_limit();
        assert!(Layout::array::<i32>(usize::try_from(limit).unwrap()).is_ok());
        assert!(Layout::array::<i32>(usize::try_from(limit + 1).unwrap()).is_err());
        let mut arena = Arena::default();
        for count in [limit + 1, u64::MAX] {
            assert!(Buffer::allocate(&mut arena, count).is_err());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
