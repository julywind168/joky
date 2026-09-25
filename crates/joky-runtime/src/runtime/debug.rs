//! Byte/duration formatting and immutable ancestry snapshots for Debug.
//! Managed Bytes keep ancestry alive across dynamic call continuations.

use super::bytes::jk_bytes_from_data;
use super::managed::{valid_header, RuntimeValueKind};
use super::string::jk_string_from_utf8;
use std::fmt::Write;

const MAX_DEPTH: usize = 64;
const WORD: usize = std::mem::size_of::<usize>();

fn bytes_payload(path: *mut u8) -> &'static [u8] {
    let header = unsafe { valid_header(path) }.expect("Debug requires managed Bytes");
    assert_eq!(header.kind, RuntimeValueKind::Bytes as u8);
    unsafe { std::slice::from_raw_parts(path, header.payload_size) }
}

pub(crate) extern "C" fn jk_debug_path(parent: *mut u8, object: *const u8) -> *mut u8 {
    let parent = bytes_payload(parent);
    let ancestors = parent.get(1..).unwrap_or_default();
    let address = (object as usize).to_ne_bytes();
    let status = if ancestors.len() / WORD >= MAX_DEPTH {
        2
    } else if !object.is_null() && ancestors.as_chunks::<WORD>().0.contains(&address) {
        1
    } else {
        0
    };
    let mut bytes = Vec::with_capacity(1 + ancestors.len() + WORD);
    bytes.push(status);
    bytes.extend_from_slice(ancestors);
    bytes.extend_from_slice(&address);
    jk_bytes_from_data(bytes.as_ptr(), bytes.len())
}

pub(crate) extern "C" fn jk_debug_path_status(path: *mut u8) -> i32 {
    i32::from(
        *bytes_payload(path)
            .first()
            .expect("Debug path must have a status"),
    )
}

pub(crate) extern "C" fn jk_debug_native_id(value: *mut u8) -> u64 {
    let header = unsafe { valid_header(value) }.expect("Debug requires a live native resource");
    assert_eq!(header.kind, RuntimeValueKind::NativeHandle as u8);
    header.debug_id
}

/// Borrow the bytes; generated code releases its argument reference.
pub(crate) extern "C" fn jk_debug_bytes(value: *mut u8) -> *mut u8 {
    let bytes = bytes_payload(value);
    let mut text = String::from("Bytes[");
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            text.push_str(", ");
        }
        write!(text, "0x{byte:02x}").expect("format into String");
    }
    text.push(']');
    jk_string_from_utf8(text.as_ptr(), text.len())
}

pub(crate) extern "C" fn jk_debug_duration(millis: u64) -> *mut u8 {
    let text = format!("{millis}ms");
    jk_string_from_utf8(text.as_ptr(), text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        bytes::jk_bytes_new,
        managed::{jk_alloc_native_handle, jk_drop},
    };

    #[test]
    fn paths_detect_cycles_limit_depth_and_keep_siblings_independent() {
        let root = jk_bytes_new();
        let value = jk_alloc_native_handle(1, 1, None);
        let first = jk_debug_path(root, value);
        assert_eq!(jk_debug_path_status(first), 0);
        let cycle = jk_debug_path(first, value);
        assert_eq!(jk_debug_path_status(cycle), 1);
        let sibling = jk_debug_path(root, value);
        assert_eq!(jk_debug_path_status(sibling), 0);
        for pointer in [first, cycle, sibling] {
            jk_drop(pointer);
        }
        let mut path = root;
        for _ in 0..MAX_DEPTH {
            let next = jk_debug_path(path, std::ptr::null());
            jk_drop(path);
            assert_eq!(jk_debug_path_status(next), 0);
            path = next;
        }
        let limited = jk_debug_path(path, std::ptr::null());
        assert_eq!(jk_debug_path_status(limited), 2);
        for pointer in [path, limited, value] {
            jk_drop(pointer);
        }
    }

    fn assert_text(value: *mut u8, expected: &str) {
        let length = super::super::string::jk_string_len(value);
        let bytes = unsafe { std::slice::from_raw_parts(value, length) };
        assert_eq!(bytes, expected.as_bytes());
        jk_drop(value);
    }

    #[test]
    fn byte_debug_formats_binary_data_without_utf8_conversion() {
        let data = [0, 1, 15, 16, 127, 128, 255];
        let bytes = jk_bytes_from_data(data.as_ptr(), data.len());
        for _ in 0..2 {
            assert_text(
                jk_debug_bytes(bytes),
                "Bytes[0x00, 0x01, 0x0f, 0x10, 0x7f, 0x80, 0xff]",
            );
        }
        assert_eq!(bytes_payload(bytes), data);
        jk_drop(bytes);
        let empty = jk_bytes_new();
        assert_text(jk_debug_bytes(empty), "Bytes[]");
        jk_drop(empty);
    }

    #[test]
    fn duration_debug_keeps_the_full_unsigned_millisecond_value() {
        for (value, expected) in [
            (0, "0ms"),
            (1500, "1500ms"),
            (u64::MAX, "18446744073709551615ms"),
        ] {
            assert_text(jk_debug_duration(value), expected);
        }
    }

    #[test]
    fn native_ids_are_stable_and_distinguish_allocations() {
        let first = jk_alloc_native_handle(1, 1, None);
        let second = jk_alloc_native_handle(1, 1, None);
        let id = jk_debug_native_id(first);
        assert_ne!(id, 0);
        assert_eq!(jk_debug_native_id(first), id);
        assert_ne!(jk_debug_native_id(second), id);
        jk_drop(first);
        let third = jk_alloc_native_handle(1, 1, None);
        assert_ne!(jk_debug_native_id(third), id);
        jk_drop(second);
        jk_drop(third);
    }
}
