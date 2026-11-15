use super::*;
use crate::runtime::{
    arena::LIMIT,
    buffer::ElementCleanup,
    parallel::{Storage, TaskBudget},
};

unsafe fn text(arena: &mut Arena, length: usize) -> *mut Buffer {
    let bytes = Buffer::copy_bytes(arena, &vec![b'x'; length]).unwrap();
    wrap(arena, bytes.as_ptr())
}

unsafe fn wrap(arena: &mut Arena, child: *mut Buffer) -> *mut Buffer {
    let layout = Layout::new::<*mut Buffer>();
    let mut root = Buffer::allocate(arena, 1, layout.size(), layout.align()).unwrap();
    root.as_mut()
        .configure(ElementCleanup { owners: vec![0] })
        .unwrap();
    root.as_mut()
        .initialize_next()
        .unwrap()
        .cast::<*mut Buffer>()
        .write(child);
    root.as_mut().commit_next();
    root.as_ptr()
}

#[test]
fn shared_freezing_rejects_hidden_resources_without_consuming_them() {
    unsafe extern "C" fn release_counter(token: *mut c_void) {
        let counter = Box::from_raw(token.cast::<Rc<std::cell::Cell<usize>>>());
        counter.set(counter.get() + 1);
    }
    let released = Rc::new(std::cell::Cell::new(0));
    let mut arena = Arena::default();
    unsafe {
        let token = NonNull::from(Box::leak(Box::new(released.clone()))).cast();
        let resource = OwnedResource::from_token(token, release_counter).unwrap();
        let child = Buffer::external(&mut arena, resource).unwrap();
        let source = wrap(&mut arena, child.as_ptr());
        let baseline = arena.bytes();
        assert!(create(&mut arena, source, true).is_err());
        assert_eq!(arena.bytes(), baseline);
        assert_eq!(released.get(), 0);
        Buffer::release(&mut arena, source).unwrap();
    }
    assert_eq!(released.get(), 1);
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn shared_handles_keep_one_payload_and_refund_after_last_release() {
    let mut arena = Arena::default();
    unsafe {
        let source = text(&mut arena, 65536);
        let first = create(&mut arena, source, true).unwrap().as_ptr();
        Buffer::release(&mut arena, source).unwrap();
        let baseline = arena.bytes();
        let second = create(&mut arena, first, false).unwrap().as_ptr();
        // Generic Buffer<T> registration must not install unique-element cleanup
        // on a shared handle, even when T contains owning fields.
        (*first)
            .configure(ElementCleanup { owners: vec![0] })
            .unwrap();
        assert!((*first).cleanup.is_none());
        assert!(
            arena.bytes() - baseline < 4096,
            "a handle must not charge the payload twice"
        );
        assert_eq!((*first).data, (*second).data);
        Buffer::release(&mut arena, first).unwrap();
        let child = (*second).load(0).unwrap().cast::<*mut Buffer>().read();
        assert_eq!((*child).bytes().unwrap(), vec![b'x'; 65536]);
        assert!(arena.bytes() >= 65536);
        Buffer::release(&mut arena, second).unwrap();
    }
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn shared_partial_allocation_failures_leave_sources_and_budgets_intact() {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(self.0));
        }
    }
    let mut arena = Arena::default();
    unsafe {
        let source = text(&mut arena, 32);
        let baseline = arena.bytes();
        let mut completed = false;
        for extra in (0..8192).step_by(32) {
            let result = {
                let _restore = Restore(LIMIT.with(|n| n.replace(baseline + extra)));
                create(&mut arena, source, true)
            };
            match result {
                Ok(value) => {
                    Buffer::release(&mut arena, value.as_ptr()).unwrap();
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
            }
            assert_eq!(arena.bytes(), baseline);
            assert_eq!((*source).len(), 1);
            if completed {
                break;
            }
        }
        assert!(completed);
        let first = create(&mut arena, source, true).unwrap();
        let baseline = arena.bytes();
        {
            let _restore = Restore(LIMIT.with(|n| n.replace(baseline)));
            assert!(create(&mut arena, first.as_ptr(), false).is_err());
        }
        assert_eq!(arena.bytes(), baseline);
        Buffer::release(&mut arena, first.as_ptr()).unwrap();
        Buffer::release(&mut arena, source).unwrap();
    }
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn shared_parallel_graphs_retain_their_root_charge_and_reject_foreign_roots() {
    let budget = TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let other = Storage::new(Vec::new()).unwrap();
    let mut arena = Arena::with_storage(root.clone());
    let mut foreign = Arena::with_storage(other);
    unsafe {
        let source = text(&mut arena, 65536);
        let first = create(&mut arena, source, true).unwrap();
        Buffer::release(&mut arena, source).unwrap();
        assert!(first.as_ref().resource_ref().unwrap().parallel_safe());
        assert!(create(&mut foreign, first.as_ptr(), false).is_err());
        let baseline = budget.used();
        let second = create(&mut arena, first.as_ptr(), false).unwrap();
        assert!(budget.used() - baseline < 4096);
        Buffer::release(&mut arena, first.as_ptr()).unwrap();
        assert!(budget.used() >= 65536);
        Buffer::release(&mut arena, second.as_ptr()).unwrap();
    }
    drop((arena, root));
    assert_eq!(budget.used(), 0);
}
