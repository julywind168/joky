//! Narrow, audited wrappers for the native function-pointer ABI.
//!
//! Host registrations intentionally remain unsafe at the public boundary: the
//! caller must provide a pointer whose real C ABI matches the checked language
//! signature. Keeping the pointer casts here prevents each backend from
//! independently re-implementing that contract.
//! Native failures are checked before returning values, including borrowed pointers.

use super::cps::{CpsEntry, CpsFrame};
use crate::Error;

/// # Safety
/// `entry` must point to an `extern "C" fn() -> i32`.
pub(crate) unsafe fn call_i32_0(entry: *const u8) -> Result<i32, Error> {
    let f: extern "C" fn() -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32) -> i32`.
pub(crate) unsafe fn call_i32_1(entry: *const u8, a: i32) -> Result<i32, Error> {
    let f: extern "C" fn(i32) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32, i32) -> i32`.
pub(crate) unsafe fn call_i32_2(entry: *const u8, a: i32, b: i32) -> Result<i32, Error> {
    let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> u64`.
pub(crate) unsafe fn call_named_0(entry: *const u8) -> Result<u64, Error> {
    let f: extern "C" fn() -> u64 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64) -> i32`.
pub(crate) unsafe fn call_named_i32(entry: *const u8, value: u64) -> Result<i32, Error> {
    let f: extern "C" fn(u64) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(value))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8) -> i32`.
pub(crate) unsafe fn call_named_string_i32(
    entry: *const u8,
    value: u64,
    text: *const u8,
) -> Result<i32, Error> {
    let f: extern "C" fn(u64, *const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(value, text))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8, i32) -> i32`.
pub(crate) unsafe fn call_named_string_i32_i32(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: i32,
) -> Result<i32, Error> {
    let f: extern "C" fn(u64, *const u8, i32) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(value, text, a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, *const u8, i32, i32) -> i32`.
pub(crate) unsafe fn call_named_string_i32_i32_i32(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: i32,
    b: i32,
) -> Result<i32, Error> {
    let f: extern "C" fn(u64, *const u8, i32, i32) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(value, text, a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn()`.
pub(crate) unsafe fn call_unit_0(entry: *const u8) -> Result<(), Error> {
    let f: extern "C" fn() = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32)`.
pub(crate) unsafe fn call_unit_1(entry: *const u8, a: i32) -> Result<(), Error> {
    let f: extern "C" fn(i32) = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i32, i32)` .
pub(crate) unsafe fn call_unit_2(entry: *const u8, a: i32, b: i32) -> Result<(), Error> {
    let f: extern "C" fn(i32, i32) = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(*const u8) -> i32`.
pub(crate) unsafe fn call_string_i32(entry: *const u8, text: *const u8) -> Result<i32, Error> {
    let f: extern "C" fn(*const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(text))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(*const u8, *const u8) -> i32`.
pub(crate) unsafe fn call_string_string_i32(
    entry: *const u8,
    text: *const u8,
    other: *const u8,
) -> Result<i32, Error> {
    let f: extern "C" fn(*const u8, *const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(text, other))
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> *const u8`.
pub(crate) unsafe fn call_string_0(entry: *const u8) -> Result<*const u8, Error> {
    let f: extern "C" fn() -> *const u8 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(*const u8) -> *const u8`.
pub(crate) unsafe fn call_string_string(
    entry: *const u8,
    text: *const u8,
) -> Result<*const u8, Error> {
    let f: extern "C" fn(*const u8) -> *const u8 = std::mem::transmute(entry);
    super::error::checked(|| f(text))
}

/// # Safety
/// `entry` must point to a function obeying the shared CPS ABI.
pub(crate) unsafe fn as_cps_entry(entry: *const u8) -> CpsEntry {
    std::mem::transmute(entry)
}

/// # Safety
/// `entry` must obey the shared CPS ABI and `frame` must be a live frame owned
/// exclusively by the caller for the duration of this poll.
pub(crate) unsafe fn poll_cps(entry: CpsEntry, frame: *mut CpsFrame) -> Result<i32, Error> {
    super::error::checked(|| entry(frame))
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> i64`.
pub(crate) unsafe fn call_i64_0(entry: *const u8) -> Result<i64, Error> {
    let f: extern "C" fn() -> i64 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i64) -> i64`.
pub(crate) unsafe fn call_i64_1(entry: *const u8, a: i64) -> Result<i64, Error> {
    let f: extern "C" fn(i64) -> i64 = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i64, i64) -> i64`.
pub(crate) unsafe fn call_i64_2(entry: *const u8, a: i64, b: i64) -> Result<i64, Error> {
    let f: extern "C" fn(i64, i64) -> i64 = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i64)`.
pub(crate) unsafe fn call_unit_i64_1(entry: *const u8, a: i64) -> Result<(), Error> {
    let f: extern "C" fn(i64) = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(i64, i64)` .
pub(crate) unsafe fn call_unit_i64_2(entry: *const u8, a: i64, b: i64) -> Result<(), Error> {
    let f: extern "C" fn(i64, i64) = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> u32`.
pub(crate) unsafe fn call_u32_0(entry: *const u8) -> Result<u32, Error> {
    let f: extern "C" fn() -> u32 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u32) -> u32`.
pub(crate) unsafe fn call_u32_1(entry: *const u8, a: u32) -> Result<u32, Error> {
    let f: extern "C" fn(u32) -> u32 = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u32, u32) -> u32`.
pub(crate) unsafe fn call_u32_2(entry: *const u8, a: u32, b: u32) -> Result<u32, Error> {
    let f: extern "C" fn(u32, u32) -> u32 = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u32)`.
pub(crate) unsafe fn call_unit_u32_1(entry: *const u8, a: u32) -> Result<(), Error> {
    let f: extern "C" fn(u32) = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u32, u32)` .
pub(crate) unsafe fn call_unit_u32_2(entry: *const u8, a: u32, b: u32) -> Result<(), Error> {
    let f: extern "C" fn(u32, u32) = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn() -> u64`.
pub(crate) unsafe fn call_u64_0(entry: *const u8) -> Result<u64, Error> {
    let f: extern "C" fn() -> u64 = std::mem::transmute(entry);
    super::error::checked(|| f())
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64) -> u64`.
pub(crate) unsafe fn call_u64_1(entry: *const u8, a: u64) -> Result<u64, Error> {
    let f: extern "C" fn(u64) -> u64 = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, u64) -> u64`.
pub(crate) unsafe fn call_u64_2(entry: *const u8, a: u64, b: u64) -> Result<u64, Error> {
    let f: extern "C" fn(u64, u64) -> u64 = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64)`.
pub(crate) unsafe fn call_unit_u64_1(entry: *const u8, a: u64) -> Result<(), Error> {
    let f: extern "C" fn(u64) = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must point to an `extern "C" fn(u64, u64)` .
pub(crate) unsafe fn call_unit_u64_2(entry: *const u8, a: u64, b: u64) -> Result<(), Error> {
    let f: extern "C" fn(u64, u64) = std::mem::transmute(entry);
    super::error::checked(|| f(a, b))
}

// Each monomorphic wrapper fixes the return ABI; pointer inputs have identical
// representation for shared and mutable borrowing. The registration contract
// supplies the pointee type, lifetime, aliasing and threading guarantees.
macro_rules! c_reference_call {
    ($name:ident, $ret:ty) => {
        /// # Safety
        /// The entry must match this return type and exactly args.len() C pointer
        /// parameters (at most four). Pointers must stay live and obey the
        /// registered layouts and permissions until the synchronous call ends.
        pub(crate) unsafe fn $name(entry: *const u8, args: &[*mut u8]) -> Result<$ret, Error> {
            super::error::checked(|| match args {
                [] => {
                    let f: unsafe extern "C" fn() -> $ret = std::mem::transmute(entry);
                    f()
                }
                [a] => {
                    let f: unsafe extern "C" fn(*mut u8) -> $ret = std::mem::transmute(entry);
                    f(*a)
                }
                [a, b] => {
                    let f: unsafe extern "C" fn(*mut u8, *mut u8) -> $ret =
                        std::mem::transmute(entry);
                    f(*a, *b)
                }
                [a, b, c] => {
                    let f: unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> $ret =
                        std::mem::transmute(entry);
                    f(*a, *b, *c)
                }
                [a, b, c, d] => {
                    let f: unsafe extern "C" fn(*mut u8, *mut u8, *mut u8, *mut u8) -> $ret =
                        std::mem::transmute(entry);
                    f(*a, *b, *c, *d)
                }
                _ => unreachable!("validated C pointer arity"),
            })
        }
    };
}
c_reference_call!(call_c_i32, i32);
c_reference_call!(call_c_i64, i64);
c_reference_call!(call_c_u32, u32);
c_reference_call!(call_c_u64, u64);
c_reference_call!(call_c_unit, ());
