//! Minimal I/O runtime ABI: `jk_print`, `jk_println`, and panic.

use std::io::{self, Write};

pub const PRINTLN_SYMBOL: &str = "jk_println";
pub const PRINT_SYMBOL: &str = "jk_print";
pub const ALLOCATE_SYMBOL: &str = "jk_allocate";
pub const PANIC_SYMBOL: &str = "jk_panic";

pub extern "C" fn jk_echo(
    location: *const u8,
    location_length: usize,
    value: *const u8,
    value_length: usize,
) {
    // Hold the lock across the whole record so concurrent tasks cannot interleave it.
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(b"[");
    if !location.is_null() {
        let _ = stderr.write_all(unsafe { std::slice::from_raw_parts(location, location_length) });
    }
    let _ = stderr.write_all(b"] ");
    if !value.is_null() {
        let _ = stderr.write_all(unsafe { std::slice::from_raw_parts(value, value_length) });
    }
    let _ = stderr.write_all(b"\n");
}

pub extern "C" fn jk_panic(pointer: *const u8, length: usize) {
    // The compiler terminates the current MIR path immediately after this call.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(b"panic: ");
    let _ = stderr.write_all(bytes);
    let _ = stderr.write_all(b"\n");
}

pub extern "C" fn jk_println(pointer: *const u8, length: usize) {
    // The runtime ABI guarantees a live UTF-8 buffer for the duration of this call.
    let mut stdout = io::stdout().lock();
    if !pointer.is_null() {
        // A null pointer represents the zero-initialized empty String used by
        // an unfulfilled suspending result.
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        let _ = stdout.write_all(bytes);
    }
    let _ = stdout.write_all(b"\n");
}

pub extern "C" fn jk_print(pointer: *const u8, length: usize) {
    let mut stdout = io::stdout().lock();
    if !pointer.is_null() {
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        let _ = stdout.write_all(bytes);
    }
    let _ = stdout.flush();
}

pub extern "C" fn jk_allocate(size: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size.max(1), 8)
        .expect("runtime allocations use a valid layout");
    unsafe { std::alloc::alloc(layout) }
}
