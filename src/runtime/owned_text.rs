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

/// Copy an explicitly described UTF-8 payload into task-owned storage.
/// # Safety
/// The arena is live and exclusive; view obeys string::borrow's contract.
/// Successful handles must enter generated cleanup before another fallible step.
#[export_name = "__joky_string_from_view_v2"]
pub(crate) unsafe extern "C" fn from_view(
    arena: *mut Arena,
    view: *const u8,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    let result = (|| {
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        let text = super::string::borrow(view)?;
        Buffer::copy_bytes(arena, text.as_bytes())
    })();
    located(result, source, start, end)
}

/// Format a float scalar with the locale-independent shortest spelling that
/// round-trips at the source width; NaN, infinities and negative zero keep
/// fixed spellings ("NaN", "inf", "-inf", "-0").
/// # Safety
/// `arena` is a live exclusive task arena. `bits` is the float's raw bit
/// pattern, zero-extended for 32-bit values. The returned handle requires
/// independent generated cleanup registration.
#[export_name = "__joky_float_to_text_v2"]
pub(crate) unsafe extern "C" fn float_to_text(
    arena: *mut Arena,
    bits: u64,
    width: i32,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    let result = (|| {
        let arena = arena
            .as_mut()
            .ok_or_else(|| abi_error("Null String task arena"))?;
        let text = if width == 32 {
            format!("{}", f32::from_bits(bits as u32))
        } else {
            format!("{}", f64::from_bits(bits))
        };
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

/// Describe a live owner without allocating or copying its payload.
/// # Safety
/// Owner obeys borrow's contract; output is live exclusive descriptor storage.
#[export_name = "__joky_string_view_v2"]
pub(crate) unsafe extern "C" fn view(owner: *const Buffer, output: *mut super::string::JokyText) {
    match borrow(owner) {
        Ok(text) => *output = super::string::JokyText::new(text),
        Err(error) => super::error::record_error(error),
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

/// Bounded, correctly rounded decimal conversion shared by every backend.
/// No allocation, ownership transfer or retained borrow. JSON grammar is checked
/// in source; invalid/oversized input yields NaN, overflow infinity, underflow
/// signed zero. The source wrapper maps nonfinite results to checked errors.
pub(crate) fn parse_float64(text: &str) -> f64 {
    if text.len() > 1024 {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// # Safety
/// `owner` obeys `borrow`'s contract. ABI errors use the ordinary runtime error
/// channel; returning bits keeps the internal C signature independent of floats.
#[export_name = "__joky_float_from_text_v1"]
pub(crate) unsafe extern "C" fn float_from_text(owner: *const Buffer) -> u64 {
    match borrow(owner) {
        Ok(text) => parse_float64(text).to_bits(),
        Err(error) => {
            super::error::record_error(error);
            f64::NAN.to_bits()
        }
    }
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 8] {
    [
        ("__joky_string_from_view_v2", from_view as *const u8),
        ("__joky_string_clone_v2", clone as *const u8),
        ("__joky_string_view_v2", view as *const u8),
        ("__joky_string_result_copy_v2", result_copy as *const u8),
        ("__joky_string_input_v2", input as *const u8),
        (
            "joky_string_result_free_v2",
            joky_string_result_free_v2 as *const u8,
        ),
        ("__joky_float_to_text_v2", float_to_text as *const u8),
        ("__joky_float_from_text_v1", float_from_text as *const u8),
    ]
}

#[cfg(test)]
mod tests;
