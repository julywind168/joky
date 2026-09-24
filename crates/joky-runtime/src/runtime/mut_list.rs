//! Mutable list runtime implementation.
use super::managed::*;
use std::alloc::{self, Layout};
use std::mem::{align_of, size_of};
struct MutListNode {
    length: usize,
    capacity: usize,
    word_count: usize,
    mask_count: usize,
    data: *mut u64,
}

unsafe fn mut_list_node<'a>(object: *mut u8) -> Option<(MutListNode, &'a [u64], &'a [u64])> {
    let header = valid_header(object)?;
    mut_list_node_from_header(object, header)
}

unsafe fn mut_list_node_from_header<'a>(
    object: *mut u8,
    header: &ObjectHeader,
) -> Option<(MutListNode, &'a [u64], &'a [u64])> {
    if header.kind != RuntimeValueKind::MutList as u8
        || header.payload_size != size_of::<MutListNode>()
    {
        return None;
    }
    let node = object.cast::<MutListNode>().read();
    if node.word_count == 0
        || node.capacity < node.length
        || mask_count(node.word_count)? != node.mask_count
        || (node.length != 0 && node.data.is_null())
    {
        return None;
    }
    let words = node.length.checked_mul(node.word_count)?;
    let masks = node.length.checked_mul(node.mask_count)?;
    let data = node.data;
    if data.is_null() {
        return Some((node, &[], &[]));
    }
    let mask_data = unsafe { data.add(node.capacity * node.word_count) };
    Some((
        node,
        std::slice::from_raw_parts(data, words),
        std::slice::from_raw_parts(mask_data, masks),
    ))
}

pub(crate) unsafe fn drop_mut_list(object: *mut u8, header: &ObjectHeader) {
    let Some((node, words, masks)) = mut_list_node_from_header(object, header) else {
        return;
    };
    for index in 0..node.length {
        drop_list_words(
            &words[index * node.word_count..(index + 1) * node.word_count],
            &masks[index * node.mask_count..(index + 1) * node.mask_count],
        );
    }
    if !node.data.is_null() {
        dealloc_mut_list_buffer(
            node.data,
            node.capacity * (node.word_count + node.mask_count),
        );
    }
}

fn mut_list_layout(words: usize) -> Option<Layout> {
    Layout::array::<u64>(words).ok()
}

fn alloc_mut_list_buffer(words: usize) -> *mut u64 {
    let Some(layout) = mut_list_layout(words) else {
        return std::ptr::null_mut();
    };
    unsafe { alloc::alloc(layout).cast::<u64>() }
}

fn dealloc_mut_list_buffer(pointer: *mut u64, words: usize) {
    if pointer.is_null() {
        return;
    }
    let Some(layout) = mut_list_layout(words) else {
        return;
    };
    unsafe {
        alloc::dealloc(pointer.cast::<u8>(), layout);
    }
}

pub(crate) extern "C" fn jk_mut_list_new(word_count: usize, mask_count_value: usize) -> *mut u8 {
    if word_count == 0 || mask_count(word_count) != Some(mask_count_value) {
        return std::ptr::null_mut();
    }
    let node = MutListNode {
        length: 0,
        capacity: 0,
        word_count,
        mask_count: mask_count_value,
        data: std::ptr::null_mut(),
    };
    let object = jk_alloc_object(
        RuntimeValueKind::MutList as u8,
        size_of::<MutListNode>(),
        align_of::<MutListNode>(),
    );
    if object.is_null() {
        return object;
    }
    unsafe {
        object.cast::<MutListNode>().write(node);
    }
    object
}

pub(crate) extern "C" fn jk_mut_list_push(
    object: *mut u8,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
) -> u8 {
    if words.is_null() || masks.is_null() {
        return 0;
    }
    let Some((mut node, _, _)) = (unsafe { mut_list_node(object) }) else {
        return 0;
    };
    if node.word_count != word_count || node.mask_count != masks_count {
        return 0;
    }
    let input_words = unsafe { std::slice::from_raw_parts(words, word_count) };
    let input_masks = unsafe { std::slice::from_raw_parts(masks, masks_count) };
    let stride = node.word_count + node.mask_count;
    if node.length == node.capacity {
        let new_capacity = node.capacity.checked_mul(2).unwrap_or(0).max(4);
        let new_data = alloc_mut_list_buffer(new_capacity.checked_mul(stride).unwrap_or(0));
        if new_data.is_null() {
            return 0;
        }
        if !node.data.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(node.data, new_data, node.length * node.word_count);
                std::ptr::copy_nonoverlapping(
                    node.data.add(node.capacity * node.word_count),
                    new_data.add(new_capacity * node.word_count),
                    node.length * node.mask_count,
                );
            }
            dealloc_mut_list_buffer(node.data, node.capacity * stride);
        }
        node.data = new_data;
        node.capacity = new_capacity;
    }
    let word_offset = node.length * node.word_count;
    let mask_offset = node.capacity * node.word_count + node.length * node.mask_count;
    unsafe {
        std::ptr::copy_nonoverlapping(
            input_words.as_ptr(),
            node.data.add(word_offset),
            node.word_count,
        );
        std::ptr::copy_nonoverlapping(
            input_masks.as_ptr(),
            node.data.add(mask_offset),
            node.mask_count,
        );
    }
    node.length += 1;
    unsafe {
        object.cast::<MutListNode>().write(node);
    }
    1
}

pub(crate) extern "C" fn jk_mut_list_length(object: *mut u8) -> usize {
    unsafe {
        mut_list_node(object)
            .map(|(node, _, _)| node.length)
            .unwrap_or(0)
    }
}

pub(crate) extern "C" fn jk_mut_list_capacity(object: *mut u8) -> usize {
    unsafe {
        mut_list_node(object)
            .map(|(node, _, _)| node.capacity)
            .unwrap_or(0)
    }
}

pub(crate) extern "C" fn jk_mut_list_get(
    object: *mut u8,
    index: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    if output.is_null() {
        return 0;
    }
    let Some((node, words, masks)) = (unsafe { mut_list_node(object) }) else {
        return 0;
    };
    if index >= node.length || output_count != node.word_count {
        return 0;
    }
    let start = index * node.word_count;
    let value = &words[start..start + node.word_count];
    let element_masks = &masks[index * node.mask_count..(index + 1) * node.mask_count];
    let Some(cloned) = clone_managed_words(value, element_masks, 0) else {
        return 0;
    };
    unsafe {
        std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count);
    }
    1
}

pub(crate) extern "C" fn jk_mut_list_set(
    object: *mut u8,
    index: usize,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
) -> u8 {
    if words.is_null() || masks.is_null() {
        return 0;
    }
    let Some((node, all_words, all_masks)) = (unsafe { mut_list_node(object) }) else {
        return 0;
    };
    if index >= node.length || word_count != node.word_count || masks_count != node.mask_count {
        return 0;
    }
    let start = index * node.word_count;
    let element_masks = &all_masks[index * node.mask_count..(index + 1) * node.mask_count];
    drop_list_words(&all_words[start..start + node.word_count], element_masks);
    unsafe {
        let target = node.data.add(start);
        std::ptr::copy_nonoverlapping(words, target, word_count);
        let target_masks = node
            .data
            .add(node.capacity * node.word_count + index * node.mask_count);
        std::ptr::copy_nonoverlapping(masks, target_masks, masks_count);
    }
    1
}

pub(crate) extern "C" fn jk_mut_list_pop(
    object: *mut u8,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    if output.is_null() {
        return 0;
    }
    let Some((mut node, words, masks)) = (unsafe { mut_list_node(object) }) else {
        return 0;
    };
    if node.length == 0 || output_count != node.word_count {
        return 0;
    }
    let start = (node.length - 1) * node.word_count;
    let value = &words[start..start + node.word_count];
    let element_masks = &masks[(node.length - 1) * node.mask_count..node.length * node.mask_count];
    let Some(cloned) = clone_managed_words(value, element_masks, 0) else {
        return 0;
    };
    unsafe {
        std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count);
    }
    // The cloned result owns its references; release the stored element and shrink length.
    drop_list_words(value, element_masks);
    node.length -= 1;
    unsafe {
        object.cast::<MutListNode>().write(node);
    }
    1
}

/// Unique cursor that owns a MutList and walks it with packed `get(i)`.
#[repr(C)]
struct MutListCursorNode {
    list: *mut u8,
    index: usize,
}

impl Drop for MutListCursorNode {
    fn drop(&mut self) {
        if !self.list.is_null() {
            jk_drop(self.list);
        }
    }
}

unsafe fn list_cursor_node(object: *mut u8) -> Option<&'static mut MutListCursorNode> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::MutListCursor as u8
        || header.payload_size != size_of::<MutListCursorNode>()
    {
        return None;
    }
    unsafe { object.cast::<MutListCursorNode>().as_mut() }
}

unsafe extern "C" fn drop_mut_list_cursor(pointer: *mut u8) {
    unsafe { pointer.cast::<MutListCursorNode>().drop_in_place() };
}

/// Adopts the unique MutList pointer; O(1), copies nothing.
pub(crate) unsafe extern "C" fn jk_mut_list_cursor_new(list: *mut u8) -> *mut u8 {
    if mut_list_node(list).is_none() {
        jk_drop(list);
        return std::ptr::null_mut();
    }
    let pointer = allocate_object(
        RuntimeValueKind::MutListCursor,
        size_of::<MutListCursorNode>(),
        align_of::<MutListCursorNode>(),
        Some(drop_mut_list_cursor),
    );
    if pointer.is_null() {
        jk_drop(list);
        return pointer;
    }
    unsafe {
        pointer
            .cast::<MutListCursorNode>()
            .write(MutListCursorNode { list, index: 0 })
    };
    pointer
}

/// Consumes the caller’s cursor reference. Returns the same object with the
/// index advanced, or null at exhaustion after releasing the cursor and list.
pub(crate) unsafe extern "C" fn jk_mut_list_cursor_step(
    object: *mut u8,
    output: *mut u64,
    output_count: usize,
) -> *mut u8 {
    let Some(node) = (unsafe { list_cursor_node(object) }) else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    if jk_mut_list_get(node.list, node.index, output, output_count) == 0 {
        jk_drop(object);
        return std::ptr::null_mut();
    }
    node.index += 1;
    object
}

/// Snapshot into an immutable List without consuming the MutList.
pub(crate) extern "C" fn jk_mut_list_to_list(object: *mut u8) -> *mut u8 {
    let Some((node, words, masks)) = (unsafe { mut_list_node(object) }) else {
        return std::ptr::null_mut();
    };
    let mut list = std::ptr::null_mut();
    for index in (0..node.length).rev() {
        let start = index * node.word_count;
        let value = &words[start..start + node.word_count];
        let element_masks = &masks[index * node.mask_count..(index + 1) * node.mask_count];
        let Some(cloned) = clone_managed_words(value, element_masks, 0) else {
            jk_drop(list);
            return std::ptr::null_mut();
        };
        list = super::list::jk_list_cons(
            cloned.as_ptr(),
            cloned.len(),
            element_masks.as_ptr(),
            element_masks.len(),
            list,
        );
        if list.is_null() {
            return list;
        }
    }
    list
}
