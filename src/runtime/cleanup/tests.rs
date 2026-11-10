use super::*;
use std::{cell::RefCell, ffi::c_void, ptr::NonNull};

thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}
unsafe extern "C" fn release(token: *mut c_void) {
    let id = *Box::from_raw(token.cast::<i32>());
    EVENTS.with(|events| events.borrow_mut().push(id));
}
fn entry(arena: &mut Arena, id: i32) -> Entry {
    let resource = super::super::resource::OwnedResource::from_token(
        NonNull::new(Box::into_raw(Box::new(id)).cast()).unwrap(),
        release,
    )
    .unwrap();
    Entry {
        keep_alive: None,
        storage_size: 0,
        storage: None,
        owners: [Some(Buffer::external(arena, resource).unwrap().as_ptr())].into(),
    }
}

#[test]
fn cleanup_inline_entry_reuse_preserves_resource_release_order() {
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut arena = Arena::default();
    let mut cleanups = Cleanups::default();
    for index in 0..3 {
        cleanups.insert((index + 1) * 10, entry(&mut arena, index as i32 + 1));
    }
    let first = cleanups.take(10).unwrap();
    let second = cleanups.take(20).unwrap();
    cleanups.insert(20, second);
    cleanups.insert(10, first);
    unsafe { cleanups.end(30, 1, &mut arena).unwrap() };
    let first = cleanups.take(10).unwrap();
    cleanups.insert(40, first);
    unsafe { cleanups.end(20, 21, &mut arena).unwrap() };
    unsafe { cleanups.finish(&mut arena).unwrap() };
    EVENTS.with(|events| assert_eq!(*events.borrow(), [3, 1, 2]));
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn cleanup_owner_lanes_promote_and_detach_resources_with_empty_lanes() {
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut arena = Arena::default();
    let mut combined = entry(&mut arena, 1);
    combined.owners.push(None);
    combined.owners.push(entry(&mut arena, 3).owners[0]);
    let mut cleanups = Cleanups::default();
    cleanups.insert(0, combined);
    let detached = cleanups.take_fields(0, 0, 2);
    cleanups.insert(4, detached);
    unsafe { cleanups.finish(&mut arena).unwrap() };
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 3]));
    assert_eq!(arena.bytes(), 0);
}
