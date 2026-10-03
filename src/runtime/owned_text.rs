//! Internal owning UTF-8 storage for the String v2 migration.
//! Handles are ordinary UInt8 Buffers charged to the task arena. These helpers
//! are shared by checked language construction and explicit host text adapters.
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

/// Create a budgeted stable legacy input view in the task arena.
/// # Safety
/// The arena is exclusive and the owner obeys borrow's contract.
#[export_name = "__joky_string_legacy_view_v2"]
pub(crate) unsafe extern "C" fn legacy_view(arena: *mut Arena, owner: *const Buffer) -> *const u8 {
    let result = (|| {
        let text = borrow(owner)?;
        let size = text
            .len()
            .checked_add(8)
            .ok_or_else(|| abi_error("Legacy text length overflow"))?;
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        let data = arena.allocate(size, 8)?.as_ptr();
        data.cast::<u64>().write(text.len() as u64);
        std::ptr::copy_nonoverlapping(text.as_ptr(), data.add(8), text.len());
        Ok::<_, Error>(data.cast_const())
    })();
    match result {
        Ok(data) => data,
        Err(error) => {
            super::error::record_error(error);
            std::ptr::null()
        }
    }
}

/// The host owns these bytes independently of every language arena.
#[repr(C)]
#[derive(Debug)]
pub struct JokyOwnedTextV2 {
    pub data: *mut u8,
    pub byte_length: u64,
}
impl Default for JokyOwnedTextV2 {
    fn default() -> Self {
        Self {
            data: std::ptr::null_mut(),
            byte_length: 0,
        }
    }
}

pub(crate) fn copy_to_host(text: &str) -> Result<String, Error> {
    if text.len() > MAX_BYTES {
        return Err(abi_error("Host text exceeds the task byte limit"));
    }
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| {
        Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Host text allocation failed",
        )
    })?;
    result.push_str(text);
    Ok(result)
}

/// # Safety
/// Output is live exclusive storage; owner obeys borrow's contract.
#[export_name = "__joky_string_result_copy_v2"]
pub(crate) unsafe extern "C" fn result_copy(owner: *const Buffer, output: *mut JokyOwnedTextV2) {
    let result = (|| {
        let output = output
            .as_mut()
            .ok_or_else(|| abi_error("Null text result output"))?;
        *output = JokyOwnedTextV2::default();
        let text = copy_to_host(borrow(owner)?)?;
        *output = host_result(text);
        Ok::<_, Error>(())
    })();
    if let Err(error) = result {
        super::error::record_error(error);
    }
}

pub(crate) fn host_result(text: String) -> JokyOwnedTextV2 {
    let bytes = text.into_bytes().into_boxed_slice();
    if bytes.is_empty() {
        return JokyOwnedTextV2::default();
    }
    let byte_length = bytes.len() as u64;
    let data = Box::into_raw(bytes).cast::<u8>();
    JokyOwnedTextV2 { data, byte_length }
}

/// Release a v2 result using its matching runtime allocator, and zero the output.
/// # Safety
/// The structure is a live, exclusive successful v2 output, or is zeroed; no copies may be freed twice.
#[no_mangle]
pub unsafe extern "C" fn joky_string_result_free_v2(output: *mut JokyOwnedTextV2) {
    if let Some(output) = output.as_mut() {
        if !output.data.is_null() {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                output.data,
                output.byte_length as usize,
            )));
        }
        *output = JokyOwnedTextV2::default();
    }
}

/// # Safety
/// The data/length form a readable initialized host buffer when the bounded length is nonzero.
#[export_name = "__joky_string_input_v2"]
pub(crate) unsafe extern "C" fn input(
    arena: *mut Arena,
    data: *const u8,
    length: u64,
) -> *mut Buffer {
    let result = (|| {
        if length > MAX_BYTES as u64 {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Text input exceeds the task arena budget",
            ));
        }
        if length != 0 && data.is_null() {
            return Err(abi_error("Null text input"));
        }
        let bytes = if length == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(data, length as usize)
        };
        std::str::from_utf8(bytes).map_err(|_| abi_error("Text input is not UTF-8"))?;
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        Buffer::copy_bytes(arena, bytes)
    })();
    located(result, 0, 0, 0)
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 6] {
    [
        ("__joky_string_literal_v2", literal as *const u8),
        ("__joky_string_clone_v2", clone as *const u8),
        ("__joky_string_legacy_view_v2", legacy_view as *const u8),
        ("__joky_string_result_copy_v2", result_copy as *const u8),
        ("__joky_string_input_v2", input as *const u8),
        (
            "joky_string_result_free_v2",
            joky_string_result_free_v2 as *const u8,
        ),
    ]
}

#[cfg(test)]
mod tests;
