//! Narrow, audited wrappers for the native function-pointer ABI.
//!
//! Host registrations intentionally remain unsafe at the public boundary: the
//! caller must provide a pointer whose real C ABI matches the checked language
//! signature. Keeping the pointer casts here prevents each backend from
//! independently re-implementing that contract.

use super::cps::{CpsEntry, CpsFrame};

/// # Safety
/// `entry` must point to an `extern "C" fn() -> i32`.
pub(crate) unsafe fn call_i32_0(entry: *const u8) -> i32 {
    let f: extern "C" fn() -> i32 = std::mem::transmute(entry);
    f()
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32) -> i32`.
pub(crate) unsafe fn call_i32_1(entry: *const u8, a: i32) -> i32 {
    let f: extern "C" fn(i32) -> i32 = std::mem::transmute(entry);
    f(a)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32, i32) -> i32`.
pub(crate) unsafe fn call_i32_2(entry: *const u8, a: i32, b: i32) -> i32 {
    let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(entry);
    f(a, b)
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> u64`.
pub(crate) unsafe fn call_named_0(entry: *const u8) -> u64 {
    let f: extern "C" fn() -> u64 = std::mem::transmute(entry);
    f()
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64) -> i32`.
pub(crate) unsafe fn call_named_i32(entry: *const u8, value: u64) -> i32 {
    let f: extern "C" fn(u64) -> i32 = std::mem::transmute(entry);
    f(value)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8) -> i32`.
pub(crate) unsafe fn call_named_string_i32(entry: *const u8, value: u64, text: *const u8) -> i32 {
    let f: extern "C" fn(u64, *const u8) -> i32 = std::mem::transmute(entry);
    f(value, text)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8, i32) -> i32`.
pub(crate) unsafe fn call_named_string_i32_i32(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: i32,
) -> i32 {
    let f: extern "C" fn(u64, *const u8, i32) -> i32 = std::mem::transmute(entry);
    f(value, text, a)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8, i32, i32) -> i32`.
pub(crate) unsafe fn call_named_string_i32_i32_i32(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: i32,
    b: i32,
) -> i32 {
    let f: extern "C" fn(u64, *const u8, i32, i32) -> i32 = std::mem::transmute(entry);
    f(value, text, a, b)
}

/// # Safety
/// `entry` must point to an `extern "C" fn()`.
pub(crate) unsafe fn call_unit_0(entry: *const u8) {
    let f: extern "C" fn() = std::mem::transmute(entry);
    f()
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32)`.
pub(crate) unsafe fn call_unit_1(entry: *const u8, a: i32) {
    let f: extern "C" fn(i32) = std::mem::transmute(entry);
    f(a)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32, i32)` .
pub(crate) unsafe fn call_unit_2(entry: *const u8, a: i32, b: i32) {
    let f: extern "C" fn(i32, i32) = std::mem::transmute(entry);
    f(a, b)
}

/// # Safety
/// `entry` must point to an `extern "C" fn(*const u8) -> i32`.
pub(crate) unsafe fn call_string_i32(entry: *const u8, text: *const u8) -> i32 {
    let f: extern "C" fn(*const u8) -> i32 = std::mem::transmute(entry);
    f(text)
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> *const u8`.
pub(crate) unsafe fn call_string_0(entry: *const u8) -> *const u8 {
    let f: extern "C" fn() -> *const u8 = std::mem::transmute(entry);
    f()
}

/// # Safety
/// `entry` must point to an `extern "C" fn(*const u8) -> *const u8`.
pub(crate) unsafe fn call_string_string(entry: *const u8, text: *const u8) -> *const u8 {
    let f: extern "C" fn(*const u8) -> *const u8 = std::mem::transmute(entry);
    f(text)
}

/// # Safety
/// `entry` must point to a function obeying the shared CPS ABI.
pub(crate) unsafe fn as_cps_entry(entry: *const u8) -> CpsEntry {
    std::mem::transmute(entry)
}

/// # Safety
/// `entry` must obey the shared CPS ABI and `frame` must be a live frame owned
/// exclusively by the caller for the duration of this poll.
pub(crate) unsafe fn poll_cps(entry: CpsEntry, frame: *mut CpsFrame) -> i32 {
    entry(frame)
}
