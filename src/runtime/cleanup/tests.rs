use super::*;
use std::cell::RefCell;

thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}
unsafe extern "C" fn record(data: *mut u8, _: *mut Arena) {
    let id = data.cast::<i32>().read();
    EVENTS.with(|events| events.borrow_mut().push(id));
    if id < 0 {
        super::super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::RuntimeError,
            "cleanup sentinel",
        ));
    }
}
fn entry(arena: &mut Arena, id: &mut i32) -> Entry {
    Entry {
        keep_alive: None,
        storage_size: 0,
        storage: None,
        owners: [Some(Buffer::copy_bytes(arena, &[42]).unwrap().as_ptr())].into(),
        hooks: vec![(
            0,
            Hook::Native {
                data: (id as *mut i32).cast(),
                callback: record,
            },
        )],
    }
}

#[test]
fn cleanup_inline_entry_reuse_preserves_order_and_drains_after_failure() {
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut arena = Arena::default();
    let mut ids = [-1, 2, 3];
    let mut cleanups = Cleanups::default();
    for (index, id) in ids.iter_mut().enumerate() {
        cleanups.insert((index + 1) * 10, entry(&mut arena, id));
    }
    let first = cleanups.take(10).unwrap();
    let second = cleanups.take(20).unwrap();
    cleanups.insert(20, second); // Reuse the inline position for a later binding.
    cleanups.insert(10, first); // Its original declaration order must survive.
    unsafe { cleanups.end(30, 1, &mut arena).unwrap() };
    let first = cleanups.take(10).unwrap();
    cleanups.insert(40, first);
    let error = unsafe { cleanups.end(20, 21, &mut arena) }.unwrap_err();
    assert_eq!(error.message(), "cleanup sentinel");
    unsafe { cleanups.finish(&mut arena).unwrap() };
    EVENTS.with(|events| assert_eq!(*events.borrow(), [3, -1, 2]));
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn cleanup_owner_lanes_promote_and_detach_without_losing_empty_hooks() {
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut arena = Arena::default();
    let mut ids = [1, 2, 3];
    let mut combined = entry(&mut arena, &mut ids[0]);
    combined.owners.push(None);
    combined
        .owners
        .push(Some(Buffer::copy_bytes(&mut arena, &[9]).unwrap().as_ptr()));
    for (index, id) in ids.iter_mut().enumerate().skip(1) {
        combined.hooks.push((
            index,
            Hook::Native {
                data: (id as *mut i32).cast(),
                callback: record,
            },
        ));
    }
    let mut cleanups = Cleanups::default();
    cleanups.insert(0, combined);
    let detached = cleanups.take_fields(0, 0, 2);
    cleanups.insert(4, detached);
    unsafe { cleanups.finish(&mut arena).unwrap() };
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3]));
    assert_eq!(arena.bytes(), 0);
}
