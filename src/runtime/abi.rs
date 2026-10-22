//! Narrow, audited wrappers for the native function-pointer ABI.
//!
//! Host registrations intentionally remain unsafe at the public boundary: the
//! caller must provide a pointer whose real C ABI matches the checked language
//! signature. Keeping the pointer casts here prevents each backend from
//! independently re-implementing that contract.
//! Native failures are checked before returning values, including borrowed pointers.

use super::cps::{CpsEntry, CpsFrame};
use crate::Error;

// The table fixes every machine signature; all calls share native error handling.
macro_rules! scalar_calls {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(#[doc = concat!("# Safety\n`entry` must point to a live `extern \"C\" fn(",
            stringify!($($ty),*), ") -> ", stringify!($ret), "` for this call.")]
        pub(crate) unsafe fn $name(entry: *const u8, $($arg: $ty),*) -> Result<$ret, Error> {
            let call: unsafe extern "C" fn($($ty),*) -> $ret = std::mem::transmute(entry);
            super::error::checked(|| call($($arg),*))
        })*
    };
}

scalar_calls! {
    fn call_i32_0() -> i32;
    fn call_i32_1(a: i32) -> i32;
    fn call_i32_2(a: i32, b: i32) -> i32;
    fn call_named_0() -> u64;
    fn call_named_i32(value: u64) -> i32;
    fn call_unit_0() -> ();
    fn call_unit_1(a: i32) -> ();
    fn call_unit_2(a: i32, b: i32) -> ();
    fn call_i64_0() -> i64;
    fn call_i64_1(a: i64) -> i64;
    fn call_i64_2(a: i64, b: i64) -> i64;
    fn call_unit_i64_1(a: i64) -> ();
    fn call_unit_i64_2(a: i64, b: i64) -> ();
    fn call_u8_0() -> u8;
    fn call_u8_1(a: u8) -> u8;
    fn call_u8_2(a: u8, b: u8) -> u8;
    fn call_unit_u8_1(a: u8) -> ();
    fn call_unit_u8_2(a: u8, b: u8) -> ();
    fn call_u32_0() -> u32;
    fn call_u32_1(a: u32) -> u32;
    fn call_u32_2(a: u32, b: u32) -> u32;
    fn call_unit_u32_1(a: u32) -> ();
    fn call_unit_u32_2(a: u32, b: u32) -> ();
    fn call_u64_0() -> u64;
    fn call_u64_1(a: u64) -> u64;
    fn call_u64_2(a: u64, b: u64) -> u64;
    fn call_unit_u64_1(a: u64) -> ();
    fn call_unit_u64_2(a: u64, b: u64) -> ();
    fn call_f32_0() -> f32;
    fn call_f32_1(a: f32) -> f32;
    fn call_f32_2(a: f32, b: f32) -> f32;
    fn call_unit_f32_1(a: f32) -> ();
    fn call_unit_f32_2(a: f32, b: f32) -> ();
    fn call_f64_0() -> f64;
    fn call_f64_1(a: f64) -> f64;
    fn call_f64_2(a: f64, b: f64) -> f64;
    fn call_unit_f64_1(a: f64) -> ();
    fn call_unit_f64_2(a: f64, b: f64) -> ();
    fn call_f64_3(a: f64, b: f64, c: f64) -> f64;
    fn call_f64_i32(a: f64, b: i32) -> f64;
    fn call_i32_f64(a: i32, b: f64) -> f64;
    fn call_i32_f64_0(a: i32) -> f64;
    fn call_i32_f64_f64_i32(a: i32, x: f64, y: f64) -> i32;
    fn call_i8_0() -> i8;
    fn call_i8_1(a: i8) -> i8;
    fn call_i8_2(a: i8, b: i8) -> i8;
    fn call_unit_i8_1(a: i8) -> ();
    fn call_unit_i8_2(a: i8, b: i8) -> ();
    fn call_i16_0() -> i16;
    fn call_i16_1(a: i16) -> i16;
    fn call_i16_2(a: i16, b: i16) -> i16;
    fn call_unit_i16_1(a: i16) -> ();
    fn call_unit_i16_2(a: i16, b: i16) -> ();
    fn call_u16_0() -> u16;
    fn call_u16_1(a: u16) -> u16;
    fn call_u16_2(a: u16, b: u16) -> u16;
    fn call_unit_u16_1(a: u16) -> ();
    fn call_unit_u16_2(a: u16, b: u16) -> ();
    fn call_named_u64_i32(handle: u64, value: u64) -> i32;
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
/// `entry` must match fn(*const u8); text is a live JokyText for this call.
pub(crate) unsafe fn call_string_unit(entry: *const u8, text: *const u8) -> Result<(), Error> {
    let f: unsafe extern "C" fn(*const u8) = std::mem::transmute(entry);
    crate::runtime::error::checked(|| f(text))
}
/// # Safety
/// Entry must match fn(*const u8, i32); text is a live JokyText.
pub(crate) unsafe fn call_string_i32_unit(
    entry: *const u8,
    text: *const u8,
    value: i32,
) -> Result<(), Error> {
    let f: unsafe extern "C" fn(*const u8, i32) = std::mem::transmute(entry);
    crate::runtime::error::checked(|| f(text, value))
}
/// # Safety
/// Entry must match fn(*const u8) -> f64; text is a live JokyText.
pub(crate) unsafe fn call_string_f64(entry: *const u8, text: *const u8) -> Result<f64, Error> {
    let f: unsafe extern "C" fn(*const u8) -> f64 = std::mem::transmute(entry);
    crate::runtime::error::checked(|| f(text))
}
/// # Safety
/// Entry must match fn(*const u8, f64); text is a live JokyText.
pub(crate) unsafe fn call_string_f64_unit(
    entry: *const u8,
    text: *const u8,
    value: f64,
) -> Result<(), Error> {
    let f: unsafe extern "C" fn(*const u8, f64) = std::mem::transmute(entry);
    crate::runtime::error::checked(|| f(text, value))
}

/// # Safety
/// Entry must match fn(*const u8) -> i32; text is a live JokyText.
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

// Each monomorphic wrapper fixes the return ABI; pointer inputs have identical
// representation for shared/mutable borrowing and opaque Ptr values. The registration contract
// supplies the pointee type, lifetime, aliasing and threading guarantees.
macro_rules! c_reference_call {
    ($name:ident, $ret:ty) => {
        /// # Safety
        /// The entry must match this return type and exactly args.len() C pointer
        /// parameters (at most four). References must stay live and obey their
        /// registered layouts and permissions until the synchronous call ends.
        /// Opaque Ptr arguments/results must obey the foreign API's validity,
        /// nullability, ownership and threading contract.
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
c_reference_call!(call_c_u8, u8);
c_reference_call!(call_c_u32, u32);
c_reference_call!(call_c_u64, u64);
c_reference_call!(call_c_unit, ());
c_reference_call!(call_c_ptr, *mut u8);
c_reference_call!(call_c_f64, f64);

/// # Safety
/// `entry` must accept an Int32 and a live JokyText descriptor and return Int32.
pub(crate) unsafe fn call_i32_string_i32(
    entry: *const u8,
    a: i32,
    text: *const u8,
) -> Result<i32, Error> {
    let f: extern "C" fn(i32, *const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(a, text))
}

/// # Safety
/// `entry` returns a JokyText descriptor readable until the caller copies it.
pub(crate) unsafe fn call_i32_string(entry: *const u8, a: i32) -> Result<*const u8, Error> {
    let f: extern "C" fn(i32) -> *const u8 = std::mem::transmute(entry);
    super::error::checked(|| f(a))
}

/// # Safety
/// `entry` must be a vm call builtin taking a handle, a JokyText pointer and
/// one f64 argument, returning f64.
pub(crate) unsafe fn call_named_string_f64(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: f64,
) -> Result<f64, Error> {
    let f: extern "C" fn(u64, *const u8, f64) -> f64 = std::mem::transmute(entry);
    super::error::checked(|| f(value, text, a))
}

/// # Safety
/// `entry` must be a vm call builtin taking a handle, a JokyText pointer and
/// two f64 arguments, returning f64.
pub(crate) unsafe fn call_named_string_f64_f64(
    entry: *const u8,
    value: u64,
    text: *const u8,
    a: f64,
    b: f64,
) -> Result<f64, Error> {
    let f: extern "C" fn(u64, *const u8, f64, f64) -> f64 = std::mem::transmute(entry);
    super::error::checked(|| f(value, text, a, b))
}

c_reference_call!(call_c_i8, i8);
c_reference_call!(call_c_i16, i16);
c_reference_call!(call_c_u16, u16);

/// # Safety
/// Entry must match fn(u64, u64, *const u8, *const u8) -> i32. Text pointers
/// reference live immutable JokyText descriptors for the duration of this call.
pub(crate) unsafe fn call_named_u64_strings_i32(
    entry: *const u8,
    handle: u64,
    schema: u64,
    source: *const u8,
    method: *const u8,
) -> Result<i32, Error> {
    let f: unsafe extern "C" fn(u64, u64, *const u8, *const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(handle, schema, source, method))
}

/// # Safety
/// Entry must match fn(u64, u64, *const u8, *const u8, *const u8) -> i32.
/// Text pointers borrow live immutable JokyText descriptors during this call.
pub(crate) unsafe fn call_named_u64_three_strings_i32(
    entry: *const u8,
    handle: u64,
    schema: u64,
    source: *const u8,
    start: *const u8,
    advance: *const u8,
) -> Result<i32, Error> {
    let f: unsafe extern "C" fn(u64, u64, *const u8, *const u8, *const u8) -> i32 =
        std::mem::transmute(entry);
    super::error::checked(|| f(handle, schema, source, start, advance))
}

/// # Safety
/// Entry must match fn(u64, *const u8, *const u8) -> i32. Text pointers borrow
/// aligned live immutable JokyText descriptors during the call.
pub(crate) unsafe fn call_named_strings_i32(
    entry: *const u8,
    handle: u64,
    first: *const u8,
    second: *const u8,
) -> Result<i32, Error> {
    let f: unsafe extern "C" fn(u64, *const u8, *const u8) -> i32 = std::mem::transmute(entry);
    super::error::checked(|| f(handle, first, second))
}
