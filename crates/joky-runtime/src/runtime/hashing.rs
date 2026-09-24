//! Borrowed hashing state and type-specific map key operations.
use super::managed::{jk_alloc_object, valid_header, RuntimeValueKind};
use std::collections::hash_map::{DefaultHasher, RandomState};
use std::hash::{BuildHasher, Hasher};
use std::mem::{align_of, size_of};
use std::sync::OnceLock;

pub(crate) extern "C" fn jk_hasher_new() -> *mut u8 {
    static SEED: OnceLock<RandomState> = OnceLock::new();
    let object = jk_alloc_object(
        RuntimeValueKind::Hasher as u8,
        size_of::<DefaultHasher>(),
        align_of::<DefaultHasher>(),
    );
    if !object.is_null() {
        unsafe {
            object
                .cast::<DefaultHasher>()
                .write(SEED.get_or_init(RandomState::new).build_hasher())
        };
    }
    object
}

unsafe fn state<'a>(object: *mut u8) -> Option<&'a mut DefaultHasher> {
    let header = valid_header(object)?;
    (header.kind == RuntimeValueKind::Hasher as u8
        && header.payload_size == size_of::<DefaultHasher>())
    .then(|| &mut *object.cast::<DefaultHasher>())
}

pub(crate) extern "C" fn jk_hasher_word(object: *mut u8, word: u64) {
    if let Some(state) = unsafe { state(object) } {
        state.write(&word.to_le_bytes());
    }
}

pub(crate) extern "C" fn jk_hasher_string(object: *mut u8, text: *const u8, length: usize) {
    if let Some(state) = unsafe { state(object) } {
        state.write(&(length as u64).to_le_bytes());
        if length != 0 && !text.is_null() {
            state.write(unsafe { std::slice::from_raw_parts(text, length) });
        }
    }
}

pub(crate) extern "C" fn jk_hasher_finish(object: *mut u8) -> u64 {
    unsafe { state(object) }.map_or(0, |state| state.finish())
}

pub(crate) extern "C" fn jk_hasher_bytes(object: *mut u8, bytes: *mut u8) {
    jk_hasher_string(
        object,
        super::bytes::jk_bytes_data(bytes),
        super::bytes::jk_bytes_length(bytes),
    );
}

pub(crate) type KeyHash = unsafe extern "C" fn(*const u64) -> u64;
pub(crate) type KeyEqual = unsafe extern "C" fn(*const u64, *const u64) -> u8;

// Generated code passes this descriptor only for the duration of a runtime call.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct KeyCallbacks {
    pub hash: KeyHash,
    pub equal: KeyEqual,
}

#[derive(Clone, Copy)]
pub(crate) struct KeyOps {
    legacy: usize,
    callbacks: Option<KeyCallbacks>,
}

impl KeyOps {
    pub(crate) unsafe fn from_abi(value: usize) -> Self {
        if value <= 1 {
            Self {
                legacy: value,
                callbacks: None,
            }
        } else {
            Self {
                legacy: 0,
                callbacks: Some((value as *const KeyCallbacks).read()),
            }
        }
    }
    pub(crate) fn hashed(self) -> bool {
        self.callbacks.is_some()
    }
    pub(crate) fn hash(self, words: &[u64]) -> u64 {
        self.callbacks
            .map_or(0, |callbacks| unsafe { (callbacks.hash)(words.as_ptr()) })
    }
    pub(crate) fn equal(self, stored: &[u64], masks: &[u64], key: &[u64]) -> bool {
        if stored.len() != key.len() {
            return false;
        }
        if let Some(callbacks) = self.callbacks {
            return unsafe { (callbacks.equal)(stored.as_ptr(), key.as_ptr()) != 0 };
        }
        if self.legacy == 1 {
            return key.len() == 2
                && super::managed::jk_string_eq(
                    stored[0] as *mut u8,
                    stored[1] as usize,
                    key[0] as *mut u8,
                    key[1] as usize,
                ) != 0;
        }
        super::map::structural_keys_equal(stored, masks, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{managed::jk_drop, map::*, mut_map::*};
    use std::cell::Cell;

    thread_local! {
        static HASH_CALLS: Cell<usize> = const { Cell::new(0) };
        static EQ_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    unsafe extern "C" fn hash(key: *const u64) -> u64 {
        HASH_CALLS.set(HASH_CALLS.get() + 1);
        *key % 2
    }
    unsafe extern "C" fn equal(left: *const u64, right: *const u64) -> u8 {
        EQ_CALLS.set(EQ_CALLS.get() + 1);
        u8::from(*left == *right)
    }
    fn callbacks() -> KeyCallbacks {
        KeyCallbacks { hash, equal }
    }

    #[test]
    fn hasher_uses_content_and_frames_strings() {
        let a = jk_hasher_new();
        let b = jk_hasher_new();
        let c = jk_hasher_new();
        let allocated = String::from("ab");
        jk_hasher_string(a, allocated.as_ptr(), allocated.len());
        jk_hasher_string(a, b"c".as_ptr(), 1);
        jk_hasher_string(b, b"ab".as_ptr(), 2);
        jk_hasher_string(b, b"c".as_ptr(), 1);
        jk_hasher_string(c, b"a".as_ptr(), 1);
        jk_hasher_string(c, b"bc".as_ptr(), 2);
        assert_eq!(jk_hasher_finish(a), jk_hasher_finish(b));
        assert_ne!(jk_hasher_finish(a), jk_hasher_finish(c));
        jk_hasher_word(a, 42);
        jk_hasher_word(b, 42);
        assert_eq!(jk_hasher_finish(a), jk_hasher_finish(b));
        for value in [a, b, c] {
            jk_drop(value);
        }
    }

    #[test]
    fn maps_cache_hashes_filter_collisions_and_copy_descriptors() {
        HASH_CALLS.set(0);
        EQ_CALLS.set(0);
        let mut map = std::ptr::null_mut();
        let mutable = {
            let descriptor = callbacks();
            jk_mut_map_new(2, 1, 1, 1, &descriptor as *const _ as usize)
        };
        let masks = [0];
        let mut out = [0];
        for id in [1, 2, 3] {
            let descriptor = callbacks();
            let abi = &descriptor as *const _ as usize;
            let words = [id, 99, id * 10];
            map = jk_map_insert(map, words.as_ptr(), 3, masks.as_ptr(), 1, 2, abi);
            assert_eq!(
                jk_mut_map_insert(
                    mutable,
                    words.as_ptr(),
                    2,
                    masks.as_ptr(),
                    1,
                    words[2..].as_ptr(),
                    1,
                    masks.as_ptr(),
                    1,
                    out.as_mut_ptr(),
                    1
                ),
                0
            );
        }
        assert_eq!(HASH_CALLS.get(), 6, "retained entries must not be rehashed");
        assert_eq!(EQ_CALLS.get(), 2, "different hashes must skip equality");
        let descriptor = callbacks();
        let abi = &descriptor as *const _ as usize;
        for id in [1, 2, 3] {
            let key = [id, 0];
            assert_eq!(
                jk_map_get(map, key.as_ptr(), 2, abi, out.as_mut_ptr(), 1),
                1
            );
            assert_eq!(out, [id * 10]);
            assert_eq!(
                jk_mut_map_get(mutable, key.as_ptr(), 2, abi, out.as_mut_ptr(), 1),
                1
            );
            assert_eq!(out, [id * 10]);
        }
        let missing = [5, 0];
        assert_eq!(jk_map_contains_key(map, missing.as_ptr(), 2, abi), 0);
        assert_eq!(
            jk_mut_map_contains_key(mutable, missing.as_ptr(), 2, abi),
            0
        );
        let key = [1, 0];
        map = jk_map_remove(map, key.as_ptr(), 2, abi);
        assert_eq!(
            jk_mut_map_remove(mutable, key.as_ptr(), 2, abi, out.as_mut_ptr(), 1),
            1
        );
        assert_eq!(out, [10]);
        assert_eq!(jk_map_length(map), 2);
        assert_eq!(jk_mut_map_length(mutable), 2);
        jk_drop(map);
        jk_drop(mutable);
    }
}
