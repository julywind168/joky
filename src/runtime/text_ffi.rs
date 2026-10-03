//! Versioned synchronous C text adapters. No language storage layout is public.
use super::{
    arena::{Arena, MAX_BYTES},
    buffer::Buffer,
    string::JokyText,
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use std::{ffi::c_void, ptr::NonNull};

pub const TEXT_FFI_ABI_V1: u32 = 1;
pub const TEXT_SPAN: u32 = 0;
pub const TEXT_C_STRING: u32 = 1;
pub const TEXT_BUFFER: u32 = 2;
pub const TEXT_SCALAR: u32 = 3;

/// Output is initialized by the runtime. A nonnull release token transfers one
/// cleanup obligation even on failure; it is never interpreted as text.
#[repr(C)]
#[derive(Default)]
pub struct TextFfiOutputV1 {
    pub data: *const u8,
    /// Span length, or readable C string bound including its terminator.
    pub byte_length: u64,
    pub release_token: *mut c_void,
    pub scalar: u64,
}

pub type TextFfiCallV1 = unsafe extern "C" fn(
    *mut c_void,
    *const JokyText,
    usize,
    *mut u8,
    usize,
    *mut TextFfiOutputV1,
) -> i32;
pub type TextFfiReleaseV1 = unsafe extern "C" fn(*mut c_void);

/// Immutable adapter description, copied at VM registration.
///
/// Callbacks run synchronously on the calling thread, must not unwind, retain
/// inputs/scratch/output slots, or reenter with conflicting accesses. Inputs
/// exclude the appended NUL from their lengths. `input_limits` specify the
/// maximum length representable by the real C prototype (e.g. INT_MAX).
/// Returned data must remain readable until release, including after `call`
/// returns. Borrowed output may alias inputs or scratch. C string outputs supply
/// a readable bound; the runtime scans only until the first NUL within it.
/// A release callback must not fail, unwind, or access language values. Every
/// nonnull token invokes it exactly once, including on callback failure.
/// `flags` is reserved and must be zero: asynchronous retention is unsupported.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TextFfiHostV1 {
    pub abi_version: u32,
    pub flags: u32,
    pub input_count: u32,
    pub input_kinds: [u32; 4],
    pub input_limits: [u64; 4],
    pub output_kind: u32,
    /// Maximum output bytes, including NUL for C strings; scratch capacity for
    /// buffer output. At most the task's 64 MiB budget.
    pub output_limit: u64,
    pub context: *mut c_void,
    pub call: Option<TextFfiCallV1>,
    pub release: Option<TextFfiReleaseV1>,
}

fn error(code: ErrorCode, message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, code, message)
}

impl TextFfiHostV1 {
    pub(crate) fn validate(&self, signature: &FunctionSignature) -> Result<(), Error> {
        crate::core::ffi::validate_text_signature(signature)?;
        self.validate_descriptor()?;
        if self.input_count as usize != signature.params.len()
            || (self.output_kind == TEXT_SCALAR) != (signature.ret != Type::String)
        {
            return Err(error(
                ErrorCode::AbiError,
                "Text FFI descriptor does not match its language signature",
            ));
        }
        Ok(())
    }

    fn validate_descriptor(&self) -> Result<(), Error> {
        if self.abi_version != TEXT_FFI_ABI_V1
            || self.flags != 0
            || self.call.is_none()
            || self.input_count > 4
            || self.output_kind > TEXT_SCALAR
            || self.output_limit > MAX_BYTES as u64
            || self.input_kinds[..self.input_count.min(4) as usize]
                .iter()
                .any(|&kind| kind > TEXT_C_STRING)
            || self.input_kinds[self.input_count.min(4) as usize..]
                .iter()
                .any(|&kind| kind != 0)
            || self.input_limits[self.input_count.min(4) as usize..]
                .iter()
                .any(|&limit| limit != 0)
            || (self.output_kind == TEXT_SCALAR && self.output_limit != 0)
        {
            return Err(error(
                ErrorCode::AbiError,
                "Invalid synchronous text FFI v1 descriptor",
            ));
        }
        Ok(())
    }
}

pub(crate) fn symbol(name: &str) -> String {
    format!("__joky_text_ffi_v1_{name}")
}

// RAII releases temporary allocations on every fallible path, independently of
// the generated caller's owning-argument cleanup.
struct Scratch {
    arena: *mut Arena,
    pointers: [Option<NonNull<u8>>; 5],
    count: usize,
}
impl Scratch {
    unsafe fn allocate(&mut self, size: usize) -> Result<*mut u8, Error> {
        let pointer = (*self.arena).allocate(size, 1)?;
        self.pointers[self.count] = Some(pointer);
        self.count += 1;
        Ok(pointer.as_ptr())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for pointer in self.pointers.iter_mut().filter_map(Option::take) {
            unsafe {
                (*self.arena).release(pointer);
            }
        }
    }
}

struct Release {
    token: *mut c_void,
    callback: Option<TextFfiReleaseV1>,
}
impl Drop for Release {
    fn drop(&mut self) {
        if !self.token.is_null() {
            if let Some(callback) = self.callback {
                unsafe {
                    callback(self.token);
                }
            }
        }
    }
}

/// Shared implementation for JIT, interpreter and relocatable AOT adapters.
/// # Safety
/// Arena and input owners must be live, exclusively managed by the calling
/// task. The descriptor and its callbacks obey TextFfiHostV1's contract.
pub(crate) unsafe fn invoke(
    arena: &mut Arena,
    host: &TextFfiHostV1,
    owners: &[*const Buffer],
) -> Result<u64, Error> {
    host.validate_descriptor()?;
    if owners.len() != host.input_count as usize {
        return Err(error(ErrorCode::AbiError, "Text FFI input count mismatch"));
    }
    let mut scratch = Scratch {
        arena,
        pointers: [None; 5],
        count: 0,
    };
    let mut inputs = [JokyText::default(); 4];
    for (index, &owner) in owners.iter().enumerate() {
        let text = super::owned_text::borrow(owner)?;
        if text.len() as u64 > host.input_limits[index] {
            return Err(error(
                ErrorCode::ResourceLimit,
                "Text length exceeds the C parameter range",
            ));
        }
        inputs[index] = JokyText::new(text);
        if host.input_kinds[index] == TEXT_C_STRING {
            if text.as_bytes().contains(&0) {
                return Err(error(
                    ErrorCode::InteriorNul,
                    "C string input contains an interior NUL",
                ));
            }
            let pointer =
                scratch.allocate(text.len().checked_add(1).ok_or_else(|| {
                    error(ErrorCode::ResourceLimit, "C string length overflow")
                })?)?;
            std::ptr::copy_nonoverlapping(text.as_ptr(), pointer, text.len());
            *pointer.add(text.len()) = 0;
            inputs[index].data = pointer;
        }
    }
    let capacity = if host.output_kind == TEXT_BUFFER {
        host.output_limit as usize
    } else {
        0
    };
    let buffer = if capacity > 0 {
        let pointer = scratch.allocate(capacity)?;
        // A short write cannot expose uninitialized bytes even if the host lies
        // about how much it initialized. Writes beyond capacity remain unsafe.
        std::ptr::write_bytes(pointer, 0, capacity);
        pointer
    } else {
        std::ptr::null_mut()
    };
    let mut output = TextFfiOutputV1::default();
    let status = super::error::checked(|| {
        (host.call.unwrap())(
            host.context,
            inputs.as_ptr(),
            owners.len(),
            buffer,
            capacity,
            &mut output,
        )
    });
    let _release = Release {
        token: output.release_token,
        callback: host.release,
    };
    if !output.release_token.is_null() && host.release.is_none() {
        return Err(error(
            ErrorCode::AbiError,
            "Text FFI returned a release token without a release callback",
        ));
    }
    if status? != 0 {
        return Err(error(
            ErrorCode::HostError,
            "Synchronous C text callback failed",
        ));
    }
    if host.output_kind == TEXT_SCALAR {
        return Ok(output.scalar);
    }
    if output.byte_length > host.output_limit {
        return Err(error(
            ErrorCode::ResourceLimit,
            "C text output exceeds its declared bound",
        ));
    }
    let mut length = output.byte_length as usize;
    let data = if host.output_kind == TEXT_BUFFER {
        buffer.cast_const()
    } else {
        output.data
    };
    if length > 0 && data.is_null() {
        return Err(error(ErrorCode::AbiError, "Null nonempty C text output"));
    }
    if host.output_kind == TEXT_C_STRING {
        // Do not construct a slice beyond the first terminator: the declared
        // bound need not all be read when a shorter C string is returned.
        let mut end = 0;
        while end < length && *data.add(end) != 0 {
            end += 1;
        }
        if end == length {
            return Err(error(
                ErrorCode::AbiError,
                "C string output has no terminator within its bound",
            ));
        }
        length = end;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, length)
    };
    std::str::from_utf8(bytes)
        .map_err(|_| error(ErrorCode::InvalidUtf8, "C text output is not UTF-8"))?;
    Ok(Buffer::copy_bytes(&mut *scratch.arena, bytes)?.as_ptr() as u64)
}

/// # Safety
/// Compiler-only entry; all pointers follow invoke's contract and the owners
/// array has exactly count initialized pointers (zero permits null).
#[export_name = "__joky_text_ffi_call_v1"]
pub(crate) unsafe extern "C" fn call(
    arena: *mut Arena,
    host: *const TextFfiHostV1,
    owners: *const *const Buffer,
    count: usize,
    scalar: u32,
) -> u64 {
    let result = (|| {
        let arena = arena
            .as_mut()
            .ok_or_else(|| error(ErrorCode::AbiError, "Null text FFI arena"))?;
        let host = host
            .as_ref()
            .ok_or_else(|| error(ErrorCode::AbiError, "Null text FFI descriptor"))?;
        if (host.output_kind == TEXT_SCALAR) != (scalar != 0) {
            return Err(error(ErrorCode::AbiError, "Text FFI result kind mismatch"));
        }
        if count > 4 || (count != 0 && owners.is_null()) {
            return Err(error(ErrorCode::AbiError, "Invalid text FFI input array"));
        }
        invoke(
            arena,
            host,
            if count == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(owners, count)
            },
        )
    })();
    match result {
        Ok(value) => value,
        Err(error) => {
            super::error::record_error(error);
            0
        }
    }
}
