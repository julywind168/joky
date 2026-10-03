//! Borrowed immutable UTF-8 ABI: native-endian u64 byte length, then inline bytes.
//! String literals live in their compiled module's data segment. There is no NUL
//! terminator, per-call allocation, or ownership transfer across the native ABI.
pub use super::owned_text::{joky_string_result_free_v2, JokyOwnedTextV2};

use crate::{Error, ErrorCode, ErrorPhase};

/// Encode text as the borrowed native ABI: native-endian u64 byte length
/// followed by the UTF-8 bytes. The inverse of [`borrow`].
pub fn encode(text: &str) -> Box<[u8]> {
    let mut bytes = (text.len() as u64).to_ne_bytes().to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes.into_boxed_slice()
}

/// # Safety
/// `ptr` must address an eight-byte length followed by that many initialized UTF-8
/// bytes in the same live allocation. The entire object must remain immutable and
/// live for the returned borrow; callers may not retain it beyond its code owner.
pub unsafe fn borrow<'a>(ptr: *const u8) -> Result<&'a str, Error> {
    if ptr.is_null() {
        return Err(abi_error("Null String argument"));
    }
    let len = unsafe { std::ptr::read_unaligned(ptr.cast::<u64>()) };
    if len > (isize::MAX as u64) - 8 {
        return Err(abi_error("String argument is too large"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len as usize) };
    std::str::from_utf8(bytes).map_err(|_| abi_error("String argument is not UTF-8"))
}

fn abi_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
