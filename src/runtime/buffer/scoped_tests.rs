//! Storage probes only: Rust scoped threads supply the lifetime fence that the
//! language task transport must implement before admitting borrowed inputs.
use super::*;
use std::sync::Barrier;

#[test]
fn scoped_borrow_received_ranges_reject_widening_and_unproven_views() {
    use crate::runtime::{cps::CpsFrame, parallel};
    use crate::vm::{InstanceValue, TaskValue, VmEngine, VmMode};
    use crate::{FunctionSignature, Type};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    unsafe extern "C" fn inspect(frame: *mut CpsFrame) -> i32 {
        let task = parallel::pool::identity().unwrap();
        let input = parallel::scoped::observations(parallel::pool::parent_identity().unwrap())
            .into_iter()
            .find(|o| !o.source && o.task == task)
            .unwrap();
        let view = &mut *(input.header as *mut Buffer);
        let data = view.data;
        let len = view.len;
        let element = view.element;
        let mut checked = u64::from(view.scoped_range(element).is_some());
        view.len = len + 1;
        checked |= u64::from(view.scoped_range(element).is_none()) << 1;
        view.len = len;
        // The owner still has this prefix, but the received loan excludes it.
        view.data = NonNull::new_unchecked(data.as_ptr().sub(element.size()));
        checked |= u64::from(view.scoped_range(element).is_none()) << 2;
        view.data = NonNull::new_unchecked(data.as_ptr().add(1));
        checked |= u64::from(view.scoped_range(element).is_none()) << 3;
        view.data = data;
        let wrong_layout = if element.size() == 8 {
            Layout::new::<i32>()
        } else {
            Layout::new::<i64>()
        };
        checked |= u64::from(view.scoped_range(wrong_layout).is_none()) << 4;
        view.data = NonNull::new_unchecked(data.as_ptr().add(len * element.size()));
        view.len = 0;
        checked |= u64::from(view.scoped_range(element).is_some()) << 5;
        view.data = data;
        view.len = len;
        if element == Layout::new::<*mut Buffer>() {
            let shape = parallel::scoped::Shape::of(&Type::reference(
                Type::BufferSlice(Arc::new(Type::String)),
                false,
            ))
            .unwrap();
            checked |= u64::from(shape.eligible(input.header as u64)) << 7;
            let child = &mut *view.data.as_ptr().cast::<*mut Buffer>().read();
            let child_len = child.len;
            child.len += 1;
            checked |= u64::from(!shape.eligible(input.header as u64)) << 8;
            child.len = child_len;
            let loan = child.loan;
            child.loan = std::ptr::null();
            checked |= u64::from(!shape.eligible(input.header as u64)) << 9;
            child.loan = loan;
            let arena = &mut *crate::runtime::cps::arena(frame);
            let mut owner = Buffer::allocate(arena, 0, 8, 8).unwrap();
            owner
                .as_mut()
                .configure(ElementCleanup {
                    owners: vec![0],
                    hooks: vec![],
                })
                .unwrap();
            checked |= u64::from(
                owner
                    .as_ref()
                    .scoped_elements(element, [0].into_iter())
                    .is_some(),
            ) << 10;
            checked |= u64::from(
                owner
                    .as_ref()
                    .scoped_elements(element, [8].into_iter())
                    .is_none(),
            ) << 11;
            owner.as_mut().failed_descriptor = true;
            checked |= u64::from(
                owner
                    .as_ref()
                    .scoped_elements(element, [0].into_iter())
                    .is_none(),
            ) << 12;
            owner.as_mut().failed_descriptor = false;
            Buffer::release(arena, owner.as_ptr()).unwrap();
        }
        let unproven = Buffer::view(
            &mut *crate::runtime::cps::arena(frame),
            data.as_ptr(),
            len,
            element,
        );
        match unproven {
            Ok(unproven) => {
                checked |= u64::from(unproven.as_ref().scoped_range(element).is_none()) << 6;
                (*frame).result = checked;
                0
            }
            Err(error) => {
                crate::runtime::error::record_error(error);
                3
            }
        }
    }
    for (mode, owning) in [VmMode::Jit, VmMode::Interpreter]
        .into_iter()
        .flat_map(|mode| [false, true].map(move |owning| (mode, owning)))
    {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "inspect",
                FunctionSignature::new(vec![], Type::Int32),
                inspect,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        let source = r#"
            import joky/string
            import joky/slice.{type Slice}
            import joky/task
            extern fn inspect() @suspends -> Int32
            fn sum(values: &Slice<(Int32, Float64)>) @suspends -> Int32 { task.yield_now(); values[0].0 + values[1].0 }
            fn reader(values: &Slice<(Int32, Float64)>) @suspends -> Int32 {
                let checked = inspect()
                let (a, b) = join { | sum(values) | sum(values) }
                checked + a + b
            }
            fn root() @suspends -> Int32 {
                let values = [(2, 1.5), (3, 2.5), (5, 3.5), (7, 4.5)]
                let (a, b) = join { | reader(values.slice(1, 3)) | task.yield_now() }
                a
            }
        "#;
        let source = if owning {
            source.replace("&Slice<(Int32, Float64)>", "&Slice<String>")
                .replace("values[0].0 + values[1].0", "int32(slice.borrow_ref(values, 0).byte_length()) + int32(slice.borrow_ref(values, 1).byte_length())")
                .replace("[(2, 1.5), (3, 2.5), (5, 3.5), (7, 4.5)]", "[\"xx\", \"abc\", \"hello\", \"last\"]")
        } else {
            source.to_owned()
        };
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let budget = parallel::TaskBudget::new(1 << 20);
        let mut pool = parallel::ParallelExecutor::new(3, 16).unwrap();
        let task = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !task.is_finished() {
            assert!(Instant::now() < deadline, "{mode:?}: view probe stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(if owning { 8207 } else { 143 })),
            "{mode:?}"
        );
        let tree = parallel::scoped::tree_observations(task.identity());
        assert_eq!(tree.iter().filter(|o| o.source).count(), 3);
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

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
