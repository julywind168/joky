//! Shared JIT/AOT host: bounded callbacks, retained borrows and real stack placement.
use super::joky;
use joky::runtime::{
    borrowed_host::{BorrowingHostV1, STOP_CANCELLED, STOP_COMPLETED},
    cps::{self, CpsEntry, CpsFrame},
    error,
    resource::ResourceFactoryV1,
};
use std::{
    arch::{asm, global_asm},
    cell::Cell,
};

thread_local! {
    static MADE: Cell<usize> = const { Cell::new(0) };
    static DROPPED: Cell<usize> = const { Cell::new(0) };
    static ACTIVE: Cell<usize> = const { Cell::new(0) };
    static STOPPED: Cell<usize> = const { Cell::new(0) };
    static READY: Cell<bool> = const { Cell::new(true) };
}

unsafe extern "C" fn create(tag: u64, output: *mut *mut std::ffi::c_void) -> i32 {
    assert_eq!(tag, 7);
    *output = Box::into_raw(Box::new(tag)).cast();
    MADE.set(MADE.get() + 1);
    0
}

unsafe extern "C" fn release(pointer: *mut std::ffi::c_void) {
    assert_eq!(
        ACTIVE.get(),
        0,
        "host access must stop before owner cleanup"
    );
    assert_eq!(*Box::from_raw(pointer.cast::<u64>()), 7);
    DROPPED.set(DROPPED.get() + 1);
}

#[no_mangle]
pub static __joky_resource_factory_v1_stack_owner: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create),
    release: Some(release),
};

unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
    if (*frame).state == 0 {
        assert_eq!(ACTIVE.get(), 0);
        ACTIVE.set(1);
        (*frame).host_context = (&(*frame).slots)[0] as usize as *mut std::ffi::c_void;
        (*frame).state = 1;
    }
    assert_eq!(*(*frame).host_context.cast::<i32>(), 42);
    if !READY.get() {
        return 1;
    }
    (*frame).result = 42;
    (*frame).state = cps::DONE;
    0
}

unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    assert!(matches!(reason, STOP_COMPLETED | STOP_CANCELLED));
    assert_eq!(*(*frame).host_context.cast::<i32>(), 42);
    assert_eq!(ACTIVE.replace(0), 1);
    STOPPED.set(STOPPED.get() + 1);
    (*frame).host_context = std::ptr::null_mut();
}

#[no_mangle]
pub static __joky_cps_borrow_v1_stack_wait: BorrowingHostV1 = BorrowingHostV1 {
    abi_version: 1,
    poll: Some(poll),
    stop: Some(stop),
};

global_asm!(
    ".text",
    ".p2align 2",
    ".globl _joky_test_cps_at_stack",
    "_joky_test_cps_at_stack:",
    "stp x29, x30, [sp, #-32]!",
    "stp x19, x20, [sp, #16]",
    "mov x29, sp",
    "mov x19, sp",
    "mov x9, x0",
    "mov x0, x1",
    "mov sp, x2",
    "blr x9",
    "mov sp, x19",
    "ldp x19, x20, [sp, #16]",
    "ldp x29, x30, [sp], #32",
    "ret",
);
unsafe extern "C" {
    fn joky_test_cps_at_stack(entry: *const u8, frame: *mut std::ffi::c_void, sp: usize) -> i32;
    fn pthread_self() -> usize;
    fn pthread_get_stackaddr_np(thread: usize) -> *mut u8;
    fn pthread_get_stacksize_np(thread: usize) -> usize;
}

pub fn reset(ready: bool) {
    assert_eq!(ACTIVE.get(), 0);
    MADE.set(0);
    DROPPED.set(0);
    STOPPED.set(0);
    READY.set(ready);
    error::joky_runtime_error_clear();
}

pub fn balanced() {
    assert_eq!(ACTIVE.get(), 0);
    assert_eq!(MADE.get(), 1);
    assert_eq!(DROPPED.get(), 1);
}

fn limited(entry: CpsEntry, frame: &mut CpsFrame, remaining: usize) -> i32 {
    unsafe {
        let thread = pthread_self();
        let low = pthread_get_stackaddr_np(thread) as usize - pthread_get_stacksize_np(thread);
        let sp: usize;
        asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));
        assert!(low + remaining + 4096 < sp);
        joky_test_cps_at_stack(
            entry as *const u8,
            (frame as *mut CpsFrame).cast(),
            low + remaining,
        )
    }
}

fn protected_poll(entry: CpsEntry, frame: &mut CpsFrame, remaining: Option<usize>) -> i32 {
    unsafe {
        let old_cr: u64;
        let old_sr: u64;
        asm!("mrs {}, fpcr", out(reg) old_cr, options(nomem, nostack));
        asm!("mrs {}, fpsr", out(reg) old_sr, options(nomem, nostack));
        let hostile_cr: u64 = (1 << 24) | (1 << 22);
        let hostile_sr: u64 = 16;
        asm!("msr fpcr, {}", in(reg) hostile_cr, options(nomem, nostack));
        asm!("msr fpsr, {}", in(reg) hostile_sr, options(nomem, nostack));
        let result = match remaining {
            Some(bytes) => limited(entry, frame, bytes),
            None => entry(frame),
        };
        let after_cr: u64;
        let after_sr: u64;
        asm!("mrs {}, fpcr", out(reg) after_cr, options(nomem, nostack));
        asm!("mrs {}, fpsr", out(reg) after_sr, options(nomem, nostack));
        asm!("msr fpcr, {}", in(reg) old_cr, options(nomem, nostack));
        asm!("msr fpsr, {}", in(reg) old_sr, options(nomem, nostack));
        assert_eq!((after_cr, after_sr), (hostile_cr, hostile_sr));
        result
    }
}

pub fn exercise(entry: CpsEntry, slots: usize, check: fn()) {
    for depth in [3, 200_000, 3] {
        reset(true);
        let mut frame = CpsFrame::new(slots);
        frame.slots[0] = depth;
        let status = protected_poll(entry, &mut frame, None);
        if depth == 3 {
            assert_eq!(status, 0);
            assert_eq!(frame.result, 42);
            assert_eq!(STOPPED.get(), 1);
            assert_eq!(error::joky_runtime_error_code(), 0);
        } else {
            assert_eq!(status, cps::RUNTIME_ERROR);
            assert_eq!(frame.state, cps::FAILED);
            assert_ne!(error::joky_runtime_error_code(), 0);
            assert!(error::joky_runtime_error_end() > error::joky_runtime_error_start());
        }
        drop(frame);
        balanced();
        check();
        error::joky_runtime_error_clear();
    }
    // Retain a real host borrow across Pending, then reject a resume on a
    // smaller portion of the same pthread stack. The stop hook reads the
    // borrowed local before the parent activation can be destroyed.
    for remaining in [64, 256, 272] {
        reset(false);
        let mut frame = CpsFrame::new(slots);
        frame.slots[0] = 64;
        assert_eq!(protected_poll(entry, &mut frame, None), 1);
        assert_eq!(ACTIVE.get(), 1);
        assert_eq!(
            protected_poll(entry, &mut frame, Some(remaining * 1024)),
            cps::RUNTIME_ERROR
        );
        drop(frame);
        balanced();
        assert_eq!(STOPPED.get(), 1);
        check();
        error::joky_runtime_error_clear();
    }
}

pub fn reject_input(entry: CpsEntry, slots: usize, check: fn()) {
    reset(true);
    let mut frame = CpsFrame::new(slots);
    assert_eq!(
        unsafe { cps::joky_cps_set_text_input_v2(&mut frame, 0, b"owned".as_ptr(), 5) },
        0
    );
    assert_eq!(
        protected_poll(entry, &mut frame, Some(256 * 1024)),
        cps::RUNTIME_ERROR
    );
    assert_eq!(frame.state, cps::FAILED);
    drop(frame);
    check();
    error::joky_runtime_error_clear();
}
