//! Observe allocations in the ordinary runtime, not a cfg(test) substitute.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use joky::runtime::error::{
    call_failed, joky_runtime_error_clear, joky_runtime_error_code, joky_runtime_error_end,
    joky_runtime_error_start,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct Tracked;
fn allocated() {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
    }
}
unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated();
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated();
        System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated();
        System.realloc(pointer, layout, size)
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}
#[global_allocator]
static ALLOCATOR: Tracked = Tracked;

unsafe extern "C" {
    fn __joky_native_stack_room_v1(
        sp: usize,
        bytes: usize,
        source: u64,
        start: usize,
        end: usize,
    ) -> i32;
}

#[test]
fn native_stack_failure_propagation_does_not_allocate() {
    std::thread::spawn(|| {
        for _ in 0..2 {
            // The first iteration covers cold stack/error TLS in this thread.
            ALLOCATIONS.set(0);
            TRACK.set(true);
            let sp: usize;
            unsafe { std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
            let room = unsafe { __joky_native_stack_room_v1(sp, usize::MAX, 7, 11, 19) };
            for n in 0..10_000 {
                call_failed(7, n, n + 1);
            }
            let code = joky_runtime_error_code();
            let start = joky_runtime_error_start();
            let end = joky_runtime_error_end();
            joky_runtime_error_clear();
            TRACK.set(false);
            assert_eq!((room, code, start, end), (0, 1, 11, 19));
            assert_eq!(ALLOCATIONS.get(), 0);
        }
    })
    .join()
    .unwrap();
}
