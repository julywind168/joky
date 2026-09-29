//! Small, allocation-free String queries exposed through `joky/string`.
//!
//! The functions consume the existing borrowed length-prefixed UTF-8 ABI and
//! return scalar values. They deliberately do not create a second String
//! ownership model; concatenation and slicing will follow the future owned
//! buffer design.
use crate::runtime::string::borrow;

fn with_text<T>(text: *const u8, fallback: T, f: impl FnOnce(&str) -> T) -> T {
    // The language guarantees valid String values at this boundary. Keeping a
    // scalar fallback makes null or malformed length/UTF-8 inputs deterministic;
    // callers still must obey the borrowed ABI allocation contract.
    unsafe { borrow(text).map_or(fallback, f) }
}

#[export_name = "joky_string_byte_length"]
pub extern "C" fn byte_length(text: *const u8) -> i32 {
    with_text(text, -1, |text| i32::try_from(text.len()).unwrap_or(-1))
}

#[export_name = "joky_string_find"]
pub extern "C" fn find(text: *const u8, needle: *const u8) -> i32 {
    with_text(text, -1, |text| {
        with_text(needle, -1, |needle| {
            text.find(needle)
                .and_then(|offset| i32::try_from(offset).ok())
                .unwrap_or(-1)
        })
    })
}

#[export_name = "joky_string_starts_with"]
pub extern "C" fn starts_with(text: *const u8, prefix: *const u8) -> i32 {
    with_text(text, 0, |text| {
        i32::from(with_text(prefix, false, |prefix| text.starts_with(prefix)))
    })
}

#[export_name = "joky_string_ends_with"]
pub extern "C" fn ends_with(text: *const u8, suffix: *const u8) -> i32 {
    with_text(text, 0, |text| {
        i32::from(with_text(suffix, false, |suffix| text.ends_with(suffix)))
    })
}

#[export_name = "joky_string_equals"]
pub extern "C" fn equals(text: *const u8, other: *const u8) -> i32 {
    with_text(text, 0, |text| {
        i32::from(with_text(other, false, |other| text == other))
    })
}
