//! Mutable map runtime implementation.
//!
//! A dense entry array owns keys/values; bucket chains index their cached hashes.
//! Removal swaps in the last entry and repairs its links without shifting a suffix.

use super::hashing::KeyOps;
use super::managed::*;
use std::alloc::{self, Layout};
use std::mem::{align_of, size_of};

#[cfg(test)]
const MAP_KEY_STRING: usize = 1;
const NO_ENTRY: usize = usize::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct MutMapEntry {
    hash: u64,
    occupied: u8,
    _padding: [u8; 7],
    previous: usize,
    next: usize,
    key: *mut u64,
    value: *mut u64,
    key_masks: *mut u64,
    value_masks: *mut u64,
}

impl MutMapEntry {
    const EMPTY: Self = Self {
        hash: 0,
        occupied: 0,
        _padding: [0; 7],
        previous: NO_ENTRY,
        next: NO_ENTRY,
        key: std::ptr::null_mut(),
        value: std::ptr::null_mut(),
        key_masks: std::ptr::null_mut(),
        value_masks: std::ptr::null_mut(),
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MutMapNode {
    length: usize,
    capacity: usize,
    key_word_count: usize,
    key_mask_count: usize,
    value_word_count: usize,
    value_mask_count: usize,
    key_ops: KeyOps,
    entries: *mut MutMapEntry,
    buckets: *mut usize,
}

fn array_layout<T>(count: usize) -> Option<Layout> {
    Layout::array::<T>(count).ok()
}

fn allocate_words(words: &[u64]) -> *mut u64 {
    let Some(layout) = array_layout::<u64>(words.len()) else {
        return std::ptr::null_mut();
    };
    let pointer = unsafe { alloc::alloc(layout).cast::<u64>() };
    if pointer.is_null() {
        return pointer;
    }
    unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), pointer, words.len()) };
    pointer
}

fn deallocate_words(pointer: *mut u64, count: usize) {
    if pointer.is_null() {
        return;
    }
    if let Some(layout) = array_layout::<u64>(count) {
        unsafe { alloc::dealloc(pointer.cast::<u8>(), layout) };
    }
}

unsafe fn node<'a>(object: *mut u8) -> Option<(MutMapNode, &'a mut [MutMapEntry])> {
    let header = valid_header(object)?;
    node_from_header(object, header)
}

unsafe fn node_from_header<'a>(
    object: *mut u8,
    header: &ObjectHeader,
) -> Option<(MutMapNode, &'a mut [MutMapEntry])> {
    if header.kind != RuntimeValueKind::MutMap as u8
        || header.payload_size != size_of::<MutMapNode>()
    {
        return None;
    }
    let node = object.cast::<MutMapNode>().read();
    if node.key_word_count == 0
        || node.value_word_count == 0
        || node.capacity < node.length
        || mask_count(node.key_word_count)? != node.key_mask_count
        || mask_count(node.value_word_count)? != node.value_mask_count
        || (node.capacity != 0
            && (!node.capacity.is_power_of_two()
                || node.entries.is_null()
                || node.buckets.is_null()))
    {
        return None;
    }
    let entries = if node.entries.is_null() {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(node.entries, node.capacity)
    };
    Some((node, entries))
}

unsafe fn drop_entry(node: MutMapNode, entry: MutMapEntry) {
    if entry.occupied == 0 {
        return;
    }
    if !entry.key.is_null() {
        drop_list_words(
            std::slice::from_raw_parts(entry.key, node.key_word_count),
            std::slice::from_raw_parts(entry.key_masks, node.key_mask_count),
        );
        deallocate_words(entry.key, node.key_word_count);
        deallocate_words(entry.key_masks, node.key_mask_count);
    }
    if !entry.value.is_null() {
        drop_list_words(
            std::slice::from_raw_parts(entry.value, node.value_word_count),
            std::slice::from_raw_parts(entry.value_masks, node.value_mask_count),
        );
        deallocate_words(entry.value, node.value_word_count);
        deallocate_words(entry.value_masks, node.value_mask_count);
    }
}

pub(crate) unsafe fn drop_mut_map(object: *mut u8, header: &ObjectHeader) {
    let Some((node, entries)) = node_from_header(object, header) else {
        return;
    };
    for entry in entries.iter().copied().take(node.length) {
        drop_entry(node, entry);
    }
    if !node.entries.is_null() {
        if let Some(layout) = array_layout::<MutMapEntry>(node.capacity) {
            alloc::dealloc(node.entries.cast::<u8>(), layout);
        }
    }
    if !node.buckets.is_null() {
        if let Some(layout) = array_layout::<usize>(node.capacity) {
            alloc::dealloc(node.buckets.cast::<u8>(), layout);
        }
    }
}

pub(crate) extern "C" fn jk_mut_map_new(
    key_word_count: usize,
    key_mask_count: usize,
    value_word_count: usize,
    value_mask_count: usize,
    key_kind: usize,
) -> *mut u8 {
    if key_word_count == 0
        || value_word_count == 0
        || mask_count(key_word_count) != Some(key_mask_count)
        || mask_count(value_word_count) != Some(value_mask_count)
    {
        return std::ptr::null_mut();
    }
    let object = jk_alloc_object(
        RuntimeValueKind::MutMap as u8,
        size_of::<MutMapNode>(),
        align_of::<MutMapNode>(),
    );
    if object.is_null() {
        return object;
    }
    unsafe {
        object.cast::<MutMapNode>().write(MutMapNode {
            length: 0,
            capacity: 0,
            key_word_count,
            key_mask_count,
            value_word_count,
            value_mask_count,
            key_ops: KeyOps::from_abi(key_kind),
            entries: std::ptr::null_mut(),
            buckets: std::ptr::null_mut(),
        });
    }
    object
}

unsafe fn keys_equal(
    node: MutMapNode,
    entry: MutMapEntry,
    key: &[u64],
    ops: KeyOps,
    hash: u64,
) -> bool {
    if entry.occupied == 0
        || key.len() != node.key_word_count
        || (node.key_ops.hashed() && ops.hashed() && entry.hash != hash)
    {
        return false;
    }
    ops.equal(
        std::slice::from_raw_parts(entry.key, key.len()),
        std::slice::from_raw_parts(entry.key_masks, node.key_mask_count),
        key,
    )
}

unsafe fn grow(node: &mut MutMapNode) -> bool {
    if node.length < node.capacity {
        return true;
    }
    let Some(new_capacity) = node.capacity.checked_mul(2).map(|n| n.max(4)) else {
        return false;
    };
    let Some(layout) = array_layout::<MutMapEntry>(new_capacity) else {
        return false;
    };
    let Some(bucket_layout) = array_layout::<usize>(new_capacity) else {
        return false;
    };
    let new_entries = alloc::alloc(layout).cast::<MutMapEntry>();
    if new_entries.is_null() {
        return false;
    }
    let new_buckets = alloc::alloc(bucket_layout).cast::<usize>();
    if new_buckets.is_null() {
        alloc::dealloc(new_entries.cast::<u8>(), layout);
        return false;
    }
    for index in 0..new_capacity {
        new_entries.add(index).write(MutMapEntry::EMPTY);
        new_buckets.add(index).write(NO_ENTRY);
    }
    if !node.entries.is_null() {
        std::ptr::copy_nonoverlapping(node.entries, new_entries, node.length);
        if let Some(old_layout) = array_layout::<MutMapEntry>(node.capacity) {
            alloc::dealloc(node.entries.cast::<u8>(), old_layout);
        }
        alloc::dealloc(
            node.buckets.cast::<u8>(),
            array_layout::<usize>(node.capacity).unwrap(),
        );
    }
    node.entries = new_entries;
    node.capacity = new_capacity;
    node.buckets = new_buckets;
    // Rebuild from cached hashes: no user Hash or equality callbacks during growth.
    let entries = std::slice::from_raw_parts_mut(new_entries, new_capacity);
    for index in 0..node.length {
        link_entry(*node, entries, index);
    }
    true
}

fn bucket(hash: u64, capacity: usize) -> usize {
    hash as usize & (capacity - 1)
}

unsafe fn link_entry(node: MutMapNode, entries: &mut [MutMapEntry], index: usize) {
    let head = node.buckets.add(bucket(entries[index].hash, node.capacity));
    entries[index].previous = NO_ENTRY;
    entries[index].next = *head;
    if *head != NO_ENTRY {
        entries[*head].previous = index;
    }
    *head = index;
}

unsafe fn unlink_entry(node: MutMapNode, entries: &mut [MutMapEntry], index: usize) {
    let entry = entries[index];
    if entry.previous == NO_ENTRY {
        *node.buckets.add(bucket(entry.hash, node.capacity)) = entry.next;
    } else {
        entries[entry.previous].next = entry.next;
    }
    if entry.next != NO_ENTRY {
        entries[entry.next].previous = entry.previous;
    }
}

unsafe fn find_entry(
    node: MutMapNode,
    entries: &[MutMapEntry],
    key: &[u64],
    ops: KeyOps,
    hash: u64,
) -> Option<usize> {
    if node.length == 0 {
        return None;
    }
    // Raw runtime callers can still use the legacy structural/String descriptors.
    if !node.key_ops.hashed() || !ops.hashed() {
        return entries
            .iter()
            .take(node.length)
            .position(|entry| keys_equal(node, *entry, key, ops, hash));
    }
    let mut index = *node.buckets.add(bucket(hash, node.capacity));
    while index != NO_ENTRY {
        let entry = entries[index];
        if keys_equal(node, entry, key, ops, hash) {
            return Some(index);
        }
        index = entry.next;
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn valid_input(
    node: MutMapNode,
    key: *const u64,
    key_count: usize,
    key_masks: *const u64,
    key_masks_count: usize,
    value: *const u64,
    value_count: usize,
    value_masks: *const u64,
    value_masks_count: usize,
) -> bool {
    !key.is_null()
        && !key_masks.is_null()
        && !value.is_null()
        && !value_masks.is_null()
        && key_count == node.key_word_count
        && key_masks_count == node.key_mask_count
        && value_count == node.value_word_count
        && value_masks_count == node.value_mask_count
}

unsafe fn copy_entry(
    node: MutMapNode,
    key: &[u64],
    key_masks: &[u64],
    value: &[u64],
    value_masks: &[u64],
    hash: u64,
) -> Option<MutMapEntry> {
    let key_pointer = allocate_words(key);
    let key_mask_pointer = allocate_words(key_masks);
    let value_pointer = allocate_words(value);
    let value_mask_pointer = allocate_words(value_masks);
    if key_pointer.is_null()
        || key_mask_pointer.is_null()
        || value_pointer.is_null()
        || value_mask_pointer.is_null()
    {
        deallocate_words(key_pointer, node.key_word_count);
        deallocate_words(key_mask_pointer, node.key_mask_count);
        deallocate_words(value_pointer, node.value_word_count);
        deallocate_words(value_mask_pointer, node.value_mask_count);
        return None;
    }
    Some(MutMapEntry {
        hash,
        occupied: 1,
        _padding: [0; 7],
        previous: NO_ENTRY,
        next: NO_ENTRY,
        key: key_pointer,
        value: value_pointer,
        key_masks: key_mask_pointer,
        value_masks: value_mask_pointer,
    })
}

unsafe fn clone_value_to_output(
    node: MutMapNode,
    entry: MutMapEntry,
    output: *mut u64,
    output_count: usize,
) -> bool {
    if output.is_null() || output_count != node.value_word_count {
        return false;
    }
    let value = std::slice::from_raw_parts(entry.value, node.value_word_count);
    let masks = std::slice::from_raw_parts(entry.value_masks, node.value_mask_count);
    let Some(cloned) = clone_managed_words(value, masks, 0) else {
        return false;
    };
    std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count);
    true
}

pub(crate) extern "C" fn jk_mut_map_insert(
    object: *mut u8,
    key: *const u64,
    key_count: usize,
    key_masks: *const u64,
    key_masks_count: usize,
    value: *const u64,
    value_count: usize,
    value_masks: *const u64,
    value_masks_count: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    let Some((mut node_value, entries)) = (unsafe { node(object) }) else {
        return 0;
    };
    if !valid_input(
        node_value,
        key,
        key_count,
        key_masks,
        key_masks_count,
        value,
        value_count,
        value_masks,
        value_masks_count,
    ) {
        return 0;
    }
    let key_slice = unsafe { std::slice::from_raw_parts(key, key_count) };
    let key_mask_slice = unsafe { std::slice::from_raw_parts(key_masks, key_masks_count) };
    let value_slice = unsafe { std::slice::from_raw_parts(value, value_count) };
    let value_mask_slice = unsafe { std::slice::from_raw_parts(value_masks, value_masks_count) };
    let hash = node_value.key_ops.hash(key_slice);
    if let Some(index) =
        unsafe { find_entry(node_value, entries, key_slice, node_value.key_ops, hash) }
    {
        let entry = &mut entries[index];
        let Some(mut replacement) = (unsafe {
            copy_entry(
                node_value,
                key_slice,
                key_mask_slice,
                value_slice,
                value_mask_slice,
                hash,
            )
        }) else {
            return 0;
        };
        if !unsafe { clone_value_to_output(node_value, *entry, output, output_count) } {
            // Payload ownership transfers only after the replacement succeeds.
            deallocate_words(replacement.key, node_value.key_word_count);
            deallocate_words(replacement.key_masks, node_value.key_mask_count);
            deallocate_words(replacement.value, node_value.value_word_count);
            deallocate_words(replacement.value_masks, node_value.value_mask_count);
            return 0;
        }
        replacement.previous = entry.previous;
        replacement.next = entry.next;
        unsafe { drop_entry(node_value, *entry) };
        *entry = replacement;
        return 1;
    }
    if output.is_null() || output_count != node_value.value_word_count {
        return 0;
    }
    if !unsafe { grow(&mut node_value) } {
        return 0;
    }
    // Publish any relocated buffers even if allocating the new payload fails.
    unsafe { object.cast::<MutMapNode>().write(node_value) };
    let Some(new_entry) = (unsafe {
        copy_entry(
            node_value,
            key_slice,
            key_mask_slice,
            value_slice,
            value_mask_slice,
            hash,
        )
    }) else {
        return 0;
    };
    unsafe {
        let entries = std::slice::from_raw_parts_mut(node_value.entries, node_value.capacity);
        entries[node_value.length] = new_entry;
        link_entry(node_value, entries, node_value.length);
    }
    node_value.length += 1;
    unsafe { object.cast::<MutMapNode>().write(node_value) };
    0
}

pub(crate) extern "C" fn jk_mut_map_get(
    object: *mut u8,
    key: *const u64,
    key_count: usize,
    key_kind: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    let Some((node, entries)) = (unsafe { node(object) }) else {
        return 0;
    };
    if key.is_null() || key_count != node.key_word_count || output_count != node.value_word_count {
        return 0;
    }
    let key_slice = unsafe { std::slice::from_raw_parts(key, key_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key_slice);
    if let Some(index) = unsafe { find_entry(node, entries, key_slice, ops, hash) } {
        return unsafe { clone_value_to_output(node, entries[index], output, output_count) } as u8;
    }
    0
}

pub(crate) extern "C" fn jk_mut_map_remove(
    object: *mut u8,
    key: *const u64,
    key_count: usize,
    key_kind: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    let Some((node, entries)) = (unsafe { node(object) }) else {
        return 0;
    };
    if key.is_null() || key_count != node.key_word_count || output_count != node.value_word_count {
        return 0;
    }
    let key_slice = unsafe { std::slice::from_raw_parts(key, key_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key_slice);
    let Some(index) = (unsafe { find_entry(node, entries, key_slice, ops, hash) }) else {
        return 0;
    };
    if !unsafe { clone_value_to_output(node, entries[index], output, output_count) } {
        return 0;
    }
    unsafe {
        unlink_entry(node, entries, index);
        drop_entry(node, entries[index]);
        let last = node.length - 1;
        if index != last {
            unlink_entry(node, entries, last);
            entries[index] = entries[last];
            link_entry(node, entries, index);
        }
    }
    entries[node.length - 1] = MutMapEntry::EMPTY;
    unsafe {
        object.cast::<MutMapNode>().write(MutMapNode {
            length: node.length - 1,
            ..node
        })
    };
    1
}

pub(crate) extern "C" fn jk_mut_map_contains_key(
    object: *mut u8,
    key: *const u64,
    key_count: usize,
    key_kind: usize,
) -> u8 {
    let Some((node, entries)) = (unsafe { node(object) }) else {
        return 0;
    };
    if key.is_null() || key_count != node.key_word_count {
        return 0;
    }
    let key_slice = unsafe { std::slice::from_raw_parts(key, key_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key_slice);
    unsafe { find_entry(node, entries, key_slice, ops, hash) }.is_some() as u8
}

pub(crate) extern "C" fn jk_mut_map_length(object: *mut u8) -> usize {
    unsafe { node(object).map(|(node, _)| node.length).unwrap_or(0) }
}

pub(crate) extern "C" fn jk_mut_map_capacity(object: *mut u8) -> usize {
    unsafe { node(object).map(|(node, _)| node.capacity).unwrap_or(0) }
}

pub(crate) extern "C" fn jk_mut_map_is_empty(object: *mut u8) -> u8 {
    (jk_mut_map_length(object) == 0) as u8
}

unsafe fn clone_key_to_output(
    node: MutMapNode,
    entry: MutMapEntry,
    output: *mut u64,
    output_count: usize,
) -> bool {
    if output.is_null() || output_count != node.key_word_count {
        return false;
    }
    let key = std::slice::from_raw_parts(entry.key, node.key_word_count);
    let masks = std::slice::from_raw_parts(entry.key_masks, node.key_mask_count);
    let Some(cloned) = clone_managed_words(key, masks, 0) else {
        return false;
    };
    std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count);
    true
}

fn append_masked_words(
    dest_words: &mut Vec<u64>,
    dest_masks: &mut Vec<u64>,
    words: &[u64],
    masks: &[u64],
) {
    let start = dest_words.len();
    dest_words.extend_from_slice(words);
    dest_masks.resize(dest_words.len().div_ceil(64), 0);
    for (offset, _) in words.iter().enumerate() {
        let bit = masks.get(offset / 64).copied().unwrap_or(0) >> (offset % 64) & 1;
        if bit == 1 {
            let index = start + offset;
            dest_masks[index / 64] |= 1 << (index % 64);
        }
    }
}

/// Unique cursor that owns a MutMap and walks packed slots.
#[repr(C)]
struct MutMapCursorNode {
    map: *mut u8,
    index: usize,
}

impl Drop for MutMapCursorNode {
    fn drop(&mut self) {
        if !self.map.is_null() {
            jk_drop(self.map);
        }
    }
}

unsafe fn map_cursor_node(object: *mut u8) -> Option<&'static mut MutMapCursorNode> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::MutMapCursor as u8
        || header.payload_size != size_of::<MutMapCursorNode>()
    {
        return None;
    }
    unsafe { object.cast::<MutMapCursorNode>().as_mut() }
}

unsafe extern "C" fn drop_mut_map_cursor(pointer: *mut u8) {
    unsafe { pointer.cast::<MutMapCursorNode>().drop_in_place() };
}

/// Adopts the unique MutMap/MutSet pointer.
pub(crate) unsafe extern "C" fn jk_mut_map_cursor_new(map: *mut u8) -> *mut u8 {
    if node(map).is_none() {
        jk_drop(map);
        return std::ptr::null_mut();
    }
    let pointer = allocate_object(
        RuntimeValueKind::MutMapCursor,
        size_of::<MutMapCursorNode>(),
        align_of::<MutMapCursorNode>(),
        Some(drop_mut_map_cursor),
    );
    if pointer.is_null() {
        jk_drop(map);
        return pointer;
    }
    unsafe {
        pointer
            .cast::<MutMapCursorNode>()
            .write(MutMapCursorNode { map, index: 0 })
    };
    pointer
}

/// Projection 0 clones key and value; 1 clones only the key (MutSet).
pub(crate) unsafe extern "C" fn jk_mut_map_cursor_step(
    object: *mut u8,
    projection: usize,
    key_output: *mut u64,
    key_count: usize,
    value_output: *mut u64,
    value_count: usize,
) -> *mut u8 {
    let Some(cursor) = (unsafe { map_cursor_node(object) }) else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    let Some((map, entries)) = (unsafe { node(cursor.map) }) else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    if cursor.index >= map.length {
        jk_drop(object);
        return std::ptr::null_mut();
    }
    let entry = entries[cursor.index];
    if !unsafe { clone_key_to_output(map, entry, key_output, key_count) } {
        jk_drop(object);
        return std::ptr::null_mut();
    }
    if projection == 0 && !unsafe { clone_value_to_output(map, entry, value_output, value_count) } {
        drop_list_words(
            std::slice::from_raw_parts(key_output, key_count),
            std::slice::from_raw_parts(entry.key_masks, map.key_mask_count),
        );
        jk_drop(object);
        return std::ptr::null_mut();
    }
    cursor.index += 1;
    object
}

/// Snapshot packed entries (`keys_only == 0`) or keys (`keys_only != 0`).
pub(crate) extern "C" fn jk_mut_map_to_list(object: *mut u8, keys_only: u8) -> *mut u8 {
    let Some((node, entries)) = (unsafe { node(object) }) else {
        return std::ptr::null_mut();
    };
    let mut list = std::ptr::null_mut();
    for index in (0..node.length).rev() {
        let entry = entries[index];
        let key = unsafe { std::slice::from_raw_parts(entry.key, node.key_word_count) };
        let key_masks = unsafe { std::slice::from_raw_parts(entry.key_masks, node.key_mask_count) };
        let Some(cloned_key) = clone_managed_words(key, key_masks, 0) else {
            jk_drop(list);
            return std::ptr::null_mut();
        };
        let (words, masks) = if keys_only == 0 {
            let value = unsafe { std::slice::from_raw_parts(entry.value, node.value_word_count) };
            let value_masks =
                unsafe { std::slice::from_raw_parts(entry.value_masks, node.value_mask_count) };
            let Some(cloned_value) = clone_managed_words(value, value_masks, 0) else {
                drop_list_words(&cloned_key, key_masks);
                jk_drop(list);
                return std::ptr::null_mut();
            };
            let mut words = Vec::new();
            let mut masks = Vec::new();
            append_masked_words(&mut words, &mut masks, &cloned_key, key_masks);
            append_masked_words(&mut words, &mut masks, &cloned_value, value_masks);
            (words, masks)
        } else {
            (cloned_key, key_masks.to_vec())
        };
        list = super::list::jk_list_cons(
            words.as_ptr(),
            words.len(),
            masks.as_ptr(),
            masks.len(),
            list,
        );
        if list.is_null() {
            return list;
        }
    }
    list
}

#[cfg(test)]
mod benchmarks;
#[cfg(test)]
mod index_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_map_round_trip() {
        let baseline = live_object_count();
        let map = jk_mut_map_new(2, 1, 1, 1, MAP_KEY_STRING);
        let key_text = "one";
        let key = jk_string_from_utf8(key_text.as_ptr(), key_text.len());
        let key_words = [key as u64, key_text.len() as u64];
        let key_masks = [1_u64];
        let value_words = [1_u64];
        let value_masks = [0_u64];
        let mut output = [0_u64];
        assert_eq!(
            jk_mut_map_insert(
                map,
                key_words.as_ptr(),
                2,
                key_masks.as_ptr(),
                1,
                value_words.as_ptr(),
                1,
                value_masks.as_ptr(),
                1,
                output.as_mut_ptr(),
                1,
            ),
            0
        );
        assert_eq!(jk_mut_map_length(map), 1);
        assert_eq!(
            jk_mut_map_contains_key(map, key_words.as_ptr(), 2, MAP_KEY_STRING),
            1
        );
        assert_eq!(
            jk_mut_map_get(
                map,
                key_words.as_ptr(),
                2,
                MAP_KEY_STRING,
                output.as_mut_ptr(),
                1
            ),
            1
        );
        assert_eq!(output, [1]);
        // Insertion consumes the key; dropping the map releases that owner.
        // A second drop of key can free another test's allocation at this address.
        jk_drop(map);
        assert_eq!(live_object_count(), baseline);
    }

    #[test]
    fn mutable_map_releases_string_entries() {
        let baseline = live_object_count();
        let map = jk_mut_map_new(2, 1, 2, 1, MAP_KEY_STRING);
        let key_text = "key";
        let value_text = "value";
        let key = jk_string_from_utf8(key_text.as_ptr(), key_text.len());
        let value = jk_string_from_utf8(value_text.as_ptr(), value_text.len());
        let key_words = [key as u64, key_text.len() as u64];
        let key_masks = [1_u64];
        let value_words = [value as u64, value_text.len() as u64];
        let value_masks = [1_u64];
        let mut output = [0_u64; 2];
        assert_eq!(
            jk_mut_map_insert(
                map,
                key_words.as_ptr(),
                2,
                key_masks.as_ptr(),
                1,
                value_words.as_ptr(),
                2,
                value_masks.as_ptr(),
                1,
                output.as_mut_ptr(),
                2,
            ),
            0
        );
        let lookup_text = "key";
        let lookup = jk_string_from_utf8(lookup_text.as_ptr(), lookup_text.len());
        let lookup_words = [lookup as u64, lookup_text.len() as u64];
        assert_eq!(
            jk_mut_map_get(
                map,
                lookup_words.as_ptr(),
                2,
                MAP_KEY_STRING,
                output.as_mut_ptr(),
                2
            ),
            1
        );
        jk_drop(lookup);
        jk_drop(map);
        assert_eq!(live_object_count(), baseline + 1);
        jk_drop(output[0] as *mut u8);
        assert_eq!(live_object_count(), baseline);
    }
}
