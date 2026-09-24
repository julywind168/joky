//! C-heap cell allocation behind `CMutPtr.alloc`/`free`.
//!
//! Cells are raw malloc blocks: they never enter the managed ownership graph,
//! and freeing a foreign pointer is undefined behavior by contract.

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(pointer: *mut u8);
}

pub const C_ALLOC_SYMBOL: &str = "jk_c_alloc";
pub const C_FREE_SYMBOL: &str = "jk_c_free";

/// Allocate a malloc-aligned C-heap cell. Returns null on allocation failure;
/// callers check with `is_null()`.
pub extern "C" fn jk_c_alloc(size: usize) -> *mut u8 {
    unsafe { malloc(size) }
}

/// Release a cell created by `jk_c_alloc`. A null pointer is a no-op; passing
/// any other foreign address is undefined behavior.
pub extern "C" fn jk_c_free(pointer: *mut u8) {
    if !pointer.is_null() {
        unsafe { free(pointer) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip_through_the_c_heap() {
        let cell = jk_c_alloc(8);
        assert!(!cell.is_null());
        unsafe { cell.cast::<usize>().write(42) };
        assert_eq!(unsafe { cell.cast::<usize>().read() }, 42);
        jk_c_free(cell);
        jk_c_free(std::ptr::null_mut());
    }
}
