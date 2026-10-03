//! Internal owning UTF-8 storage for the String v2 migration.
//! Handles are ordinary UInt8 Buffers charged to the task arena. These helpers
//! do not change the existing language String ABI or its public host interfaces.
use super::{
    arena::{Arena, MAX_BYTES},
    buffer::Buffer,
};
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::ptr::NonNull;

/// # Safety
/// `owner` must be a live compiler-created byte Buffer, with no conflicting
/// mutable access, on the task thread for the entire returned borrow.
pub(crate) unsafe fn borrow<'a>(owner: *const Buffer) -> Result<&'a str, Error> {
    let owner = owner
        .as_ref()
        .ok_or_else(|| abi_error("Null owning String handle"))?;
    std::str::from_utf8(owner.bytes()?).map_err(|_| abi_error("Owning String storage is not UTF-8"))
}

fn abi_error(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}

fn located(
    result: Result<NonNull<Buffer>, Error>,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    match result {
        Ok(owner) => owner.as_ptr(),
        Err(error) => {
            super::error::record_error(error.with_span(
                Span::new(start, end.max(start)).with_source(SourceId::from_raw(source)),
            ));
            std::ptr::null_mut()
        }
    }
}

/// Copy a relocatable, length-prefixed UTF-8 constant into task-owned storage.
///
/// # Safety
/// `arena` must be live and exclusive on this task thread. `literal` is null or
/// addresses a readable native-endian u64 length. If that length fits MAX_BYTES,
/// the same live immutable allocation must contain its initialized payload.
/// Successful handles must enter generated cleanup before another fallible step.
#[export_name = "__joky_string_literal_v2"]
pub(crate) unsafe extern "C" fn literal(
    arena: *mut Arena,
    literal: *const u8,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    let result = (|| {
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        if literal.is_null() {
            return Err(abi_error("Null String literal source"));
        }
        let len = std::ptr::read_unaligned(literal.cast::<u64>());
        // Reject impossible allocations before constructing or validating a slice.
        if len > MAX_BYTES as u64 {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "String literal exceeds the task arena budget",
            ));
        }
        let text = super::string::borrow(literal)?;
        Buffer::copy_bytes(arena, text.as_bytes())
    })();
    located(result, source, start, end)
}

/// # Safety
/// `arena` is a live exclusive task arena; `owner` obeys `borrow`'s contract.
/// The returned handle requires independent generated cleanup registration.
#[export_name = "__joky_string_clone_v2"]
pub(crate) unsafe extern "C" fn clone(
    arena: *mut Arena,
    owner: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    let result = (|| {
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        let text = borrow(owner)?;
        Buffer::copy_bytes(arena, text.as_bytes())
    })();
    located(result, source, start, end)
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 2] {
    [
        ("__joky_string_literal_v2", literal as *const u8),
        ("__joky_string_clone_v2", clone as *const u8),
    ]
}

#[cfg(test)]
mod tests;
