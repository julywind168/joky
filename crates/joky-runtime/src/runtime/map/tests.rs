use super::*;
use crate::runtime::hashing::{KeyCallbacks, KeyHash};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use trie::Kind;

thread_local! {
    static HASH_CALLS: Cell<usize> = const { Cell::new(0) };
    static EQ_CALLS: Cell<usize> = const { Cell::new(0) };
}
unsafe extern "C" fn hash(key: *const u64) -> u64 {
    HASH_CALLS.set(HASH_CALLS.get() + 1);
    *key
}
unsafe extern "C" fn collision(_: *const u64) -> u64 {
    0
}
unsafe extern "C" fn prefix(key: *const u64) -> u64 {
    *key << 54
}
unsafe extern "C" fn grouped(key: *const u64) -> u64 {
    (*key / 2) << 60
}
unsafe extern "C" fn equal(a: *const u64, b: *const u64) -> u8 {
    EQ_CALLS.set(EQ_CALLS.get() + 1);
    u8::from(*a == *b)
}
fn insert(map: *mut u8, key: u64, value: u64, ops: usize) -> *mut u8 {
    jk_map_insert(map, [key, value].as_ptr(), 2, [0].as_ptr(), 1, 1, ops)
}
fn get(map: *mut u8, key: u64, ops: usize) -> Option<u64> {
    let mut output = 0;
    (jk_map_get(map, &key, 1, ops, &mut output, 1) != 0).then_some(output)
}

fn inspect(node: &Arc<Node>, shift: u32, path: &[(u32, u32)], seen: &mut HashSet<usize>) -> usize {
    assert!(
        seen.insert(Arc::as_ptr(node) as usize),
        "duplicate subtree in one version"
    );
    let check = |entry: &Entry| {
        for (shift, branch) in path {
            assert_eq!((entry.hash >> shift) & 31, u64::from(*branch));
        }
    };
    let length = match &node.kind {
        Kind::Leaf(entry) => {
            check(entry);
            1
        }
        Kind::Collision { hash, entries } => {
            assert!(entries.len() >= 2);
            for entry in entries {
                assert_eq!(entry.hash, *hash);
                check(entry);
            }
            entries.len()
        }
        Kind::Branch { bitmap, children } => {
            assert!(shift < 64);
            assert_ne!(*bitmap, 0);
            assert_eq!(bitmap.count_ones() as usize, children.len());
            if children.len() == 1 {
                assert!(matches!(children[0].kind, Kind::Branch { .. }));
            }
            let mut length = 0;
            for (child, branch) in children
                .iter()
                .zip((0..32).filter(|b| bitmap & (1 << b) != 0))
            {
                let mut path = path.to_vec();
                path.push((shift, branch));
                length += inspect(child, shift + 5, &path, seen);
            }
            length
        }
    };
    assert_eq!(node.length, length);
    length
}

fn verify(map: *mut u8, expected: &BTreeMap<u64, u64>, ops: usize) {
    assert_eq!(jk_map_length(map), expected.len());
    assert_eq!(map.is_null(), expected.is_empty());
    if let Some(root) = unsafe { map_root(map) } {
        inspect(&root.node, 0, &[], &mut HashSet::new());
    }
    for key in 0..96 {
        assert_eq!(get(map, key, ops), expected.get(&key).copied());
        assert_eq!(
            jk_map_contains_key(map, &key, 1, ops) != 0,
            expected.contains_key(&key)
        );
    }
}

#[test]
fn versions_match_reference_maps_with_collisions_and_long_prefixes() {
    let baseline = live_object_count();
    for hash in [hash as KeyHash, collision, prefix] {
        let callbacks = KeyCallbacks { hash, equal };
        let ops = &callbacks as *const _ as usize;
        let mut map = std::ptr::null_mut();
        let mut expected = BTreeMap::new();
        let mut versions = Vec::new();
        let mut random = 42_u64;
        for step in 0..1000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = (random >> 32) % 96;
            if step % 29 == 0 {
                versions.push((jk_dup(map), expected.clone()));
            }
            if random & 3 == 0 {
                map = jk_map_remove(map, &key, 1, ops);
                expected.remove(&key);
            } else {
                map = insert(map, key, step, ops);
                expected.insert(key, step);
            }
            if step % 17 == 0 {
                verify(map, &expected, ops);
            }
        }
        verify(map, &expected, ops);
        for key in 0..96 {
            map = jk_map_remove(map, &key, 1, ops);
        }
        assert!(map.is_null());
        // Snapshots survive deletion of the latest version, in arbitrary release order.
        for (old, expected) in versions.into_iter().rev() {
            verify(old, &expected, ops);
            jk_drop(old);
        }
    }
    assert_eq!(live_object_count(), baseline);
}

#[test]
fn final_hash_bits_and_collision_collapse_preserve_trie_depth() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let keys = [0, 1_u64 << 60, 1_u64 << 63, u64::MAX, 31, 32];
    let mut map = std::ptr::null_mut();
    for key in keys {
        map = insert(map, key, key, ops);
    }
    let original = jk_dup(map);
    for key in keys {
        assert_eq!(get(map, key, ops), Some(key));
        map = jk_map_remove(map, &key, 1, ops);
        if let Some(root) = unsafe { map_root(map) } {
            inspect(&root.node, 0, &[], &mut HashSet::new());
        }
    }
    assert!(map.is_null());
    for key in keys {
        assert_eq!(get(original, key, ops), Some(key));
    }
    jk_drop(original);

    // Collision nodes split at the last hash level, then collapse back to a leaf.
    let callbacks = KeyCallbacks {
        hash: grouped,
        equal,
    };
    let ops = &callbacks as *const _ as usize;
    let mut map = std::ptr::null_mut();
    for key in 0..6 {
        map = insert(map, key, key, ops);
    }
    let original = jk_dup(map);
    map = insert(map, 1, 99, ops);
    for key in [0, 4, 2, 3, 5, 1] {
        assert_eq!(get(map, key, ops), Some(if key == 1 { 99 } else { key }));
        map = jk_map_remove(map, &key, 1, ops);
        if let Some(root) = unsafe { map_root(map) } {
            inspect(&root.node, 0, &[], &mut HashSet::new());
        }
    }
    assert!(map.is_null());
    for key in 0..6 {
        assert_eq!(get(original, key, ops), Some(key));
    }
    jk_drop(original);
}

#[test]
fn updates_share_untouched_nodes_and_release_all_arcs() {
    HASH_CALLS.set(0);
    EQ_CALLS.set(0);
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let mut map = std::ptr::null_mut();
    for key in 0..1024 {
        map = insert(map, key, key, ops);
    }
    assert_eq!(HASH_CALLS.get(), 1024);
    assert_eq!(EQ_CALLS.get(), 0);
    let root = unsafe { map_root(map).unwrap() };
    let mut old_nodes = HashSet::new();
    inspect(&root.node, 0, &[], &mut old_nodes);
    let weak_root = Arc::downgrade(&root.node);
    let old_entry = trie::find(&root.node, &[0], root_ops(ops), 0, true, 0).unwrap();
    let weak_entry = Arc::downgrade(old_entry);
    EQ_CALLS.set(0);
    let changed = insert(jk_dup(map), 0, 9999, ops);
    let new_root = unsafe { map_root(changed).unwrap() };
    let weak_new = Arc::downgrade(&new_root.node);
    fn weak_nodes(node: &Arc<Node>, output: &mut Vec<std::sync::Weak<Node>>) {
        output.push(Arc::downgrade(node));
        if let Kind::Branch { children, .. } = &node.kind {
            for child in children {
                weak_nodes(child, output);
            }
        }
    }
    let mut all_nodes = Vec::new();
    weak_nodes(&root.node, &mut all_nodes);
    weak_nodes(&new_root.node, &mut all_nodes);
    let mut new_nodes = HashSet::new();
    inspect(&new_root.node, 0, &[], &mut new_nodes);
    assert_eq!(
        new_nodes.difference(&old_nodes).count(),
        3,
        "two branches and one leaf copied"
    );
    assert_eq!(HASH_CALLS.get(), 1025);
    assert_eq!(EQ_CALLS.get(), 1);
    assert_eq!(get(map, 0, ops), Some(0));
    assert_eq!(get(changed, 0, ops), Some(9999));
    let missing = jk_map_remove(jk_dup(map), &2000, 1, ops);
    assert_eq!(missing, map, "absent removal should reuse the root");
    jk_drop(missing);
    jk_drop(map);
    assert!(weak_root.upgrade().is_none());
    assert!(weak_entry.upgrade().is_none());
    assert_eq!(get(changed, 999, ops), Some(999));
    jk_drop(changed);
    assert!(weak_new.upgrade().is_none());
    assert!(all_nodes.iter().all(|node| node.upgrade().is_none()));
}

#[test]
fn empty_lookups_still_dispatch_hash_once() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    HASH_CALLS.set(0);
    let empty = std::ptr::null_mut();
    assert_eq!(get(empty, 42, ops), None);
    assert_eq!(jk_map_contains_key(empty, &42, 1, ops), 0);
    assert!(jk_map_remove(empty, &42, 1, ops).is_null());
    assert_eq!(HASH_CALLS.get(), 3);
}

fn root_ops(ops: usize) -> KeyOps {
    unsafe { KeyOps::from_abi(ops) }
}

#[test]
fn mixed_legacy_descriptors_relocate_equal_keys_without_duplicates() {
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    for (first, second) in [(0, ops), (ops, 0)] {
        let mut map = std::ptr::null_mut();
        for key in 0..40 {
            map = insert(map, key, key, first);
        }
        let old = jk_dup(map);
        for key in 0..40 {
            map = insert(map, key, 99, second);
        }
        assert_eq!(jk_map_length(map), 40);
        for key in 0..40 {
            assert_eq!(get(map, key, first), Some(99));
            assert_eq!(get(map, key, second), Some(99));
            assert_eq!(get(old, key, first), Some(key));
            map = jk_map_remove(map, &key, 1, first);
        }
        assert!(map.is_null());
        jk_drop(old);
    }
}

#[test]
fn shared_payloads_live_until_the_last_version_and_lookup_are_dropped() {
    let baseline = live_object_count();
    let callbacks = KeyCallbacks { hash, equal };
    let ops = &callbacks as *const _ as usize;
    let mut map = std::ptr::null_mut();
    let mut versions = Vec::new();
    for pass in 0..3 {
        for id in 0..80 {
            let text = format!("payload-{pass}-{id}");
            let label = jk_string_from_utf8(text.as_ptr(), text.len());
            let value = jk_string_from_utf8(text.as_ptr(), text.len());
            map = jk_map_insert(
                map,
                [
                    id,
                    label as u64,
                    text.len() as u64,
                    value as u64,
                    text.len() as u64,
                ]
                .as_ptr(),
                5,
                [0b01010].as_ptr(),
                1,
                3,
                ops,
            );
        }
        versions.push(jk_dup(map));
    }
    let mut output = [0; 2];
    assert_eq!(
        jk_map_get(map, [7, 0, 0].as_ptr(), 3, ops, output.as_mut_ptr(), 2),
        1
    );
    jk_drop(map);
    for version in versions {
        jk_drop(version);
    }
    assert_eq!(live_object_count(), baseline + 1);
    jk_drop(output[0] as *mut u8);
    assert_eq!(live_object_count(), baseline);
}

#[test]
fn map_cursor_walks_all_shapes_projections_and_releases_resources() {
    let baseline = live_object_count();
    for hash in [hash as KeyHash, collision, prefix, grouped] {
        let callbacks = KeyCallbacks { hash, equal };
        let ops = &callbacks as *const _ as usize;
        let mut map = std::ptr::null_mut();
        let mut expected = BTreeMap::new();
        for key in 0..40u64 {
            map = insert(map, key, key * 10, ops);
            expected.insert(key, key * 10);
        }
        for (projection, wants_key, wants_value) in
            [(0, true, true), (1, true, false), (2, false, true)]
        {
            let mut cursor = unsafe { jk_map_cursor_new(map) };
            let mut seen = BTreeMap::new();
            let mut key_out = [0u64; 1];
            let mut value_out = [0u64; 1];
            loop {
                let next = unsafe {
                    jk_map_cursor_step(
                        cursor,
                        projection,
                        key_out.as_mut_ptr(),
                        1,
                        value_out.as_mut_ptr(),
                        1,
                    )
                };
                if next.is_null() {
                    break;
                }
                cursor = next;
                seen.insert(
                    if wants_key {
                        key_out[0]
                    } else {
                        u64::MAX - seen.len() as u64
                    },
                    if wants_value { value_out[0] } else { 0 },
                );
                // In-place advance: the successor is the same object.
                assert_eq!(next, cursor);
            }
            if wants_key && wants_value {
                assert_eq!(seen, expected);
            } else if wants_key {
                let keys = seen.keys().copied().collect::<Vec<_>>();
                assert_eq!(keys, expected.keys().copied().collect::<Vec<_>>());
            } else {
                let mut values = seen.values().copied().collect::<Vec<_>>();
                values.sort_unstable();
                let mut want = expected.values().copied().collect::<Vec<_>>();
                want.sort_unstable();
                assert_eq!(values, want);
            }
        }
        // An empty (null) map cursor is immediately exhausted.
        let mut key_out = [0u64; 1];
        let mut value_out = [0u64; 1];
        assert!(unsafe {
            jk_map_cursor_step(
                unsafe { jk_map_cursor_new(std::ptr::null_mut()) },
                0,
                key_out.as_mut_ptr(),
                1,
                value_out.as_mut_ptr(),
                1,
            )
        }
        .is_null());
        // The map itself stays usable after traversal.
        assert_eq!(jk_map_length(map), 40);
        jk_drop(map);
        assert_eq!(live_object_count(), baseline);
    }
}
