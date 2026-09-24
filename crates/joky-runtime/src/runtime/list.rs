//! Persistent immutable list runtime implementation.
use super::managed::*;
use std::mem::{align_of, size_of};
#[derive(Clone, Copy)]
struct ListNode {
    tail: *mut u8,
    word_count: usize,
    mask_count: usize,
}

/// Both nodes are uniquely owned here; consumes tail into the singleton.
pub(crate) unsafe fn prepend_singleton(singleton: *mut u8, tail: *mut u8) {
    let node = unsafe { &mut *singleton.cast::<ListNode>() };
    debug_assert!(node.tail.is_null());
    node.tail = tail;
}
unsafe fn list_node_from_header<'a>(
    object: *mut u8,
    header: &ObjectHeader,
) -> Option<(ListNode, &'a [u64], &'a [u64])> {
    if header.kind != RuntimeValueKind::List as u8 || header.payload_size < size_of::<ListNode>() {
        return None;
    }
    let node = object.cast::<ListNode>().read();
    if node.word_count == 0 || mask_count(node.word_count)? != node.mask_count {
        return None;
    }
    let words_size = node.word_count.checked_mul(size_of::<u64>())?;
    let masks_size = node.mask_count.checked_mul(size_of::<u64>())?;
    let expected_size = size_of::<ListNode>()
        .checked_add(words_size)?
        .checked_add(masks_size)?;
    if expected_size != header.payload_size {
        return None;
    }
    let words_pointer = object.add(size_of::<ListNode>()).cast::<u64>();
    let masks_pointer = words_pointer.add(node.word_count);
    Some((
        node,
        std::slice::from_raw_parts(words_pointer, node.word_count),
        std::slice::from_raw_parts(masks_pointer, node.mask_count),
    ))
}

pub(crate) fn clone_list_words(words: &[u64], masks: &[u64]) -> Option<Vec<u64>> {
    clone_managed_words(words, masks, 0)
}

pub(crate) fn drop_list_object(object: *mut u8, header: &ObjectHeader) {
    let Some((node, words, masks)) = (unsafe { list_node_from_header(object, header) }) else {
        return;
    };
    drop_list_words(words, masks);
    jk_drop(node.tail);
}

unsafe fn list_node<'a>(object: *mut u8) -> Option<(ListNode, &'a [u64], &'a [u64])> {
    let header = valid_header(object)?;
    list_node_from_header(object, header)
}

pub(crate) extern "C" fn jk_list_cons(
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
    tail: *mut u8,
) -> *mut u8 {
    if words.is_null()
        || masks.is_null()
        || word_count == 0
        || mask_count(word_count) != Some(masks_count)
    {
        return std::ptr::null_mut();
    }
    let words = unsafe { std::slice::from_raw_parts(words, word_count) };
    let masks = unsafe { std::slice::from_raw_parts(masks, masks_count) };
    let payload_size = match size_of::<ListNode>()
        .checked_add(word_count.saturating_mul(size_of::<u64>()))
        .and_then(|size| size.checked_add(masks_count.saturating_mul(size_of::<u64>())))
    {
        Some(size) => size,
        None => return std::ptr::null_mut(),
    };
    let object = jk_alloc_object(
        RuntimeValueKind::List as u8,
        payload_size,
        align_of::<ListNode>(),
    );
    if object.is_null() {
        drop_list_words(words, masks);
        jk_drop(tail);
        return object;
    }
    unsafe {
        object.cast::<ListNode>().write(ListNode {
            tail,
            word_count,
            mask_count: masks_count,
        });
        let words_pointer = object.add(size_of::<ListNode>()).cast::<u64>();
        std::ptr::copy_nonoverlapping(words.as_ptr(), words_pointer, word_count);
        std::ptr::copy_nonoverlapping(masks.as_ptr(), words_pointer.add(word_count), masks_count);
    };
    object
}

pub(crate) extern "C" fn jk_list_head(
    object: *mut u8,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    if output.is_null() || output_count == 0 {
        return 0;
    }
    unsafe { std::ptr::write_bytes(output, 0, output_count) };
    let Some((_, words, masks)) = (unsafe { list_node(object) }) else {
        return 0;
    };
    if words.len() != output_count {
        return 0;
    }
    let Some(cloned) = clone_list_words(words, masks) else {
        return 0;
    };
    unsafe { std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count) };
    1
}

pub(crate) extern "C" fn jk_list_tail(object: *mut u8) -> *mut u8 {
    unsafe { list_node(object) }
        .map(|(node, _, _)| jk_dup(node.tail))
        .unwrap_or(std::ptr::null_mut())
}

pub(crate) extern "C" fn jk_list_length(object: *mut u8) -> usize {
    let mut length: usize = 0;
    let mut current = object;
    while let Some((node, _, _)) = unsafe { list_node(current) } {
        length = match length.checked_add(1) {
            Some(length) => length,
            None => return 0,
        };
        current = node.tail;
    }
    length
}

pub(crate) extern "C" fn jk_list_reverse(object: *mut u8) -> *mut u8 {
    let mut current = object;
    let mut reversed = std::ptr::null_mut();
    while let Some((node, words, masks)) = unsafe { list_node(current) } {
        let Some(cloned) = clone_list_words(words, masks) else {
            jk_drop(reversed);
            return std::ptr::null_mut();
        };
        let next = jk_list_cons(
            cloned.as_ptr(),
            cloned.len(),
            masks.as_ptr(),
            masks.len(),
            reversed,
        );
        if next.is_null() {
            return std::ptr::null_mut();
        }
        reversed = next;
        current = node.tail;
    }
    reversed
}

/// Sequential result builder: links singleton cons nodes at the tail in push
/// order, replacing the lock-and-sort batch coordinator for sequential loops.
#[repr(C)]
struct SeqBuilderNode {
    head: *mut u8,
    tail: *mut u8,
}

unsafe fn seq_builder_state<'a>(object: *mut u8) -> Option<&'a mut SeqBuilderNode> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::SeqBuilder as u8
        || header.payload_size != size_of::<SeqBuilderNode>()
    {
        return None;
    }
    unsafe { object.cast::<SeqBuilderNode>().as_mut() }
}

unsafe extern "C" fn drop_seq_builder(pointer: *mut u8) {
    let builder = unsafe { &*pointer.cast::<SeqBuilderNode>() };
    if !builder.head.is_null() {
        jk_drop(builder.head);
    }
}

pub(crate) extern "C" fn jk_seq_new() -> *mut u8 {
    let pointer = allocate_object(
        RuntimeValueKind::SeqBuilder,
        size_of::<SeqBuilderNode>(),
        align_of::<SeqBuilderNode>(),
        Some(drop_seq_builder),
    );
    if pointer.is_null() {
        return pointer;
    }
    unsafe {
        pointer.cast::<SeqBuilderNode>().write(SeqBuilderNode {
            head: std::ptr::null_mut(),
            tail: std::ptr::null_mut(),
        })
    };
    pointer
}

/// Borrows the builder; adopts the caller's singleton node reference. O(1):
/// links the node after the current tail without copying payload.
pub(crate) unsafe extern "C" fn jk_seq_push(builder: *mut u8, node: *mut u8) {
    let Some(state) = (unsafe { seq_builder_state(builder) }) else {
        jk_drop(node);
        return;
    };
    if state.tail.is_null() {
        state.head = node;
    } else {
        unsafe { prepend_singleton(state.tail, node) };
    }
    state.tail = node;
}

/// Borrows the builder and detaches the linked list in push order; null
/// when nothing was pushed. The caller still owns the builder reference.
pub(crate) unsafe extern "C" fn jk_seq_finish(builder: *mut u8) -> *mut u8 {
    let Some(state) = (unsafe { seq_builder_state(builder) }) else {
        return std::ptr::null_mut();
    };
    let head = state.head;
    state.head = std::ptr::null_mut();
    state.tail = std::ptr::null_mut();
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    fn singleton(word: u64) -> *mut u8 {
        let words = [word];
        let masks = [0];
        jk_list_cons(words.as_ptr(), 1, masks.as_ptr(), 1, std::ptr::null_mut())
    }

    fn heads(mut current: *mut u8) -> Vec<u64> {
        let mut values = Vec::new();
        while !current.is_null() {
            let mut head = [0];
            assert_eq!(jk_list_head(current, head.as_mut_ptr(), 1), 1);
            values.push(head[0]);
            let next = jk_list_tail(current);
            jk_drop(current);
            current = next;
        }
        values
    }

    #[test]
    fn seq_builder_links_singletons_in_push_order() {
        let builder = jk_seq_new();
        assert!(!builder.is_null());
        unsafe {
            jk_seq_push(builder, singleton(1));
            jk_seq_push(builder, singleton(2));
            jk_seq_push(builder, singleton(3));
        }
        let list = unsafe { jk_seq_finish(builder) };
        jk_drop(builder);
        assert_eq!(heads(list), [1, 2, 3]);
        assert_eq!(live_object_count(), 0);
    }

    #[test]
    fn seq_builder_finish_of_empty_is_null() {
        let builder = jk_seq_new();
        let list = unsafe { jk_seq_finish(builder) };
        jk_drop(builder);
        assert!(list.is_null());
        assert_eq!(live_object_count(), 0);
    }

    #[test]
    fn seq_builder_drop_releases_unfinished_nodes() {
        let builder = jk_seq_new();
        unsafe {
            jk_seq_push(builder, singleton(7));
            jk_seq_push(builder, singleton(8));
        }
        jk_drop(builder);
        assert_eq!(live_object_count(), 0);
    }
}
