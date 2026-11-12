//! Synchronous UTF-8 operations using the runtime target's lexical path rules.
use super::{located, Buffer};
use crate::runtime::{arena::Arena, owned_text};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    path::{Path, PathBuf},
    ptr::NonNull,
};

fn invalid() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::InvalidState,
        "Invalid lexical path query",
    )
}

/// Both handles are live immutable language strings for the duration of the call.
pub(crate) unsafe fn join(
    arena: &mut Arena,
    base: *const Buffer,
    child: *const Buffer,
) -> Result<NonNull<Buffer>, Error> {
    let base = owned_text::borrow(base)?;
    let child = owned_text::borrow(child)?;
    let capacity = base.len() + child.len() + 1;
    let _charge = arena.reserve_external(capacity)?;
    let mut result = PathBuf::new();
    result.try_reserve(capacity).map_err(|_| {
        Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Path allocation failed",
        )
    })?;
    result.push(base);
    result.push(child);
    Buffer::copy_bytes(arena, result.to_str().ok_or_else(invalid)?.as_bytes())
}

/// Part queries pack a UTF-8 byte range, or return MAX for an absent component.
/// The absolute-path query returns a boolean as an integer.
pub(crate) unsafe fn query(input: *const Buffer, part: u64) -> Result<u64, Error> {
    let text = owned_text::borrow(input)?;
    let path = Path::new(text);
    let component = match part {
        0 => return Ok(u64::from(path.is_absolute())),
        1 => path.parent().map(Path::as_os_str),
        2 => path.file_name(),
        3 => path.extension(),
        _ => return Err(invalid()),
    };
    let Some(component) = component else {
        return Ok(u64::MAX);
    };
    let component = component.to_str().ok_or_else(invalid)?;
    if component.is_empty() {
        return Ok(0);
    }
    let start = component
        .as_ptr()
        .addr()
        .checked_sub(text.as_ptr().addr())
        .ok_or_else(invalid)?;
    let end = start
        .checked_add(component.len())
        .filter(|&end| end <= text.len())
        .ok_or_else(invalid)?;
    let start = u32::try_from(start).map_err(|_| invalid())?;
    let end = u32::try_from(end).map_err(|_| invalid())?;
    Ok((u64::from(start) << 32) | u64::from(end))
}

#[export_name = "__joky_buffer_path_join"]
pub(super) unsafe extern "C" fn join_native(
    arena: *mut Arena,
    base: *const Buffer,
    child: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> *mut Buffer {
    located(
        join(&mut *arena, base, child).map(NonNull::as_ptr),
        source,
        start,
        end,
    )
}

#[export_name = "__joky_buffer_path_query"]
pub(super) unsafe extern "C" fn query_native(
    input: *const Buffer,
    part: u64,
    source: u64,
    start: usize,
    end: usize,
) -> u64 {
    located(query(input, part), source, start, end)
}
