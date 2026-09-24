use super::*;
use crate::runtime::hashing::{KeyCallbacks, KeyHash};
use std::cell::Cell;
use std::collections::BTreeMap;

thread_local! {
    static HASH_CALLS: Cell<usize> = const { Cell::new(0) };
    static EQ_CALLS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn hash(key: *const u64) -> u64 {
    HASH_CALLS.set(HASH_CALLS.get() + 1);
    *key
}
unsafe extern "C" fn collision_hash(_: *const u64) -> u64 {
    0
}
unsafe extern "C" fn same_bucket_hash(key: *const u64) -> u64 {
    *key << 32
}
unsafe extern "C" fn equal(left: *const u64, right: *const u64) -> u8 {
    EQ_CALLS.set(EQ_CALLS.get() + 1);
    u8::from(*left == *right)
}

fn insert(map: *mut u8, key: u64, value: u64) -> Option<u64> {
    let masks = [0];
    let mut output = 0;
    let found = jk_mut_map_insert(
        map,
        &key,
        1,
        masks.as_ptr(),
        1,
        &value,
        1,
        masks.as_ptr(),
        1,
        &mut output,
        1,
    );
    (found != 0).then_some(output)
}

fn get(map: *mut u8, key: u64, ops: usize, remove: bool) -> Option<u64> {
    let mut output = 0;
    let found = if remove {
        jk_mut_map_remove(map, &key, 1, ops, &mut output, 1)
    } else {
        jk_mut_map_get(map, &key, 1, ops, &mut output, 1)
    };
    (found != 0).then_some(output)
}

fn check_index(map: *mut u8) {
    let (node, entries) = unsafe { node(map).unwrap() };
    let mut seen = vec![false; node.length];
    for b in 0..node.capacity {
        let mut index = unsafe { *node.buckets.add(b) };
        let mut previous = NO_ENTRY;
        while index != NO_ENTRY {
            assert!(index < node.length);
            assert!(!seen[index], "cycle or duplicate index {index}");
            seen[index] = true;
            let entry = entries[index];
            assert_eq!(entry.occupied, 1);
            assert_eq!(bucket(entry.hash, node.capacity), b);
            assert_eq!(entry.previous, previous);
            previous = index;
            index = entry.next;
        }
    }
    assert!(seen.iter().all(|value| *value));
    assert!(entries[node.length..]
        .iter()
        .all(|entry| entry.occupied == 0));
}

#[test]
fn growth_reuses_hashes_and_replacement_keeps_capacity() {
    HASH_CALLS.set(0);
    EQ_CALLS.set(0);
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let map = jk_mut_map_new(1, 1, 1, 1, ops);
    assert_eq!(get(map, 0, ops, false), None);
    HASH_CALLS.set(0);
    for i in 0..256 {
        assert_eq!(insert(map, i, i + 1), None);
        assert_eq!(HASH_CALLS.get(), i as usize + 1);
        assert_eq!(EQ_CALLS.get(), 0);
        check_index(map);
    }
    let capacity = jk_mut_map_capacity(map);
    assert_eq!(capacity, 256);
    assert_eq!(insert(map, 0, 9), Some(1));
    assert_eq!(jk_mut_map_capacity(map), capacity);
    assert_eq!(EQ_CALLS.get(), 1);
    assert_eq!(get(map, 256, ops, false), None);
    assert_eq!(
        EQ_CALLS.get(),
        1,
        "bucket collision alone must not invoke equality"
    );
    assert_eq!(get(map, 0, ops, true), Some(9));
    assert_eq!(jk_mut_map_capacity(map), capacity);
    assert_eq!(insert(map, 256, 10), None);
    assert_eq!(jk_mut_map_capacity(map), capacity);
    check_index(map);
    jk_drop(map);
}

#[test]
fn indexed_operations_match_a_reference_map_through_collision_and_deletion_churn() {
    for hash in [hash as KeyHash, collision_hash, same_bucket_hash] {
        let callbacks = KeyCallbacks { hash, equal };
        let ops = &callbacks as *const _ as usize;
        let map = jk_mut_map_new(1, 1, 1, 1, ops);
        let mut reference = BTreeMap::new();
        // Include full chains before deleting their head, middle, tail and moved last entry.
        for key in 0..129 {
            assert_eq!(insert(map, key, key), reference.insert(key, key));
        }
        for key in [128, 64, 0, 127, 1, 126, 63] {
            assert_eq!(get(map, key, ops, true), reference.remove(&key));
            check_index(map);
        }
        let mut random = 42_u64;
        for step in 0..4000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = (random >> 32) % 257;
            match (random >> 16) % 4 {
                0 | 1 => assert_eq!(insert(map, key, step), reference.insert(key, step)),
                2 => assert_eq!(get(map, key, ops, true), reference.remove(&key)),
                _ => {
                    assert_eq!(get(map, key, ops, false), reference.get(&key).copied());
                    assert_eq!(
                        jk_mut_map_contains_key(map, &key, 1, ops) != 0,
                        reference.contains_key(&key)
                    );
                }
            }
            assert_eq!(jk_mut_map_length(map), reference.len());
            check_index(map);
        }
        for key in 0..257 {
            assert_eq!(get(map, key, ops, true), reference.remove(&key));
            check_index(map);
        }
        assert_eq!(jk_mut_map_is_empty(map), 1);
        assert_eq!(insert(map, 300, 42), None);
        assert_eq!(get(map, 300, ops, false), Some(42));
        check_index(map);
        jk_drop(map);
    }
}

#[test]
fn legacy_and_indexed_lookup_descriptors_remain_compatible() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    for (create, query) in [(ops, 0), (0, ops)] {
        let map = jk_mut_map_new(1, 1, 1, 1, create);
        for i in 0..16 {
            insert(map, i, i + 1);
        }
        assert_eq!(get(map, 7, query, false), Some(8));
        assert_eq!(get(map, 7, query, true), Some(8));
        assert_eq!(get(map, 7, query, false), None);
        check_index(map);
        jk_drop(map);
    }
}

#[test]
fn indexed_growth_replacement_and_removal_release_shared_fields() {
    let baseline = live_object_count();
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let map = jk_mut_map_new(3, 1, 2, 1, ops);
    let mut output = [0_u64; 2];
    for pass in 0..2 {
        for id in 0..128 {
            let text = format!("entry-{pass}-{id}");
            let label = jk_string_from_utf8(text.as_ptr(), text.len());
            let value = jk_string_from_utf8(text.as_ptr(), text.len());
            let key = [id, label as u64, text.len() as u64];
            let words = [value as u64, text.len() as u64];
            assert_eq!(
                jk_mut_map_insert(
                    map,
                    key.as_ptr(),
                    3,
                    [2].as_ptr(),
                    1,
                    words.as_ptr(),
                    2,
                    [1].as_ptr(),
                    1,
                    output.as_mut_ptr(),
                    2
                ),
                pass
            );
            if pass != 0 {
                jk_drop(output[0] as *mut u8);
            }
        }
    }
    check_index(map);
    for id in (0..128).step_by(3) {
        let key = [id, 0, 0];
        assert_eq!(
            jk_mut_map_remove(map, key.as_ptr(), 3, ops, output.as_mut_ptr(), 2),
            1
        );
        jk_drop(output[0] as *mut u8);
        check_index(map);
    }
    jk_drop(map);
    assert_eq!(live_object_count(), baseline);
}
