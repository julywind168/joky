//! Shared host-side stack placement for ordinary JIT and standalone AOT code.
//! Move SP only within the current pthread's real stack; never spoof its bounds.
use std::arch::{asm, global_asm};

pub type Entry = unsafe extern "C" fn(i32) -> i32;

global_asm!(
    ".text",
    ".p2align 2",
    ".globl _joky_test_call_at_stack",
    "_joky_test_call_at_stack:",
    "stp x29, x30, [sp, #-32]!",
    "stp x19, x20, [sp, #16]",
    "mov x29, sp",
    "mov x19, sp",
    "mov x9, x0",
    "mov w0, w1",
    "mov sp, x2",
    "blr x9",
    "mov sp, x19",
    "ldp x19, x20, [sp, #16]",
    "ldp x29, x30, [sp], #32",
    "ret",
);
unsafe extern "C" {
    fn joky_test_call_at_stack(entry: Entry, argument: i32, sp: usize) -> i32;
    fn pthread_self() -> usize;
    fn pthread_get_stackaddr_np(thread: usize) -> *mut u8;
    fn pthread_get_stacksize_np(thread: usize) -> usize;
}

pub struct Errors {
    pub clear: extern "C" fn(),
    pub code: extern "C" fn() -> i32,
    pub start: extern "C" fn() -> usize,
    pub end: extern "C" fn() -> usize,
}
#[derive(Debug)]
pub struct Observation {
    pub value: i32,
    pub code: i32,
    pub start: usize,
    pub end: usize,
}

// Callbacks observe allocation balance on the original, ample host stack.
// No callback or test hook is inserted into the generated/runtime call chain.
pub fn exercise(
    entries: [Entry; 3],
    errors: Errors,
    begin: fn(),
    finish: fn(),
    spans: [(usize, usize); 3],
) {
    let thread = unsafe { pthread_self() };
    let high = unsafe { pthread_get_stackaddr_np(thread) } as usize;
    let size = unsafe { pthread_get_stacksize_np(thread) };
    assert!(size >= 512 * 1024);
    let low = high - size;
    let reserve = 256 * 1024;
    let host_sp: usize;
    unsafe {
        asm!("mov {0}, sp", out(reg) host_sp, options(nomem, nostack));
    }
    assert!(low + reserve + 160 * 1024 + 4096 < host_sp);
    let mut old_cr: u64;
    let mut old_sr: u64;
    unsafe {
        asm!("mrs {0}, fpcr", out(reg) old_cr, options(nomem, nostack));
        asm!("mrs {0}, fpsr", out(reg) old_sr, options(nomem, nostack));
    }
    let hostile_cr: u64 = (1 << 24) | (1 << 22);
    let hostile_sr: u64 = 16;
    let call = |entry, arg, remaining| {
        (errors.clear)();
        begin();
        let value;
        let after_cr: u64;
        let after_sr: u64;
        unsafe {
            asm!("msr fpcr, {0}", in(reg) hostile_cr, options(nomem, nostack));
            asm!("msr fpsr, {0}", in(reg) hostile_sr, options(nomem, nostack));
            value = joky_test_call_at_stack(entry, arg, low + remaining);
            asm!("mrs {0}, fpcr", out(reg) after_cr, options(nomem, nostack));
            asm!("mrs {0}, fpsr", out(reg) after_sr, options(nomem, nostack));
        }
        let result = Observation {
            value,
            code: (errors.code)(),
            start: (errors.start)(),
            end: (errors.end)(),
        };
        (errors.clear)();
        finish();
        assert_eq!((after_cr, after_sr), (hostile_cr, hostile_sr));
        result
    };
    // Find the first successful complete maximum-depth construction at ABI
    // alignment precision. The immediately preceding point must reject.
    let mut below = reserve;
    let mut above = reserve + 48 * 1024;
    assert_ne!(call(entries[0], 1, below).code, 0);
    assert_eq!(call(entries[0], 1, above).code, 0);
    while above - below > 16 {
        let middle = ((below + above) / 2) & !15;
        if call(entries[0], 1, middle).code == 0 {
            above = middle;
        } else {
            below = middle;
        }
    }
    for _ in 0..2 {
        let denied = call(entries[0], 1, below);
        assert_ne!(denied.code, 0);
        assert!(denied.end > denied.start);
        let completed = call(entries[0], 1, above);
        assert_eq!((completed.code, completed.value), (0, 42));
        // At the first admitted depth, the second element fails after the first
        // owner was initialized; cleanup must preserve the arithmetic span.
        let partial = call(entries[0], 0, above);
        assert_ne!(partial.code, 0);
        assert_eq!((partial.start, partial.end), spans[0], "{partial:?}");
        for (route, expected) in [(1, spans[1]), (2, spans[2])] {
            // The caller and constructor fit, but the padded target does not.
            // Direct preflight retains ownership; indirect entry has received it.
            let denied = call(entries[route], 1, above + 4096);
            assert_ne!(denied.code, 0);
            assert_eq!(
                (denied.start, denied.end),
                expected,
                "route {route}: {denied:?}"
            );
            let recovered = call(entries[route], 1, reserve + 160 * 1024);
            assert_eq!((recovered.code, recovered.value), (0, 42));
        }
    }
    unsafe {
        asm!("msr fpcr, {0}", in(reg) old_cr, options(nomem, nostack));
        asm!("msr fpsr, {0}", in(reg) old_sr, options(nomem, nostack));
    }
}
