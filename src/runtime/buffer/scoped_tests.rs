//! Storage probes only: Rust scoped threads supply the lifetime fence that the
//! language task transport must implement before admitting borrowed inputs.
use super::*;
use std::sync::Barrier;

#[test]
fn scoped_borrow_storage_root_does_not_replace_an_owner_lease() {
    use crate::runtime::parallel::{Storage, TaskBudget};
    let budget = TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let mut arena = Arena::with_storage(root.clone());
    let owner = Buffer::copy_bytes(&mut arena, b"source").unwrap();
    let payload = unsafe { owner.as_ref().data };
    assert!(root.contains(owner.cast()));
    assert!(root.contains(payload));
    unsafe {
        Buffer::release(&mut arena, owner.as_ptr()).unwrap();
    }
    // The root and its code/budget lifetime survive, but both allocations are
    // already gone. A scoped transport must prevent this explicit cleanup.
    assert!(!root.contains(owner.cast()));
    assert!(!root.contains(payload));
    drop((arena, root));
    assert_eq!(budget.used(), 0);
}

#[test]
fn scoped_borrow_views_share_payload_but_keep_headers_and_cursors_local() {
    let mut arena = Arena::default();
    let mut owner = Buffer::allocate(&mut arena, 4, 4, 4).unwrap();
    unsafe {
        for value in [3i32, 5, 7, 11] {
            owner
                .as_ref()
                .initialize_next()
                .unwrap()
                .cast::<i32>()
                .write(value);
            owner.as_mut().commit_next();
        }
    }
    // Only initialized plain scalar storage is shared. The Rust scope joins
    // both readers before any mutation or explicit owner release below.
    let source =
        unsafe { std::slice::from_raw_parts(owner.as_ref().data.as_ptr().cast::<i32>(), 4) };
    let barrier = Barrier::new(2);
    let observed = std::thread::scope(|scope| {
        let readers: Vec<_> = [0..3, 1..4]
            .into_iter()
            .map(|range| {
                let values = &source[range];
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut arena = Arena::default();
                    // SAFETY: values is an aligned, initialized, immutable scoped
                    // loan. This nonowning header and all cursors stay on this worker.
                    let view = unsafe {
                        Buffer::view(
                            &mut arena,
                            values.as_ptr().cast_mut().cast(),
                            values.len(),
                            Layout::new::<i32>(),
                        )
                        .unwrap()
                    };
                    let view = unsafe { view.as_ref() };
                    assert!(view.cleanup.is_none());
                    assert!(view.metadata_parallel.is_none());
                    assert!(view.resource.is_none());
                    assert!(view.dynamic.is_none());
                    assert_eq!(view.data.as_ptr().cast::<i32>(), values.as_ptr().cast_mut());
                    let cursor = view.iterator(&mut arena).unwrap();
                    let other = view.iterator(&mut arena).unwrap();
                    let cursor = unsafe { cursor.as_ref() };
                    let other = unsafe { other.as_ref() };
                    assert!(cursor.advance());
                    assert!(
                        other.current().is_err(),
                        "cursor positions must be independent"
                    );
                    barrier.wait();
                    let mut sum = unsafe { cursor.current().unwrap().cast::<i32>().read() };
                    while cursor.advance() {
                        sum += unsafe { cursor.current().unwrap().cast::<i32>().read() };
                    }
                    let nested = view.slice(&mut arena, 1, 3).unwrap();
                    let nested = unsafe { nested.as_ref() };
                    assert_eq!(nested.load(0).unwrap(), unsafe {
                        values.as_ptr().add(1).cast_mut().cast()
                    });
                    assert!(other.advance());
                    assert_eq!(
                        unsafe { other.current().unwrap().cast::<i32>().read() },
                        values[0]
                    );
                    barrier.wait();
                    (
                        sum,
                        view as *const Buffer as usize,
                        std::thread::current().id(),
                    )
                })
            })
            .collect();
        readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!((observed[0].0, observed[1].0), (15, 23));
    assert_ne!(observed[0].1, observed[1].1);
    assert_ne!(observed[0].2, observed[1].2);
    unsafe {
        owner.as_ref().load(0).unwrap().cast::<i32>().write(42);
        assert_eq!(owner.as_ref().load(0).unwrap().cast::<i32>().read(), 42);
        Buffer::release(&mut arena, owner.as_ptr()).unwrap();
    }
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn scoped_borrow_text_views_preserve_utf8_and_empty_ranges_without_copying() {
    for text in ["", "héllo 世界"] {
        let mut arena = Arena::default();
        let owner = Buffer::copy_bytes(&mut arena, text.as_bytes()).unwrap();
        let bytes = unsafe { owner.as_ref().bytes().unwrap() };
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let mut local = Arena::default();
                    // SAFETY: the Rust scope retains the immutable source; only a
                    // private header is installed and no owning cleanup is registered.
                    let view = unsafe {
                        Buffer::view(
                            &mut local,
                            bytes.as_ptr().cast_mut(),
                            bytes.len(),
                            Layout::new::<u8>(),
                        )
                        .unwrap()
                    };
                    let view = unsafe { view.as_ref() };
                    assert_eq!(view.bytes().unwrap().as_ptr(), bytes.as_ptr());
                    assert_eq!(std::str::from_utf8(view.bytes().unwrap()).unwrap(), text);
                    let empty = view.slice(&mut local, view.len(), view.len()).unwrap();
                    assert!(unsafe { empty.as_ref().bytes().unwrap() }.is_empty());
                })
                .join()
                .unwrap();
        });
        unsafe {
            Buffer::release(&mut arena, owner.as_ptr()).unwrap();
        }
        assert_eq!(arena.bytes(), 0);
    }
}
