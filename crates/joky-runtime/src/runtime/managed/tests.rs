use super::*;
use crate::runtime::map::{
    jk_map_contains_key, jk_map_get, jk_map_insert, jk_map_length, jk_map_remove, MAP_KEY_STRING,
    MAP_KEY_WORDS,
};
use std::mem::{align_of, size_of};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;

static CLOSURE_CALLBACK_RAN: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn closure_drop_callback(_: *mut u8) {
    CLOSURE_CALLBACK_RAN.store(true, Ordering::Relaxed);
}

#[test]
fn header_layout_is_stable_and_aligned() {
    assert_eq!(align_of::<ObjectHeader>(), align_of::<usize>());
    assert_eq!(size_of::<ObjectHeader>() % align_of::<ObjectHeader>(), 0);
}

#[test]
fn alloc_dup_drop_round_trip() {
    let object = jk_alloc_object(RuntimeValueKind::String as u8, 7, 1);
    assert!(!object.is_null());
    let header = unsafe { valid_header(object).expect("valid managed object") };
    assert_eq!(header.payload_size, 7);
    assert_eq!(header.ref_count.load(Ordering::Relaxed), 1);
    assert_eq!(jk_dup(object), object);
    assert_eq!(header.ref_count.load(Ordering::Relaxed), 2);
    jk_drop(object);
    assert_eq!(header.ref_count.load(Ordering::Relaxed), 1);
    jk_drop(object);
}

#[test]
fn owned_class_cannot_be_duplicated() {
    let object = jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8);
    assert!(!object.is_null());
    let header = unsafe { valid_header(object).expect("valid managed object") };
    assert_eq!(header.ownership, OwnershipKind::Owned as u8);
    assert!(jk_dup(object).is_null());
    jk_drop(object);
}

#[test]
fn cown_payload_destructor_runs_once_when_region_closes_after_last_owner_thread() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _scope = scope.enter();
    #[repr(C)]
    struct Payload {
        drops: *const AtomicUsize,
        text: *mut u8,
    }
    unsafe extern "C" fn drop_payload(pointer: *mut u8) {
        let payload = unsafe { &*pointer.cast::<Payload>() };
        unsafe { &*payload.drops }.fetch_add(1, Ordering::SeqCst);
        jk_drop(payload.text);
        jk_drop(pointer);
    }

    let drops = AtomicUsize::new(0);
    let payload = jk_alloc_object(
        RuntimeValueKind::Class as u8,
        size_of::<Payload>(),
        align_of::<Payload>(),
    );
    unsafe {
        payload.cast::<Payload>().write(Payload {
            drops: &drops,
            text: jk_string_from_utf8(b"owned".as_ptr(), 5),
        });
    }
    let cown = jk_cown_new(payload, Some(drop_payload));
    let retained = jk_dup(cown) as usize;
    jk_drop(cown);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(live_object_count(), 3);
    thread::spawn(move || jk_drop(retained as *mut u8))
        .join()
        .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    scope.close_and_wait();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn acquire_many_is_atomic_and_releases_all_leases() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let first = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    let second = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    assert!(!first.is_null() && !second.is_null());
    let handles = [jk_dup(first), jk_dup(second)];
    assert!(handles.iter().all(|handle| !handle.is_null()));
    let mut payloads = [std::ptr::null_mut(); 2];
    assert_eq!(
        jk_cown_acquire_many(handles.as_ptr(), handles.len(), payloads.as_mut_ptr()),
        1
    );
    assert!(payloads.iter().all(|payload| !payload.is_null()));
    jk_cown_release(first);
    jk_cown_release(second);
    region_scope.close_and_wait();
}

#[test]
fn acquire_many_orders_by_stable_id_without_reverse_deadlock() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let first = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    let second = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    let barrier = std::sync::Arc::new(Barrier::new(2));
    let run = |left: *mut u8, right: *mut u8, barrier: std::sync::Arc<Barrier>| {
        let acquire = [jk_dup(left), jk_dup(right)];
        let release = [jk_dup(left), jk_dup(right)];
        let acquire_addresses = acquire.map(|value| value as usize);
        let release_addresses = release.map(|value| value as usize);
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            let mut payloads = [std::ptr::null_mut(); 2];
            assert_eq!(
                jk_cown_acquire_many(
                    acquire_addresses.as_ptr().cast(),
                    acquire_addresses.len(),
                    payloads.as_mut_ptr(),
                ),
                1
            );
            jk_cown_release(release_addresses[0] as *mut u8);
            jk_cown_release(release_addresses[1] as *mut u8);
        })
    };
    let left = run(first, second, barrier.clone());
    let right = run(second, first, barrier);
    left.join().unwrap();
    right.join().unwrap();
    jk_drop(first);
    jk_drop(second);
    region_scope.close_and_wait();
}

#[test]
fn contended_single_cown_waits_until_release() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let cown = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    let held = jk_dup(cown);
    let waiting = jk_dup(cown);
    let payload = jk_cown_acquire(held);
    assert!(!payload.is_null());
    let (sender, receiver) = mpsc::channel();
    let waiting_address = waiting as usize;
    let thread = std::thread::spawn(move || {
        let payload = jk_cown_acquire(waiting_address as *mut u8);
        sender
            .send(!payload.is_null())
            .expect("waiter should report");
        jk_cown_release(waiting_address as *mut u8);
    });
    assert!(receiver.recv_timeout(Duration::from_millis(10)).is_err());
    jk_cown_release(cown);
    assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
    thread.join().unwrap();
    region_scope.close_and_wait();
}

#[test]
fn contended_cown_allows_every_waiter_to_make_progress() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    // Exercise repeated handoff under contention. This checks that no waiter
    // can be starved indefinitely without asserting a scheduler-specific
    // acquisition order.
    const THREADS: usize = 8;
    const ROUNDS: usize = 32;
    let cown = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    assert!(!cown.is_null());
    let barrier = std::sync::Arc::new(Barrier::new(THREADS));
    let counts = std::sync::Arc::new(
        (0..THREADS)
            .map(|_| AtomicUsize::new(0))
            .collect::<Vec<_>>(),
    );
    let mut workers = Vec::with_capacity(THREADS);
    for index in 0..THREADS {
        let barrier = std::sync::Arc::clone(&barrier);
        let counts = std::sync::Arc::clone(&counts);
        let address = cown as usize;
        workers.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..ROUNDS {
                let acquire = jk_dup(address as *mut u8);
                let release = jk_dup(address as *mut u8);
                assert!(!acquire.is_null() && !release.is_null());
                assert!(!jk_cown_acquire(acquire).is_null());
                jk_cown_release(release);
                counts[index].fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for worker in workers {
        worker.join().expect("contended waiter should finish");
    }
    assert!(counts
        .iter()
        .all(|count| count.load(Ordering::Relaxed) == ROUNDS));
    jk_drop(cown);
    region_scope.close_and_wait();
}

#[test]
fn closure_environment_runs_its_drop_callback() {
    CLOSURE_CALLBACK_RAN.store(false, Ordering::Relaxed);
    let environment = jk_alloc_closure_environment(8, 8, Some(closure_drop_callback));
    assert!(!environment.is_null());
    jk_drop(environment);
    assert!(CLOSURE_CALLBACK_RAN.load(Ordering::Relaxed));
}

#[test]
fn payload_alignment_is_honored() {
    let object = jk_alloc_object(RuntimeValueKind::Class as u8, 32, 32);
    assert!(!object.is_null());
    let header = unsafe { valid_header(object).expect("valid managed object") };
    assert_eq!(object as usize % 32, 0);
    assert_eq!(header.payload_align, 32);
    jk_drop(object);
}

#[test]
fn null_and_invalid_inputs_are_no_ops() {
    assert!(jk_dup(std::ptr::null_mut()).is_null());
    jk_drop(std::ptr::null_mut());
    assert!(jk_alloc_object(0, 1, 1).is_null());
    assert!(jk_alloc_object(RuntimeValueKind::String as u8, 1, 3).is_null());
}

#[test]
fn cown_leases_are_mutually_exclusive() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let payload = jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8);
    let cown = jk_cown_new(payload, None);
    assert!(!cown.is_null());

    // Keep one capability for the first lease's release and reserve two
    // capabilities for the second lease's acquire/release pair.
    let first_release = jk_dup(cown);
    let second_acquire = jk_dup(first_release);
    let second_release = jk_dup(second_acquire);
    assert!(!first_release.is_null());
    assert!(!second_acquire.is_null());
    assert!(!second_release.is_null());

    let first_payload = jk_cown_acquire(cown);
    assert_eq!(first_payload, payload);
    let (sender, receiver) = mpsc::channel::<usize>();
    let second_acquire_address = second_acquire as usize;
    let second_release_address = second_release as usize;
    let contender = thread::spawn(move || {
        let payload = jk_cown_acquire(second_acquire_address as *mut u8);
        sender
            .send(payload as usize)
            .expect("contender should report acquire");
        jk_cown_release(second_release_address as *mut u8);
    });
    assert!(receiver.recv_timeout(Duration::from_millis(10)).is_err());

    jk_cown_release(first_release);
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("contender should acquire after release"),
        payload as usize
    );
    contender.join().expect("contender should finish");
    region_scope.close_and_wait();
}

#[test]
fn c_string_check_accepts_nul_free_payloads_and_null() {
    let hello = jk_string_from_utf8(b"hello".as_ptr(), 5);
    jk_string_c_string_check(hello, 5);
    let empty = jk_string_from_utf8(std::ptr::null(), 0);
    jk_string_c_string_check(empty, 0);
    jk_string_c_string_check(std::ptr::null(), 5);
    jk_drop(hello);
    jk_drop(empty);
}

#[test]
fn string_from_cstr_copies_valid_utf8_and_rejects_the_rest() {
    let value = "héllo wörld";
    let terminated = format!("{value}\0");
    let copied = jk_string_from_cstr(terminated.as_ptr().cast());
    assert!(!copied.is_null());
    assert_eq!(jk_string_len(copied), value.len());
    let expected = jk_string_from_utf8(value.as_ptr(), value.len());
    assert_eq!(jk_string_eq(copied, value.len(), expected, value.len()), 1);

    let source = jk_string_from_utf8(std::ptr::null(), 0);
    let empty_copy = jk_string_from_cstr(source);
    assert!(!empty_copy.is_null());
    assert_eq!(jk_string_len(empty_copy), 0);

    let invalid: &[u8] = &[b'c', 0xFF, b'd'];
    assert!(jk_string_from_cstr(invalid.as_ptr().cast()).is_null());
    assert!(jk_string_from_cstr(std::ptr::null()).is_null());
    jk_drop(copied);
    jk_drop(expected);
    jk_drop(empty_copy);
    jk_drop(source);
}

#[test]
fn string_operations_copy_compare_and_release() {
    let hello = jk_string_from_utf8(b"hello".as_ptr(), 5);
    let world = jk_string_from_utf8(b" world".as_ptr(), 6);
    assert_eq!(jk_string_len(hello), 5);
    assert_eq!(jk_string_len(world), 6);
    let joined = jk_string_concat(hello, 5, world, 6);
    assert_eq!(jk_string_len(joined), 11);
    let expected = jk_string_from_utf8(b"hello world".as_ptr(), 11);
    assert_eq!(jk_string_eq(joined, 11, expected, 11), 1);
    assert_eq!(jk_string_eq(joined, 10, expected, 11), 0);
    assert_eq!(live_object_count(), 4);
    jk_drop(hello);
    jk_drop(world);
    jk_drop(joined);
    jk_drop(expected);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn empty_string_and_invalid_string_inputs_are_safe() {
    let empty = jk_string_from_utf8(std::ptr::null(), 0);
    assert!(!empty.is_null());
    assert_eq!(jk_string_len(empty), 0);
    assert_eq!(jk_string_eq(empty, 0, empty, 0), 1);
    assert!(jk_string_concat(std::ptr::null_mut(), 0, empty, 0).is_null());
    assert_eq!(jk_string_len(std::ptr::null_mut()), 0);
    jk_drop(empty);
}

#[test]
fn string_search_operations_use_byte_content() {
    let value = jk_string_from_utf8("hello world".as_ptr(), 11);
    let hello = jk_string_from_utf8("hello".as_ptr(), 5);
    let world = jk_string_from_utf8("world".as_ptr(), 5);
    let nope = jk_string_from_utf8("nope".as_ptr(), 4);
    let empty = jk_string_from_utf8(std::ptr::null(), 0);

    assert_eq!(jk_string_starts_with(value, 11, hello, 5), 1);
    assert_eq!(jk_string_ends_with(value, 11, world, 5), 1);
    assert_eq!(jk_string_contains(value, 11, world, 5), 1);
    assert_eq!(jk_string_contains(value, 11, nope, 4), 0);
    assert_eq!(jk_string_contains(value, 11, empty, 0), 1);
    assert_eq!(jk_string_starts_with(value, 10, hello, 5), 0);

    for object in [value, hello, world, nope, empty] {
        jk_drop(object);
    }
}

#[test]
fn string_counts_use_unicode_scalar_and_grapheme_semantics() {
    let value = "e\u{301}😀";
    let object = jk_string_from_utf8(value.as_ptr(), value.len());
    assert_eq!(jk_string_scalar_count(object, value.len()), 3);
    assert_eq!(jk_string_grapheme_count(object, value.len()), 2);
    assert_eq!(jk_string_is_ascii(object, value.len()), 0);
    jk_drop(object);
}

#[test]
fn list_length_walks_persistent_nodes() {
    let empty = std::ptr::null_mut();
    let masks = [0];
    let first_words = [1];
    let second_words = [2];
    let third_words = [3];
    let first = jk_list_cons(first_words.as_ptr(), 1, masks.as_ptr(), 1, empty);
    let second = jk_list_cons(second_words.as_ptr(), 1, masks.as_ptr(), 1, first);
    let third = jk_list_cons(third_words.as_ptr(), 1, masks.as_ptr(), 1, second);
    let mut head = [0];

    assert_eq!(jk_list_length(std::ptr::null_mut()), 0);
    assert_eq!(jk_list_length(third), 3);
    assert_eq!(jk_list_head(third, head.as_mut_ptr(), head.len()), 1);
    assert_eq!(head, [3]);

    jk_drop(third);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn list_reverse_builds_a_new_persistent_chain() {
    let masks = [0];
    let first_words = [1];
    let second_words = [2];
    let third_words = [3];
    let first = jk_list_cons(
        first_words.as_ptr(),
        1,
        masks.as_ptr(),
        1,
        std::ptr::null_mut(),
    );
    let second = jk_list_cons(second_words.as_ptr(), 1, masks.as_ptr(), 1, first);
    let third = jk_list_cons(third_words.as_ptr(), 1, masks.as_ptr(), 1, second);
    let reversed = jk_list_reverse(third);
    let mut head = [0];

    assert_eq!(jk_list_length(third), 3);
    assert_eq!(jk_list_head(reversed, head.as_mut_ptr(), head.len()), 1);
    assert_eq!(head, [1]);
    let tail = jk_list_tail(reversed);
    assert_eq!(jk_list_head(tail, head.as_mut_ptr(), head.len()), 1);
    assert_eq!(head, [2]);
    let tail_tail = jk_list_tail(tail);
    assert_eq!(jk_list_head(tail_tail, head.as_mut_ptr(), head.len()), 1);
    assert_eq!(head, [3]);
    assert!(jk_list_tail(tail_tail).is_null());

    jk_drop(third);
    jk_drop(reversed);
    jk_drop(tail);
    jk_drop(tail_tail);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn list_manages_all_shared_words_in_composite_elements() {
    let first_value = "hello";
    let second_value = "world";
    let first = jk_string_from_utf8(first_value.as_ptr(), first_value.len());
    let second = jk_string_from_utf8(second_value.as_ptr(), second_value.len());
    let words = [
        7,
        first as u64,
        first_value.len() as u64,
        second as u64,
        second_value.len() as u64,
    ];
    let masks = [0b01010];
    let list = jk_list_cons(
        words.as_ptr(),
        words.len(),
        masks.as_ptr(),
        masks.len(),
        std::ptr::null_mut(),
    );
    let reversed = jk_list_reverse(list);
    let mut head = [0; 5];

    assert_eq!(jk_list_head(reversed, head.as_mut_ptr(), head.len()), 1);
    assert_eq!(head[0], 7);
    assert_eq!(jk_string_len(head[1] as *mut u8), first_value.len());
    assert_eq!(head[2], first_value.len() as u64);
    assert_eq!(jk_string_len(head[3] as *mut u8), second_value.len());
    assert_eq!(head[4], second_value.len() as u64);
    assert_eq!(live_object_count(), 4);

    jk_drop(list);
    jk_drop(reversed);
    assert_eq!(jk_string_len(head[1] as *mut u8), first_value.len());
    assert_eq!(jk_string_len(head[3] as *mut u8), second_value.len());
    jk_drop(head[1] as *mut u8);
    jk_drop(head[3] as *mut u8);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn map_replaces_and_removes_numeric_keys_persistently() {
    let masks = [0];
    let first_words = [7, 41];
    let replacement_words = [7, 42];
    let key = [7];
    let first = jk_map_insert(
        std::ptr::null_mut(),
        first_words.as_ptr(),
        first_words.len(),
        masks.as_ptr(),
        masks.len(),
        key.len(),
        MAP_KEY_WORDS,
    );
    let updated = jk_map_insert(
        jk_dup(first),
        replacement_words.as_ptr(),
        replacement_words.len(),
        masks.as_ptr(),
        masks.len(),
        key.len(),
        MAP_KEY_WORDS,
    );
    let mut value = [0];

    assert_eq!(jk_map_length(first), 1);
    assert_eq!(jk_map_length(updated), 1);
    assert_eq!(
        jk_map_get(
            first,
            key.as_ptr(),
            key.len(),
            MAP_KEY_WORDS,
            value.as_mut_ptr(),
            value.len(),
        ),
        1
    );
    assert_eq!(value, [41]);
    assert_eq!(
        jk_map_get(
            updated,
            key.as_ptr(),
            key.len(),
            MAP_KEY_WORDS,
            value.as_mut_ptr(),
            value.len(),
        ),
        1
    );
    assert_eq!(value, [42]);
    assert_eq!(
        jk_map_contains_key(updated, key.as_ptr(), key.len(), MAP_KEY_WORDS),
        1
    );

    let removed = jk_map_remove(jk_dup(updated), key.as_ptr(), key.len(), MAP_KEY_WORDS);
    assert_eq!(jk_map_length(removed), 0);

    jk_drop(first);
    jk_drop(updated);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn map_releases_managed_string_keys_and_values() {
    let key_text = "key";
    let value_text = "value";
    let key = jk_string_from_utf8(key_text.as_ptr(), key_text.len());
    let value = jk_string_from_utf8(value_text.as_ptr(), value_text.len());
    let words = [
        key as u64,
        key_text.len() as u64,
        value as u64,
        value_text.len() as u64,
    ];
    let masks = [0b0101];
    let map = jk_map_insert(
        std::ptr::null_mut(),
        words.as_ptr(),
        words.len(),
        masks.as_ptr(),
        masks.len(),
        2,
        MAP_KEY_STRING,
    );
    let lookup = [key as u64, key_text.len() as u64];
    let mut output = [0; 2];

    assert_eq!(
        jk_map_get(
            map,
            lookup.as_ptr(),
            lookup.len(),
            MAP_KEY_STRING,
            output.as_mut_ptr(),
            output.len(),
        ),
        1
    );
    assert_eq!(jk_string_len(output[0] as *mut u8), value_text.len());

    jk_drop(map);
    assert_eq!(live_object_count(), 1);
    jk_drop(output[0] as *mut u8);
    assert_eq!(live_object_count(), 0);
}

#[test]
fn string_ordering_uses_full_utf8_contents_and_preserves_operands() {
    for (left, right, expected) in [
        ("", "", 0),
        ("", "a", -1),
        ("a", "ab", -1),
        ("same", "same", 0),
        ("z", "中", -1),
        ("a\0b", "a\0c", -1),
    ] {
        let a = jk_string_from_utf8(left.as_ptr(), left.len());
        let b = jk_string_from_utf8(right.as_ptr(), right.len());
        assert_eq!(
            crate::runtime::string::jk_string_compare(a, left.len(), b, right.len()),
            expected
        );
        assert_eq!(
            crate::runtime::string::jk_string_compare(b, right.len(), a, left.len()),
            -expected
        );
        assert_eq!(jk_string_len(a), left.len());
        assert_eq!(jk_string_len(b), right.len());
        jk_drop(a);
        jk_drop(b);
    }
    assert_eq!(live_object_count(), 0);
}

#[test]
fn mut_list_set_replaces_reference_masks_with_the_value() {
    use crate::runtime::mut_list::*;
    let list = jk_mut_list_new(1, 1);
    let no_refs = [0u64];
    let refs = [1u64];
    assert_eq!(
        jk_mut_list_push(list, [42].as_ptr(), 1, no_refs.as_ptr(), 1),
        1
    );
    let text = jk_string_from_utf8(b"first".as_ptr(), 5);
    assert_eq!(
        jk_mut_list_set(list, 0, [text as u64].as_ptr(), 1, refs.as_ptr(), 1),
        1
    );
    let mut output = [0u64];
    assert_eq!(jk_mut_list_get(list, 0, output.as_mut_ptr(), 1), 1);
    assert_eq!(output[0], text as u64);
    jk_drop(output[0] as *mut u8);
    assert_eq!(jk_string_len(text), 5);
    assert_eq!(
        jk_mut_list_set(list, 0, [99].as_ptr(), 1, no_refs.as_ptr(), 1),
        1
    );
    assert_eq!(live_object_count(), 1);
    let final_text = jk_string_from_utf8(b"last".as_ptr(), 4);
    assert_eq!(
        jk_mut_list_set(list, 0, [final_text as u64].as_ptr(), 1, refs.as_ptr(), 1),
        1
    );
    jk_drop(list);
    assert_eq!(live_object_count(), 0);
}
