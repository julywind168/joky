//! String storage and string-related runtime ABI.

use super::managed::{allocate_object_with_padding, valid_header, ObjectHeader, RuntimeValueKind};
use unicode_segmentation::UnicodeSegmentation;

fn valid_string(object: *mut u8) -> Option<&'static ObjectHeader> {
    let header = unsafe { valid_header(object) }?;
    (header.kind == RuntimeValueKind::String as u8).then_some(header)
}

pub(crate) extern "C" fn jk_string_from_utf8(data: *const u8, length: usize) -> *mut u8 {
    if data.is_null() && length != 0 {
        return std::ptr::null_mut();
    }
    // The payload keeps a NUL terminator past `length` so it can be lent to C
    // as a `const char*` without copying; `payload_size` stays the UTF-8
    // length and every length-based operation ignores the terminator.
    let object = allocate_object_with_padding(RuntimeValueKind::String, length, 1, 1, None);
    if object.is_null() {
        return object;
    }
    if length != 0 {
        unsafe { std::ptr::copy_nonoverlapping(data, object, length) };
    }
    unsafe { *object.add(length) = 0 };
    object
}

pub(crate) extern "C" fn jk_string_len(object: *mut u8) -> usize {
    valid_string(object).map_or(0, |header| header.payload_size)
}

/// Debug guard behind `String.as_cstr()`: reject embedded NUL bytes, which
/// would otherwise truncate silently at the C boundary. Release builds do not
/// call this.
pub(crate) extern "C" fn jk_string_c_string_check(data: *const u8, length: usize) {
    if data.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    if let Some(offset) = bytes.iter().position(|byte| *byte == 0) {
        eprintln!("panic: String.as_c_string() found an embedded NUL byte at offset {offset}");
        std::process::abort();
    }
}

/// Copy a NUL-terminated C string into a fresh managed String. Returns null
/// for a null pointer or non-UTF-8 content; the fresh allocation restores its
/// own hidden terminator.
pub(crate) extern "C" fn jk_string_from_cstr(data: *const u8) -> *mut u8 {
    if data.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(data.cast()) }.to_bytes();
    if std::str::from_utf8(bytes).is_err() {
        return std::ptr::null_mut();
    }
    jk_string_from_utf8(bytes.as_ptr(), bytes.len())
}

fn show_value(value: impl ToString) -> *mut u8 {
    let value = value.to_string();
    jk_string_from_utf8(value.as_ptr(), value.len())
}

pub(crate) extern "C" fn jk_debug_string(data: *const u8, length: usize) -> *mut u8 {
    let text = if length == 0 {
        ""
    } else {
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(data, length) })
            .expect("Joky String contains UTF-8")
    };
    show_value(format!("{text:?}"))
}

pub(crate) extern "C" fn jk_show_i64(value: i64) -> *mut u8 {
    show_value(value)
}

pub(crate) extern "C" fn jk_show_u64(value: u64) -> *mut u8 {
    show_value(value)
}

pub(crate) extern "C" fn jk_show_f64(value: f64) -> *mut u8 {
    show_value(value)
}

pub(crate) extern "C" fn jk_show_bool(value: u8) -> *mut u8 {
    show_value(value != 0)
}

pub(crate) extern "C" fn jk_string_concat(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> *mut u8 {
    if valid_string(left).is_none() || valid_string(right).is_none() {
        return std::ptr::null_mut();
    }
    let length = match left_length.checked_add(right_length) {
        Some(length) => length,
        None => return std::ptr::null_mut(),
    };
    let object = allocate_object_with_padding(RuntimeValueKind::String, length, 1, 1, None);
    if object.is_null() {
        return object;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(left, object, left_length);
        std::ptr::copy_nonoverlapping(right, object.add(left_length), right_length);
        *object.add(length) = 0;
    }
    object
}

pub(crate) extern "C" fn jk_string_eq(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> u8 {
    if left_length != right_length || valid_string(left).is_none() || valid_string(right).is_none()
    {
        return 0;
    }
    if left_length == 0 {
        return 1;
    }
    unsafe {
        (std::slice::from_raw_parts(left, left_length)
            == std::slice::from_raw_parts(right, right_length)) as u8
    }
}

/// Lexicographic comparison of valid UTF-8 byte sequences (including embedded NUL).
pub(crate) extern "C" fn jk_string_compare(
    left: *mut u8,
    left_length: usize,
    right: *mut u8,
    right_length: usize,
) -> i32 {
    let left = string_bytes(left, left_length).expect("valid comparison operand");
    let right = string_bytes(right, right_length).expect("valid comparison operand");
    match left.cmp(right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn string_text(object: *mut u8, length: usize) -> Option<&'static str> {
    if length == 0 {
        return Some("");
    }
    let bytes = string_bytes(object, length)?;
    std::str::from_utf8(bytes).ok()
}

fn string_bytes(object: *mut u8, length: usize) -> Option<&'static [u8]> {
    let header = valid_string(object)?;
    (header.payload_size == length).then(|| unsafe { std::slice::from_raw_parts(object, length) })
}

pub(crate) extern "C" fn jk_string_starts_with(
    object: *mut u8,
    object_length: usize,
    prefix: *mut u8,
    prefix_length: usize,
) -> u8 {
    let Some(value) = string_bytes(object, object_length) else {
        return 0;
    };
    let Some(prefix) = string_bytes(prefix, prefix_length) else {
        return 0;
    };
    value.starts_with(prefix) as u8
}

pub(crate) extern "C" fn jk_string_ends_with(
    object: *mut u8,
    object_length: usize,
    suffix: *mut u8,
    suffix_length: usize,
) -> u8 {
    let Some(value) = string_bytes(object, object_length) else {
        return 0;
    };
    let Some(suffix) = string_bytes(suffix, suffix_length) else {
        return 0;
    };
    value.ends_with(suffix) as u8
}

pub(crate) extern "C" fn jk_string_contains(
    object: *mut u8,
    object_length: usize,
    needle: *mut u8,
    needle_length: usize,
) -> u8 {
    let Some(value) = string_bytes(object, object_length) else {
        return 0;
    };
    let Some(needle) = string_bytes(needle, needle_length) else {
        return 0;
    };
    (needle.is_empty() || value.windows(needle.len()).any(|window| window == needle)) as u8
}

pub(crate) extern "C" fn jk_string_scalar_count(object: *mut u8, length: usize) -> usize {
    let Some(value) = string_bytes(object, length) else {
        return 0;
    };
    std::str::from_utf8(value)
        .map(|value| value.chars().count())
        .unwrap_or(0)
}

pub(crate) extern "C" fn jk_string_grapheme_count(object: *mut u8, length: usize) -> usize {
    let Some(value) = string_bytes(object, length) else {
        return 0;
    };
    std::str::from_utf8(value)
        .map(|value| value.graphemes(true).count())
        .unwrap_or(0)
}

pub(crate) extern "C" fn jk_string_is_ascii(object: *mut u8, length: usize) -> u8 {
    string_bytes(object, length)
        .map(|value| value.is_ascii())
        .unwrap_or(false) as u8
}

pub(crate) extern "C" fn jk_string_trim(object: *mut u8, length: usize) -> *mut u8 {
    let Some(value) = string_bytes(object, length) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = std::str::from_utf8(value) else {
        return std::ptr::null_mut();
    };
    let value = value.trim().as_bytes();
    jk_string_from_utf8(value.as_ptr(), value.len())
}

pub(crate) extern "C" fn jk_string_to_upper(object: *mut u8, length: usize) -> *mut u8 {
    let Some(value) = string_bytes(object, length) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = std::str::from_utf8(value) else {
        return std::ptr::null_mut();
    };
    let value = value.to_uppercase();
    jk_string_from_utf8(value.as_ptr(), value.len())
}

pub(crate) extern "C" fn jk_string_split(
    object: *mut u8,
    length: usize,
    separator: *mut u8,
    separator_length: usize,
) -> *mut u8 {
    let Some(text) = string_text(object, length) else {
        return std::ptr::null_mut();
    };
    let Some(separator) = string_text(separator, separator_length) else {
        return std::ptr::null_mut();
    };
    let pieces = if separator.is_empty() {
        text.char_indices()
            .map(|(index, scalar)| &text[index..index + scalar.len_utf8()])
            .collect::<Vec<_>>()
    } else {
        text.split(separator).collect::<Vec<_>>()
    };
    let mut list = std::ptr::null_mut();
    for piece in pieces.into_iter().rev() {
        let string = jk_string_from_utf8(piece.as_ptr(), piece.len());
        if string.is_null() {
            super::managed::jk_drop(list);
            return std::ptr::null_mut();
        }
        let words = [string as usize as u64, piece.len() as u64];
        let masks = [1_u64];
        list = super::list::jk_list_cons(words.as_ptr(), 2, masks.as_ptr(), 1, list);
        if list.is_null() {
            return std::ptr::null_mut();
        }
    }
    list
}

pub(crate) extern "C" fn jk_string_replace(
    object: *mut u8,
    length: usize,
    from: *mut u8,
    from_length: usize,
    to: *mut u8,
    to_length: usize,
) -> *mut u8 {
    let Some(text) = string_text(object, length) else {
        return std::ptr::null_mut();
    };
    let Some(from) = string_text(from, from_length) else {
        return std::ptr::null_mut();
    };
    let Some(to) = string_text(to, to_length) else {
        return std::ptr::null_mut();
    };
    let value = text.replace(from, to);
    jk_string_from_utf8(value.as_ptr(), value.len())
}

pub(crate) extern "C" fn jk_string_get_byte(
    object: *mut u8,
    length: usize,
    index: usize,
    output: *mut u8,
) -> u8 {
    if output.is_null() || index >= length {
        return 0;
    }
    let Some(bytes) = string_bytes(object, length) else {
        return 0;
    };
    unsafe { *output = bytes[index] };
    1
}

pub(crate) extern "C" fn jk_string_slice(
    object: *mut u8,
    length: usize,
    start: usize,
    end: usize,
) -> *mut u8 {
    let Some(text) = string_text(object, length) else {
        return std::ptr::null_mut();
    };
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return std::ptr::null_mut();
    }
    let slice = &text[start..end];
    jk_string_from_utf8(slice.as_ptr(), slice.len())
}

pub(crate) extern "C" fn jk_string_to_lower(object: *mut u8, length: usize) -> *mut u8 {
    let Some(value) = string_bytes(object, length) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = std::str::from_utf8(value) else {
        return std::ptr::null_mut();
    };
    let value = value.to_lowercase();
    jk_string_from_utf8(value.as_ptr(), value.len())
}

fn parse_string<T: std::str::FromStr>(object: *mut u8, length: usize) -> Option<T> {
    std::str::from_utf8(string_bytes(object, length)?)
        .ok()?
        .parse()
        .ok()
}

macro_rules! numeric_parsers {
    ($($name:ident: $ty:ty),* $(,)?) => {
        $(pub(crate) extern "C" fn $name(object: *mut u8, length: usize, output: *mut $ty) -> u8 {
            let Some(value) = parse_string::<$ty>(object, length) else {
                return 0;
            };
            unsafe { output.write(value) };
            1
        })*
    };
}

numeric_parsers! {
    jk_string_parse_i8: i8,
    jk_string_parse_i16: i16,
    jk_string_parse_i32: i32,
    jk_string_parse_i64: i64,
    jk_string_parse_u8: u8,
    jk_string_parse_u16: u16,
    jk_string_parse_u32: u32,
    jk_string_parse_u64: u64,
    jk_string_parse_f32: f32,
    jk_string_parse_f64: f64,
}

pub(crate) extern "C" fn jk_string_parse_bool(
    object: *mut u8,
    length: usize,
    output: *mut u8,
) -> u8 {
    let Some(value) = parse_string::<bool>(object, length) else {
        return 0;
    };
    unsafe { output.write(u8::from(value)) };
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_parser<T: Copy + std::fmt::Debug + PartialEq>(
        parser: extern "C" fn(*mut u8, usize, *mut T) -> u8,
        text: &[u8],
        expected: T,
        sentinel: T,
    ) {
        let mut output = [sentinel; 3];
        let object = jk_string_from_utf8(text.as_ptr(), text.len());
        assert_eq!(parser(object, text.len(), &mut output[1]), 1);
        assert_eq!(output, [sentinel, expected, sentinel]);
        output[1] = sentinel;
        assert_eq!(parser(object, text.len() + 1, &mut output[1]), 0);
        assert_eq!(output, [sentinel; 3]);
        super::super::managed::jk_drop(object);

        for invalid in [b"".as_slice(), b"bad", &[0xff]] {
            let object = jk_string_from_utf8(invalid.as_ptr(), invalid.len());
            assert_eq!(parser(object, invalid.len(), &mut output[1]), 0);
            assert_eq!(output, [sentinel; 3]);
            super::super::managed::jk_drop(object);
        }
        assert_eq!(parser(std::ptr::null_mut(), 0, &mut output[1]), 0);
        assert_eq!(output, [sentinel; 3]);
    }

    #[test]
    fn string_parsers_write_only_the_target_width_and_only_on_success() {
        check_parser(jk_string_parse_i8, b"-128", -128, 42);
        check_parser(jk_string_parse_i16, b"-32768", -32768, 42);
        check_parser(jk_string_parse_i32, b"-2147483648", i32::MIN, 42);
        check_parser(jk_string_parse_i64, b"-9223372036854775808", i64::MIN, 42);
        check_parser(jk_string_parse_u8, b"255", u8::MAX, 42);
        check_parser(jk_string_parse_u16, b"65535", u16::MAX, 42);
        check_parser(jk_string_parse_u32, b"4294967295", u32::MAX, 42);
        check_parser(jk_string_parse_u64, b"18446744073709551615", u64::MAX, 42);
        check_parser(jk_string_parse_f32, b"1.25", 1.25, 42.0);
        check_parser(jk_string_parse_f64, b"1.25", 1.25, 42.0);
        check_parser(jk_string_parse_bool, b"true", 1, 0xa5);
        check_parser(jk_string_parse_bool, b"false", 0, 0xa5);
    }
}
