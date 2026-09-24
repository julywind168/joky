//! Contiguous immutable and mutable byte storage.

use std::alloc::{self, Layout};
use std::mem::{align_of, size_of};

use super::managed::{
    allocate_object, jk_alloc_object, jk_drop, jk_dup, valid_header, ObjectHeader, RuntimeValueKind,
};
use super::string::jk_string_from_utf8;

#[repr(C)]
#[derive(Clone, Copy)]
struct MutBytesNode {
    length: usize,
    capacity: usize,
    data: *mut u8,
}

fn bytes_header(object: *mut u8) -> Option<&'static ObjectHeader> {
    let header = unsafe { valid_header(object) }?;
    (header.kind == RuntimeValueKind::Bytes as u8).then_some(header)
}

unsafe fn mut_bytes_node(object: *mut u8) -> Option<MutBytesNode> {
    let header = valid_header(object)?;
    mut_bytes_node_from_header(object, header)
}

unsafe fn mut_bytes_node_from_header(
    object: *mut u8,
    header: &ObjectHeader,
) -> Option<MutBytesNode> {
    if header.kind != RuntimeValueKind::MutBytes as u8
        || header.payload_size != size_of::<MutBytesNode>()
    {
        return None;
    }
    let node = object.cast::<MutBytesNode>().read();
    if node.length > node.capacity || (node.capacity != 0 && node.data.is_null()) {
        return None;
    }
    Some(node)
}

fn buffer_layout(capacity: usize) -> Option<Layout> {
    Layout::array::<u8>(capacity).ok()
}

fn allocate_buffer(capacity: usize) -> *mut u8 {
    if capacity == 0 {
        return std::ptr::null_mut();
    }
    let Some(layout) = buffer_layout(capacity) else {
        return std::ptr::null_mut();
    };
    unsafe { alloc::alloc(layout) }
}

fn deallocate_buffer(data: *mut u8, capacity: usize) {
    if data.is_null() || capacity == 0 {
        return;
    }
    let Some(layout) = buffer_layout(capacity) else {
        return;
    };
    unsafe { alloc::dealloc(data, layout) };
}

fn reserve_total(object: *mut u8, mut node: MutBytesNode, required: usize) -> Option<MutBytesNode> {
    if required <= node.capacity {
        return Some(node);
    }
    let new_capacity = node
        .capacity
        .max(8)
        .checked_next_power_of_two()?
        .max(required);
    let new_data = allocate_buffer(new_capacity);
    if new_data.is_null() {
        return None;
    }
    if node.length != 0 {
        unsafe { std::ptr::copy_nonoverlapping(node.data, new_data, node.length) };
    }
    deallocate_buffer(node.data, node.capacity);
    node.data = new_data;
    node.capacity = new_capacity;
    unsafe { object.cast::<MutBytesNode>().write(node) };
    Some(node)
}

pub(crate) unsafe fn drop_mut_bytes(object: *mut u8, header: &ObjectHeader) {
    let Some(node) = mut_bytes_node_from_header(object, header) else {
        return;
    };
    deallocate_buffer(node.data, node.capacity);
}

pub(crate) extern "C" fn jk_bytes_from_data(data: *const u8, length: usize) -> *mut u8 {
    if data.is_null() && length != 0 {
        return std::ptr::null_mut();
    }
    let object = jk_alloc_object(RuntimeValueKind::Bytes as u8, length, 1);
    if object.is_null() {
        return object;
    }
    if length != 0 {
        unsafe { std::ptr::copy_nonoverlapping(data, object, length) };
    }
    object
}

pub(crate) extern "C" fn jk_bytes_new() -> *mut u8 {
    jk_bytes_from_data(std::ptr::null(), 0)
}

pub(crate) extern "C" fn jk_bytes_from_string(object: *mut u8, length: usize) -> *mut u8 {
    let header = unsafe { valid_header(object) };
    if !header.is_some_and(|header| {
        header.kind == RuntimeValueKind::String as u8 && header.payload_size == length
    }) {
        return std::ptr::null_mut();
    }
    jk_bytes_from_data(object, length)
}

/// Borrowed view valid only while the `Bytes` object remains alive.
pub(crate) extern "C" fn jk_bytes_data(object: *mut u8) -> *const u8 {
    bytes_header(object)
        .map(|_| object.cast_const())
        .unwrap_or(std::ptr::null())
}

/// Compares immutable byte contents without consuming either borrowed handle.
pub(crate) extern "C" fn jk_bytes_eq(left: *mut u8, right: *mut u8) -> u8 {
    let (Some(left_header), Some(right_header)) = (bytes_header(left), bytes_header(right)) else {
        return 0;
    };
    if left_header.payload_size != right_header.payload_size {
        return 0;
    }
    unsafe {
        (std::slice::from_raw_parts(left, left_header.payload_size)
            == std::slice::from_raw_parts(right, right_header.payload_size)) as u8
    }
}

/// Unsigned byte lexicographic ordering, borrowing both handles.
pub(crate) extern "C" fn jk_bytes_compare(left: *mut u8, right: *mut u8) -> i32 {
    let left_header = bytes_header(left).expect("valid comparison operand");
    let right_header = bytes_header(right).expect("valid comparison operand");
    let left = unsafe { std::slice::from_raw_parts(left, left_header.payload_size) };
    let right = unsafe { std::slice::from_raw_parts(right, right_header.payload_size) };
    match left.cmp(right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

pub(crate) extern "C" fn jk_bytes_length(object: *mut u8) -> usize {
    bytes_header(object).map_or(0, |header| header.payload_size)
}

pub(crate) extern "C" fn jk_bytes_is_empty(object: *mut u8) -> u8 {
    (bytes_header(object).is_some_and(|header| header.payload_size == 0)) as u8
}

pub(crate) extern "C" fn jk_bytes_get(object: *mut u8, index: usize, output: *mut u8) -> u8 {
    if output.is_null() {
        return 0;
    }
    let Some(header) = bytes_header(object) else {
        return 0;
    };
    if index >= header.payload_size {
        return 0;
    }
    unsafe { output.write(*object.add(index)) };
    1
}

pub(crate) extern "C" fn jk_bytes_slice(object: *mut u8, start: usize, length: usize) -> *mut u8 {
    let Some(header) = bytes_header(object) else {
        return std::ptr::null_mut();
    };
    let Some(end) = start.checked_add(length) else {
        return std::ptr::null_mut();
    };
    if end > header.payload_size {
        return std::ptr::null_mut();
    }
    jk_bytes_from_data(unsafe { object.add(start) }, length)
}

pub(crate) extern "C" fn jk_bytes_concat(left: *mut u8, right: *mut u8) -> *mut u8 {
    let Some(left_header) = bytes_header(left) else {
        return std::ptr::null_mut();
    };
    let Some(right_header) = bytes_header(right) else {
        return std::ptr::null_mut();
    };
    let Some(length) = left_header
        .payload_size
        .checked_add(right_header.payload_size)
    else {
        return std::ptr::null_mut();
    };
    let object = jk_alloc_object(RuntimeValueKind::Bytes as u8, length, 1);
    if object.is_null() {
        return object;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(left, object, left_header.payload_size);
        std::ptr::copy_nonoverlapping(
            right,
            object.add(left_header.payload_size),
            right_header.payload_size,
        );
    }
    object
}

pub(crate) extern "C" fn jk_bytes_to_string(object: *mut u8) -> *mut u8 {
    let Some(header) = bytes_header(object) else {
        return std::ptr::null_mut();
    };
    let bytes = unsafe { std::slice::from_raw_parts(object, header.payload_size) };
    if std::str::from_utf8(bytes).is_err() {
        return std::ptr::null_mut();
    }
    jk_string_from_utf8(bytes.as_ptr(), bytes.len())
}

pub(crate) extern "C" fn jk_mut_bytes_with_capacity(capacity: usize) -> *mut u8 {
    let data = allocate_buffer(capacity);
    if capacity != 0 && data.is_null() {
        return std::ptr::null_mut();
    }
    let object = jk_alloc_object(
        RuntimeValueKind::MutBytes as u8,
        size_of::<MutBytesNode>(),
        align_of::<MutBytesNode>(),
    );
    if object.is_null() {
        deallocate_buffer(data, capacity);
        return object;
    }
    unsafe {
        object.cast::<MutBytesNode>().write(MutBytesNode {
            length: 0,
            capacity,
            data,
        });
    }
    object
}

pub(crate) extern "C" fn jk_mut_bytes_new() -> *mut u8 {
    jk_mut_bytes_with_capacity(0)
}

pub(crate) extern "C" fn jk_mut_bytes_from_bytes(bytes: *mut u8) -> *mut u8 {
    let Some(header) = bytes_header(bytes) else {
        return std::ptr::null_mut();
    };
    let object = jk_mut_bytes_with_capacity(header.payload_size);
    if object.is_null() {
        return object;
    }
    if header.payload_size != 0 {
        let mut node = unsafe { mut_bytes_node(object).expect("new MutBytes") };
        unsafe { std::ptr::copy_nonoverlapping(bytes, node.data, header.payload_size) };
        node.length = header.payload_size;
        unsafe { object.cast::<MutBytesNode>().write(node) };
    }
    object
}

pub(crate) extern "C" fn jk_mut_bytes_length(object: *mut u8) -> usize {
    unsafe { mut_bytes_node(object) }.map_or(0, |node| node.length)
}

pub(crate) extern "C" fn jk_mut_bytes_capacity(object: *mut u8) -> usize {
    unsafe { mut_bytes_node(object) }.map_or(0, |node| node.capacity)
}

pub(crate) extern "C" fn jk_mut_bytes_reserve(object: *mut u8, additional: usize) -> u8 {
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    let Some(required) = node.length.checked_add(additional) else {
        return 0;
    };
    reserve_total(object, node, required).is_some() as u8
}

pub(crate) extern "C" fn jk_mut_bytes_push(object: *mut u8, value: u8) -> u8 {
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    let Some(required) = node.length.checked_add(1) else {
        return 0;
    };
    let Some(mut node) = reserve_total(object, node, required) else {
        return 0;
    };
    unsafe { node.data.add(node.length).write(value) };
    node.length = required;
    unsafe { object.cast::<MutBytesNode>().write(node) };
    1
}

pub(crate) extern "C" fn jk_mut_bytes_get(object: *mut u8, index: usize, output: *mut u8) -> u8 {
    if output.is_null() {
        return 0;
    }
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    if index >= node.length {
        return 0;
    }
    unsafe { output.write(*node.data.add(index)) };
    1
}

pub(crate) extern "C" fn jk_mut_bytes_set(object: *mut u8, index: usize, value: u8) -> u8 {
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    if index >= node.length {
        return 0;
    }
    unsafe { node.data.add(index).write(value) };
    1
}

pub(crate) extern "C" fn jk_mut_bytes_pop(object: *mut u8, output: *mut u8) -> u8 {
    if output.is_null() {
        return 0;
    }
    let Some(mut node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    if node.length == 0 {
        return 0;
    }
    node.length -= 1;
    unsafe {
        output.write(*node.data.add(node.length));
        object.cast::<MutBytesNode>().write(node);
    }
    1
}

pub(crate) extern "C" fn jk_mut_bytes_clear(object: *mut u8) -> u8 {
    let Some(mut node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    node.length = 0;
    unsafe { object.cast::<MutBytesNode>().write(node) };
    1
}

pub(crate) extern "C" fn jk_mut_bytes_extend(object: *mut u8, bytes: *mut u8) -> u8 {
    let Some(source) = bytes_header(bytes) else {
        return 0;
    };
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return 0;
    };
    let Some(required) = node.length.checked_add(source.payload_size) else {
        return 0;
    };
    let Some(mut node) = reserve_total(object, node, required) else {
        return 0;
    };
    if source.payload_size != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes, node.data.add(node.length), source.payload_size)
        };
    }
    node.length = required;
    unsafe { object.cast::<MutBytesNode>().write(node) };
    1
}

pub(crate) extern "C" fn jk_mut_bytes_to_bytes(object: *mut u8) -> *mut u8 {
    let Some(node) = (unsafe { mut_bytes_node(object) }) else {
        return std::ptr::null_mut();
    };
    jk_bytes_from_data(node.data, node.length)
}

/// Shared cursor over an immutable Bytes payload: the handle reference plus a
/// byte offset. `advance` mutates the offset in place and hands back the same
/// object, so traversal performs no per-step allocation.
#[repr(C)]
struct BytesCursorNode {
    handle: *mut u8,
    offset: usize,
}

impl Drop for BytesCursorNode {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            jk_drop(self.handle);
        }
    }
}

unsafe fn cursor_node(object: *mut u8) -> Option<&'static mut BytesCursorNode> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::BytesCursor as u8
        || header.payload_size != size_of::<BytesCursorNode>()
    {
        return None;
    }
    unsafe { object.cast::<BytesCursorNode>().as_mut() }
}

unsafe extern "C" fn drop_bytes_cursor(pointer: *mut u8) {
    unsafe { pointer.cast::<BytesCursorNode>().drop_in_place() };
}

/// Borrows the caller's Bytes reference and retains an independent handle
/// for the cursor; O(1), copies nothing.
pub(crate) unsafe extern "C" fn jk_bytes_cursor_new(bytes: *mut u8) -> *mut u8 {
    if bytes_header(bytes).is_none() {
        return std::ptr::null_mut();
    }
    let handle = jk_dup(bytes);
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let pointer = allocate_object(
        RuntimeValueKind::BytesCursor,
        size_of::<BytesCursorNode>(),
        align_of::<BytesCursorNode>(),
        Some(drop_bytes_cursor),
    );
    if pointer.is_null() {
        jk_drop(handle);
        return pointer;
    }
    unsafe {
        pointer
            .cast::<BytesCursorNode>()
            .write(BytesCursorNode { handle, offset: 0 })
    };
    pointer
}

/// Consumes the caller's cursor reference. Returns the successor cursor (the
/// same object, offset advanced) with that reference now belonging to the
/// result, or null at exhaustion after releasing the cursor and its handle.
pub(crate) unsafe extern "C" fn jk_bytes_cursor_step(object: *mut u8, output: *mut u8) -> *mut u8 {
    let Some(node) = (unsafe { cursor_node(object) }) else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    let length = jk_bytes_length(node.handle);
    if node.offset >= length {
        jk_drop(object);
        return std::ptr::null_mut();
    }
    if !output.is_null() {
        unsafe { output.write(*node.handle.add(node.offset)) };
    }
    node.offset += 1;
    object
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::managed::jk_drop;

    #[test]
    fn bytes_ordering_uses_unsigned_lexicographic_contents() {
        let data: &[&[u8]] = &[&[], &[0], &[0, 128], &[0, 255], &[0, 255, 1], &[255]];
        let values = data
            .iter()
            .map(|value| jk_bytes_from_data(value.as_ptr(), value.len()))
            .collect::<Vec<_>>();
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                assert_eq!(
                    jk_bytes_compare(*left, *right),
                    if i < j {
                        -1
                    } else if i == j {
                        0
                    } else {
                        1
                    }
                );
            }
            let copy = jk_bytes_from_data(data[i].as_ptr(), data[i].len());
            assert_eq!(jk_bytes_compare(*left, copy), 0);
            assert_eq!(jk_bytes_length(*left), data[i].len());
            jk_drop(copy);
        }
        for value in values {
            jk_drop(value);
        }
    }

    #[test]
    fn bytes_equality_compares_raw_contents_and_borrows_handles() {
        let left = jk_bytes_from_data([0, 255, 128].as_ptr(), 3);
        let equal = jk_bytes_from_data([0, 255, 128].as_ptr(), 3);
        let different = jk_bytes_from_data([0, 255, 129].as_ptr(), 3);
        let shorter = jk_bytes_from_data([0, 255].as_ptr(), 2);
        let empty = jk_bytes_new();
        let other_empty = jk_bytes_new();
        assert_eq!(jk_bytes_eq(left, equal), 1);
        assert_eq!(jk_bytes_eq(left, left), 1);
        assert_eq!(jk_bytes_eq(left, different), 0);
        assert_eq!(jk_bytes_eq(left, shorter), 0);
        assert_eq!(jk_bytes_eq(empty, other_empty), 1);
        assert_eq!(jk_bytes_eq(empty, left), 0);
        assert_eq!(jk_bytes_eq(std::ptr::null_mut(), left), 0);
        assert_eq!(jk_bytes_length(left), 3);
        for value in [left, equal, different, shorter, empty, other_empty] {
            jk_drop(value);
        }
    }

    #[test]
    fn immutable_bytes_slice_concat_and_utf8() {
        let left = jk_bytes_from_data(b"hello".as_ptr(), 5);
        let right = jk_bytes_slice(left, 1, 3);
        let joined = jk_bytes_concat(right, right);
        assert_eq!(jk_bytes_length(joined), 6);
        let value = unsafe { std::slice::from_raw_parts(jk_bytes_data(joined), 6) };
        assert_eq!(value, b"ellell");
        assert!(!jk_bytes_to_string(joined).is_null());
        jk_drop(left);
        jk_drop(right);
        jk_drop(joined);
    }

    #[test]
    fn mutable_bytes_grow_edit_and_freeze() {
        let value = jk_mut_bytes_with_capacity(1);
        assert_eq!(jk_mut_bytes_push(value, 1), 1);
        assert_eq!(jk_mut_bytes_push(value, 2), 1);
        assert_eq!(jk_mut_bytes_set(value, 0, 9), 1);
        let mut popped = 0;
        assert_eq!(jk_mut_bytes_pop(value, &mut popped), 1);
        assert_eq!(popped, 2);
        let frozen = jk_mut_bytes_to_bytes(value);
        assert_eq!(unsafe { *jk_bytes_data(frozen) }, 9);
        jk_drop(frozen);
        jk_drop(value);
    }

    #[test]
    fn bytes_cursor_walks_in_place_and_releases_on_exhaustion() {
        let bytes = jk_bytes_from_data([1u8, 2, 3].as_ptr(), 3);
        // The cursor retains its own reference; release the local one.
        let mut cursor = unsafe { jk_bytes_cursor_new(bytes) };
        jk_drop(bytes);
        assert!(!cursor.is_null());
        let mut byte = 0u8;
        for expected in [1u8, 2, 3] {
            let next = unsafe { jk_bytes_cursor_step(cursor, &mut byte) };
            assert_eq!(byte, expected);
            assert_eq!(next, cursor);
            cursor = next;
        }
        // Exhaustion consumes the cursor and its handle reference.
        assert!(unsafe { jk_bytes_cursor_step(cursor, &mut byte) }.is_null());
        let empty = jk_bytes_from_data(std::ptr::null(), 0);
        let cursor = unsafe { jk_bytes_cursor_new(empty) };
        jk_drop(empty);
        assert!(unsafe { jk_bytes_cursor_step(cursor, &mut byte) }.is_null());
    }
}
