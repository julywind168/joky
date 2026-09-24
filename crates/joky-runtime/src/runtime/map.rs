//! Persistent immutable maps backed by a bitmap-indexed hash trie.
use super::hashing::KeyOps;
use super::managed::*;
use std::mem::{align_of, size_of};
use std::sync::Arc;

mod trie;
use trie::{Entry, Kind, Node};
#[cfg(test)]
mod benchmarks;
#[cfg(test)]
mod tests;

struct MapRoot {
    node: Arc<Node>,
    // Legacy descriptors have no hashes; mixed raw-runtime calls require a scan.
    indexed: bool,
}

#[cfg(test)]
pub(crate) const MAP_KEY_WORDS: usize = 0;
#[cfg(test)]
pub(crate) const MAP_KEY_STRING: usize = 1;

unsafe fn map_root<'a>(object: *mut u8) -> Option<&'a MapRoot> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::Map as u8 || header.payload_size != size_of::<MapRoot>() {
        return None;
    }
    Some(&*object.cast::<MapRoot>())
}

pub(crate) fn drop_map_object(object: *mut u8, header: &ObjectHeader) {
    if header.kind == RuntimeValueKind::Map as u8 && header.payload_size == size_of::<MapRoot>() {
        unsafe { std::ptr::drop_in_place(object.cast::<MapRoot>()) };
    }
}

fn allocate_root(node: Arc<Node>, indexed: bool) -> *mut u8 {
    let object = jk_alloc_object(
        RuntimeValueKind::Map as u8,
        size_of::<MapRoot>(),
        align_of::<MapRoot>(),
    );
    if !object.is_null() {
        unsafe { object.cast::<MapRoot>().write(MapRoot { node, indexed }) };
    }
    object
}

pub(crate) extern "C" fn jk_map_insert(
    map: *mut u8,
    words: *const u64,
    word_count: usize,
    masks: *const u64,
    masks_count: usize,
    key_word_count: usize,
    key_kind: usize,
) -> *mut u8 {
    if words.is_null()
        || masks.is_null()
        || key_word_count == 0
        || key_word_count >= word_count
        || mask_count(word_count) != Some(masks_count)
    {
        return std::ptr::null_mut();
    }
    let words = unsafe { std::slice::from_raw_parts(words, word_count) };
    let masks = unsafe { std::slice::from_raw_parts(masks, masks_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(&words[..key_word_count]);
    let entry = Arc::new(Entry {
        hash,
        key_words: key_word_count,
        ops,
        words: words.into(),
        masks: masks.into(),
    });
    let old = unsafe { map_root(map) };
    let indexed = ops.hashed() && old.is_none_or(|root| root.indexed);
    let replaced = old.and_then(|root| {
        trie::find(
            &root.node,
            &entry.words[..key_word_count],
            ops,
            hash,
            indexed,
            0,
        )
    });
    let node = if let Some(replaced) = replaced.filter(|old| old.hash != hash) {
        // Only legacy/generated descriptor mixing can move an equal key to another hash path.
        let removed = trie::remove(&old.unwrap().node, replaced, 0);
        trie::insert(removed.as_ref(), entry, None, 0)
    } else {
        trie::insert(old.map(|root| &root.node), entry, replaced, 0)
    };
    let output = allocate_root(node, indexed);
    jk_drop(map);
    output
}

pub(crate) extern "C" fn jk_map_remove(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
) -> *mut u8 {
    if key.is_null() || key_word_count == 0 {
        return std::ptr::null_mut();
    }
    let key = unsafe { std::slice::from_raw_parts(key, key_word_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key);
    let Some(root) = (unsafe { map_root(map) }) else {
        jk_drop(map);
        return std::ptr::null_mut();
    };
    let Some(entry) = trie::find(&root.node, key, ops, hash, root.indexed && ops.hashed(), 0)
    else {
        return map;
    };
    let output = trie::remove(&root.node, entry, 0).map_or(std::ptr::null_mut(), |node| {
        allocate_root(node, root.indexed)
    });
    jk_drop(map);
    output
}

pub(crate) extern "C" fn jk_map_get(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
    output: *mut u64,
    output_count: usize,
) -> u8 {
    if key.is_null() || output.is_null() || key_word_count == 0 || output_count == 0 {
        return 0;
    }
    unsafe { std::ptr::write_bytes(output, 0, output_count) };
    let key = unsafe { std::slice::from_raw_parts(key, key_word_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key);
    let Some(root) = (unsafe { map_root(map) }) else {
        return 0;
    };
    let Some(entry) = trie::find(&root.node, key, ops, hash, root.indexed && ops.hashed(), 0)
    else {
        return 0;
    };
    let value = &entry.words[entry.key_words..];
    if value.len() != output_count {
        return 0;
    }
    let Some(cloned) = clone_managed_words(value, &entry.masks, entry.key_words) else {
        return 0;
    };
    unsafe { std::ptr::copy_nonoverlapping(cloned.as_ptr(), output, output_count) };
    1
}

pub(crate) extern "C" fn jk_map_contains_key(
    map: *mut u8,
    key: *const u64,
    key_word_count: usize,
    key_kind: usize,
) -> u8 {
    if key.is_null() || key_word_count == 0 {
        return 0;
    }
    let key = unsafe { std::slice::from_raw_parts(key, key_word_count) };
    let ops = unsafe { KeyOps::from_abi(key_kind) };
    let hash = ops.hash(key);
    let Some(root) = (unsafe { map_root(map) }) else {
        return 0;
    };
    trie::find(&root.node, key, ops, hash, root.indexed && ops.hashed(), 0).is_some() as u8
}

pub(crate) extern "C" fn jk_map_length(map: *mut u8) -> usize {
    unsafe { map_root(map) }.map_or(0, |root| root.node.length)
}

/// Uniquely owned cursor over a persistent map trie: a retained map reference
/// plus a DFS frame stack whose depth is bounded by the trie height. `advance`
/// rewinds and refills the stack in place, so traversal allocates only when
/// the stack itself must grow. Iteration order follows trie structure (hash
/// order), which is stable within a process run only.
struct Frame {
    node: Arc<Node>,
    child: usize,
}

struct MapCursorState {
    map: *mut u8,
    frames: Vec<Frame>,
    pending: Option<Arc<Entry>>,
}

impl Drop for MapCursorState {
    fn drop(&mut self) {
        if !self.map.is_null() {
            jk_drop(self.map);
        }
    }
}

unsafe fn cursor_state<'a>(object: *mut u8) -> Option<&'a mut MapCursorState> {
    let header = valid_header(object)?;
    if header.kind != RuntimeValueKind::MapCursor as u8
        || header.payload_size != size_of::<MapCursorState>()
    {
        return None;
    }
    unsafe { object.cast::<MapCursorState>().as_mut() }
}

unsafe extern "C" fn drop_map_cursor(pointer: *mut u8) {
    unsafe { pointer.cast::<MapCursorState>().drop_in_place() };
}

/// Leaf children are yielded directly and never occupy a frame.
fn descend(state: &mut MapCursorState, node: Arc<Node>) {
    if let Kind::Leaf(entry) = &node.kind {
        state.pending = Some(entry.clone());
        return;
    }
    state.frames.push(Frame { node, child: 0 });
}

fn find_next(state: &mut MapCursorState) {
    enum Step {
        Pop,
        Descend(Arc<Node>),
    }
    state.pending = None;
    loop {
        let Some(frame) = state.frames.last_mut() else {
            return;
        };
        let step = match &frame.node.kind {
            Kind::Leaf(_) => Step::Pop,
            Kind::Collision { entries, .. } => {
                if frame.child < entries.len() {
                    state.pending = Some(entries[frame.child].clone());
                    frame.child += 1;
                    return;
                }
                Step::Pop
            }
            Kind::Branch { children, .. } => match children.get(frame.child) {
                Some(child) => {
                    frame.child += 1;
                    Step::Descend(child.clone())
                }
                None => Step::Pop,
            },
        };
        match step {
            Step::Pop => {
                state.frames.pop();
            }
            Step::Descend(child) => {
                descend(state, child);
                if state.pending.is_some() {
                    return;
                }
            }
        }
    }
}

/// Borrows the caller's Map reference and retains an independent one for the
/// cursor, keeping the whole trie alive until the cursor is exhausted or
/// dropped. Returns null for invalid or empty (null) maps; advancing such a
/// cursor immediately reports exhaustion.
pub(crate) unsafe extern "C" fn jk_map_cursor_new(map: *mut u8) -> *mut u8 {
    let Some(root) = (unsafe { map_root(map) }) else {
        return std::ptr::null_mut();
    };
    let handle = jk_dup(map);
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let pointer = allocate_object(
        RuntimeValueKind::MapCursor,
        size_of::<MapCursorState>(),
        align_of::<MapCursorState>(),
        Some(drop_map_cursor),
    );
    if pointer.is_null() {
        jk_drop(handle);
        return pointer;
    }
    let mut state = MapCursorState {
        map: handle,
        frames: Vec::new(),
        pending: None,
    };
    let node = root.node.clone();
    descend(&mut state, node);
    if state.pending.is_none() {
        find_next(&mut state);
    }
    unsafe { pointer.cast::<MapCursorState>().write(state) };
    pointer
}

/// Consumes the caller's cursor reference. On success writes the requested
/// projection (with independent references for shared words) and returns the
/// successor cursor, which is the same object with the stack advanced; null
/// means exhaustion and releases the cursor together with its map reference.
pub(crate) unsafe extern "C" fn jk_map_cursor_step(
    object: *mut u8,
    projection: usize,
    key_output: *mut u64,
    key_count: usize,
    value_output: *mut u64,
    value_count: usize,
) -> *mut u8 {
    let Some(state) = (unsafe { cursor_state(object) }) else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    let Some(entry) = state.pending.clone() else {
        jk_drop(object);
        return std::ptr::null_mut();
    };
    // Projection: 0 full entry, 1 key only, 2 value only.
    let wants_key = projection != 2;
    let wants_value = projection != 1;
    let key = &entry.words[..entry.key_words];
    let value = &entry.words[entry.key_words..];
    let mut released = false;
    if wants_key && !key_output.is_null() && key.len() == key_count {
        if let Some(cloned) = clone_managed_words(key, &entry.masks, 0) {
            unsafe { std::ptr::copy_nonoverlapping(cloned.as_ptr(), key_output, key_count) };
        } else {
            released = true;
        }
    } else if wants_key {
        released = true;
    }
    if wants_value && !value_output.is_null() && value.len() == value_count {
        if let Some(cloned) = clone_managed_words(value, &entry.masks, entry.key_words) {
            unsafe { std::ptr::copy_nonoverlapping(cloned.as_ptr(), value_output, value_count) };
        } else {
            released = true;
        }
    } else if wants_value {
        released = true;
    }
    if released {
        // A malformed projection must not hand out partially owned words; the
        // payload stays with the entry, which keeps its own references.
        jk_drop(object);
        return std::ptr::null_mut();
    }
    find_next(state);
    object
}

/// Supported structural keys contain only scalar words and String pairs.
/// The ownership mask marks each String pointer, followed by its byte length.
pub(super) fn structural_keys_equal(stored: &[u64], masks: &[u64], key: &[u64]) -> bool {
    if stored.len() != key.len() {
        return false;
    }
    let mut index = 0;
    while index < key.len() {
        if masks
            .get(index / 64)
            .is_some_and(|mask| mask & (1_u64 << (index % 64)) != 0)
        {
            if index + 1 >= key.len()
                || jk_string_eq(
                    stored[index] as *mut u8,
                    stored[index + 1] as usize,
                    key[index] as *mut u8,
                    key[index + 1] as usize,
                ) == 0
            {
                return false;
            }
            index += 2;
        } else {
            if stored[index] != key[index] {
                return false;
            }
            index += 1;
        }
    }
    true
}
