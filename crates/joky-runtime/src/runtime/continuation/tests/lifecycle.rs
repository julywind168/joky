use super::super::*;

#[test]
fn ready_before_next_callback_reservation_keeps_scope_work() {
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _guard = scope.enter();
    let continuation = Continuation::new(1);
    assert!(continuation.begin_scope_work());
    let callback = continuation.reserve_callback(std::ptr::null_mut()).unwrap();
    // The previous callback is exiting while the next completion has made
    // the handle Ready but has not reserved/enqueued its callback yet.
    assert_eq!(continuation.state(), ContinuationState::Ready);
    continuation.release_scope_work_if_idle();
    assert!(continuation.inner.scope_work.lock().unwrap().is_some());
    drop(callback);
    assert!(continuation.cancel());
    assert!(continuation.inner.scope_work.lock().unwrap().is_none());
}

#[test]
fn timer_moves_a_continuation_back_to_ready() {
    let continuation = Continuation::new(7);
    assert_eq!(continuation.generation(), 7);
    assert!(continuation.sleep(1));
    assert_eq!(continuation.state(), ContinuationState::Sleeping);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(continuation.state(), ContinuationState::Ready);
    assert!(continuation.resume());
    assert!(continuation.complete());
    assert_eq!(continuation.state(), ContinuationState::Completed);
}

#[test]
fn late_abi_callbacks_after_free_are_rejected() {
    let handle = jk_continuation_new(34);
    unsafe { jk_continuation_free(handle) };
    let payload = [0_u8; 8];
    assert_eq!(unsafe { jk_continuation_cancel(handle) }, 0);
    assert_eq!(
        unsafe {
            jk_continuation_complete_suspend_with_payload(handle, 0xfeed, payload.as_ptr(), 8)
        },
        0
    );
    assert_eq!(
        unsafe { jk_continuation_state(handle) },
        ContinuationState::Cancelled as u8
    );
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn retired_handle_token_is_not_reused_for_a_new_continuation() {
    let first = jk_continuation_new(35);
    let first_token = first as usize;
    unsafe { jk_continuation_free(first) };
    let replacement = jk_continuation_new(36);
    assert_ne!(replacement as usize, first_token);
    unsafe { jk_continuation_free(replacement) };
}

#[test]
fn cancelling_a_timer_wakes_it_without_waiting_for_deadline() {
    let continuation = Continuation::new(0);
    assert!(continuation.sleep(60_000));
    assert!(continuation.cancel());
    assert_eq!(continuation.state(), ContinuationState::Cancelled);
}

#[test]
fn generation_guards_resume_and_spill_metadata_is_stable() {
    let continuation = Continuation::new(9);
    assert!(continuation.set_resume_entry(17));
    assert_eq!(continuation.resume_entry(), 17);
    let mut spill = [0_u8; 8];
    assert!(continuation.set_spill(spill.as_mut_ptr(), spill.len()));
    assert_eq!(continuation.spill_pointer(), spill.as_mut_ptr());
    assert_eq!(continuation.spill_size(), 8);
    assert!(!continuation.resume_at(8));
    assert!(continuation.resume_at(9));
    assert!(continuation.complete());
    assert!(!continuation.set_spill(spill.as_mut_ptr(), spill.len()));
}

#[test]
fn heap_spill_storage_is_owned_and_released() {
    let continuation = Continuation::new(1);
    let pointer = continuation.allocate_spill(16);
    assert!(!pointer.is_null());
    assert_eq!(continuation.spill_pointer(), pointer);
    assert_eq!(continuation.spill_size(), 16);
    assert!(continuation.allocate_spill(8).is_null());
    assert!(continuation.release_spill());
    assert!(continuation.spill_pointer().is_null());
    assert_eq!(continuation.spill_size(), 0);
}

#[test]
fn cancellation_releases_heap_spill_immediately() {
    let continuation = Continuation::new(2);
    assert!(!continuation.allocate_spill(32).is_null());
    assert!(continuation.cancel());
    assert!(continuation.spill_pointer().is_null());
    assert_eq!(continuation.spill_size(), 0);
    assert!(continuation.allocate_spill(8).is_null());
}

#[test]
fn stale_callback_cannot_reach_a_new_scope_with_the_same_generation() {
    let old_scope = crate::runtime::scope::RuntimeScope::new();
    let old = {
        let _guard = old_scope.enter();
        jk_continuation_new(1)
    };
    let token = old as usize;
    let (release, receive) = std::sync::mpsc::channel();
    let callback = std::thread::spawn(move || {
        receive.recv().unwrap();
        assert_eq!(
            unsafe { jk_continuation_cancel(token as *mut Continuation) },
            0
        );
        assert_eq!(
            unsafe {
                jk_continuation_complete_suspend_with_payload(
                    token as *mut Continuation,
                    7,
                    std::ptr::null(),
                    0,
                )
            },
            0
        );
    });
    unsafe { jk_continuation_free(old) };
    old_scope.close_and_wait();
    let scope = crate::runtime::scope::RuntimeScope::new();
    let _guard = scope.enter();
    let current = jk_continuation_new(1);
    assert_ne!(current, old);
    release.send(()).unwrap();
    callback.join().unwrap();
    assert_eq!(
        unsafe { jk_continuation_state(current) },
        ContinuationState::Ready as u8
    );
    assert_eq!(scope.resources.snapshot()[0], 1);
    assert_eq!(old_scope.resources.snapshot()[0], 0);
    unsafe { jk_continuation_free(current) };
}
