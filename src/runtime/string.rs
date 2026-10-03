//! Explicit borrowed UTF-8 descriptors at host boundaries.
pub use super::owned_text::{joky_string_result_free_v2, JokyOwnedTextV2};
use crate::{Error, ErrorCode, ErrorPhase};

/// The descriptor and its immutable UTF-8 payload must outlive every borrow.
/// A null data pointer is accepted only for an empty payload. No NUL terminator.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct JokyText {
    pub data: *const u8,
    pub byte_length: u64,
}
impl Default for JokyText {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            byte_length: 0,
        }
    }
}
impl JokyText {
    /// The caller keeps `text` live and immutable while using this descriptor.
    pub fn new(text: &str) -> Self {
        Self {
            data: text.as_ptr(),
            byte_length: text.len() as u64,
        }
    }
    pub fn as_ptr(&self) -> *const u8 {
        (self as *const Self).cast()
    }
}

/// # Safety
/// `pointer` addresses a live JokyText descriptor. Its nonempty bounded payload
/// must be initialized and readable; descriptor and bytes remain immutable and
/// live for the returned borrow. Validation cannot establish pointer validity.
pub unsafe fn borrow<'a>(pointer: *const u8) -> Result<&'a str, Error> {
    let view = pointer
        .cast::<JokyText>()
        .as_ref()
        .ok_or_else(|| abi_error("Null text descriptor"))?;
    if view.byte_length > super::arena::MAX_BYTES as u64 {
        return Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Text exceeds the task byte limit",
        ));
    }
    let bytes = if view.byte_length == 0 {
        &[]
    } else {
        if view.data.is_null() {
            return Err(abi_error("Null text payload"));
        }
        std::slice::from_raw_parts(view.data, view.byte_length as usize)
    };
    std::str::from_utf8(bytes).map_err(|_| abi_error("Text is not UTF-8"))
}
fn abi_error(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
