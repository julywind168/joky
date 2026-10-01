//! Private typed storage with task-registered, independently reclaimable owners.
use super::arena::Arena;
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::{alloc::Layout, ptr::NonNull};

/// Stable control block; no element pointers or layout inputs are source-visible.
/// Replaced payloads and explicitly released owners refund their arena charge.
pub(crate) struct Buffer {
    data: NonNull<u8>,
    len: usize,
    capacity: usize,
    element: Layout,
    cleanup: Option<ElementCleanup>,
    // Only terminal cleanup follows a failed replacement. Null owner lanes
    // alone cannot distinguish it from a live enum with a nullary variant.
    failed_store: Option<usize>,
}

#[derive(Clone)]
pub(crate) enum ElementCallback {
    Native(super::cleanup::Callback),
    Interpreted(super::cleanup::InterpretedCallback),
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
    /// Build cleanup for an initialized element, testing outer tags first.
    ///
    /// # Safety
    /// Data must address a live element with the descriptor's byte layout,
    /// including valid Int32 tag and buffer lanes. Its uniquely owned buffers,
    /// hook bytes and callback code must remain live on this task thread.
    unsafe fn entry(&self, data: *mut u8) -> super::cleanup::Entry {
        super::cleanup::Entry {
            owners: self
                .owners
                .iter()
                .map(|offset| {
                    NonNull::new(data.add(*offset).cast::<*mut Buffer>().read_unaligned())
                        .map(NonNull::as_ptr)
                })
                .collect(),
            hooks: self
                .hooks
                .iter()
                .filter(|hook| {
                    hook.guards.iter().all(|(offset, tag)| {
                        data.add(*offset).cast::<i32>().read_unaligned() == *tag
                    })
                })
                .map(|hook| {
                    let pointer = data.add(hook.offset);
                    let pointer = if hook.indirect {
                        pointer.cast::<*mut u8>().read_unaligned()
                    } else {
                        pointer
                    };
                    let callback = match &hook.callback {
                        ElementCallback::Native(callback) => super::cleanup::Hook::Native {
                            data: pointer,
                            callback: *callback,
                        },
                        ElementCallback::Interpreted(callback) => {
                            super::cleanup::Hook::Interpreted {
                                data: pointer,
                                callback: callback.clone(),
                            }
                        }
                    };
                    (hook.before, callback)
                })
                .collect(),
        }
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
            data: NonNull::new(element.align() as *mut u8).unwrap(),
            len: 0,
            capacity: 0,
            element,
            cleanup: None,
            failed_store: None,
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
            cleanup: None,
            failed_store: None,
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

    pub(crate) fn configure(&mut self, cleanup: ElementCleanup) {
        self.cleanup = Some(cleanup);
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

    /// Consume an unborrowed owner. Stored references still point at independent
    /// arena objects; their task-long shared loans are not released here.
    pub(crate) unsafe fn release(arena: &mut Arena, owner: *mut Self) -> Result<(), Error> {
        Self::validate_owner(arena, owner)?;
        let result = (*owner).truncate_owned(arena, 0);
        if (*owner).capacity != 0 && (*owner).element.size() != 0 {
            arena.release((*owner).data);
        }
        // Arena allocations are raw bytes: drop callback descriptors explicitly.
        (*owner).cleanup.take();
        arena.release(NonNull::new_unchecked(owner.cast()));
        result
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
        (*destination).cleanup.take();
        let value = source.read();
        if (*destination).capacity != 0 && (*destination).element.size() != 0 {
            arena.release((*destination).data);
        }
        destination.write(value);
        arena.release(NonNull::new_unchecked(source.cast()));
        Ok(())
    }
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
unsafe extern "C" fn cleanup_new(buffer: *mut Buffer) {
    if let Some(buffer) = buffer.as_mut() {
        buffer.configure(ElementCleanup::default());
    }
}
#[export_name = "__joky_buffer_cleanup_owner"]
unsafe extern "C" fn cleanup_owner(buffer: *mut Buffer, offset: usize) {
    if let Some(buffer) = buffer.as_mut() {
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
    let Some(buffer) = buffer.as_mut() else {
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
    if let Some(buffer) = buffer.as_mut() {
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

pub(crate) fn symbols() -> [(&'static str, *const u8); 19] {
    [
        ("__joky_buffer_transfer", transfer as *const u8),
        ("__joky_buffer_take_last", take_last as *const u8),
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
