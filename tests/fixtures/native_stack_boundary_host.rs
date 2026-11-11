//! Standalone AOT host: observe the ordinary runtime through Rust's allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
}
struct Tracked;
fn change(delta: isize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        LIVE.with(|live| live.set(live.get() + delta));
    }
}
unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let value = System.alloc(layout);
        if !value.is_null() {
            change(1);
        }
        value
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let value = System.alloc_zeroed(layout);
        if !value.is_null() {
            change(1);
        }
        value
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        System.realloc(pointer, layout, size)
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        change(-1);
        System.dealloc(pointer, layout);
    }
}
#[global_allocator]
static ALLOCATOR: Tracked = Tracked;
fn begin() {
    LIVE.set(0);
    TRACK.set(true);
}
fn finish() {
    TRACK.set(false);
    assert_eq!(LIVE.get(), 0, "standalone runtime leaked an allocation");
}
mod boundary;
unsafe extern "C" {
    fn construct(n: i32) -> i32;
    fn direct(n: i32) -> i32;
    fn indirect(n: i32) -> i32;
}
fn main() {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            use joky::runtime::error::*;
            boundary::exercise(
                [construct, direct, indirect],
                boundary::Errors {
                    clear: joky_runtime_error_clear,
                    code: joky_runtime_error_code,
                    start: joky_runtime_error_start,
                    end: joky_runtime_error_end,
                },
                begin,
                finish,
                SPANS,
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
