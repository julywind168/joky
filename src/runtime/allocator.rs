//! Allocation helpers backed by the Rust global allocator.
//!
//! These helpers reserve an interface for future arena or slab allocators.
//! Callers must preserve allocation layouts when freeing memory.

/// Allocate `size` bytes with alignment `align`.
///
/// # Safety
/// The layout must be valid and alignment a power of two. Zero size returns null.
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    let layout =
        std::alloc::Layout::from_size_align(size, align).expect("Invalid allocation layout");
    if size == 0 {
        return std::ptr::null_mut();
    }
    std::alloc::alloc(layout)
}

/// Free memory returned by `alloc`.
///
/// # Safety
/// `ptr` may be null; otherwise it must be a live allocation from `alloc`.
/// `size` and `align` must match the original allocation.
pub unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    if ptr.is_null() {
        return;
    }
    let layout =
        std::alloc::Layout::from_size_align(size, align).expect("Invalid allocation layout");
    std::alloc::dealloc(ptr, layout);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_size_allocation_is_null_and_safe_to_free() {
        let ptr = unsafe { alloc(0, 8) };
        assert!(ptr.is_null());
        unsafe { dealloc(ptr, 0, 8) };
    }

    #[test]
    fn allocated_memory_can_be_written_and_freed() {
        let ptr = unsafe { alloc(16, 8) };
        assert!(!ptr.is_null());
        unsafe {
            *ptr = 0x5a;
            assert_eq!(*ptr, 0x5a);
            dealloc(ptr, 16, 8);
        }
    }
}
