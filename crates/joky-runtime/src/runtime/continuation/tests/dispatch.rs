use super::super::*;
use super::support::*;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::thread;

#[test]
fn dispatch_rejects_stale_generation_without_calling_scheduler() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    DISPATCH_COUNT.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(11);
    assert!(continuation.set_resume_callback(count_dispatch as *mut c_void));
    let handle = &continuation as *const Continuation as *mut Continuation;
    assert!(!continuation.dispatch(handle, 10));
    assert_eq!(DISPATCH_COUNT.load(Ordering::SeqCst), 0);
    assert_eq!(continuation.state(), ContinuationState::Ready);
}

#[test]
fn dispatch_transitions_once_and_invokes_registered_scheduler() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    DISPATCH_COUNT.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(12);
    assert!(continuation.set_resume_callback(count_dispatch as *mut c_void));
    let handle = &continuation as *const Continuation as *mut Continuation;
    assert!(continuation.dispatch(handle, 12));
    assert_eq!(DISPATCH_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(continuation.state(), ContinuationState::Resuming);
    assert!(!continuation.dispatch(handle, 12));
    assert_eq!(DISPATCH_COUNT.load(Ordering::SeqCst), 1);
    assert!(continuation.complete());
}

#[test]
fn machine_trampoline_requires_resuming_state_and_invokes_entry_once() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    DISPATCH_COUNT.store(0, Ordering::SeqCst);
    let handle = jk_continuation_new(13);
    let continuation =
        unsafe { Continuation::retain_registered(handle).expect("registered continuation handle") };
    assert!(continuation.set_machine_entry(count_machine_entry as *mut c_void));
    assert_eq!(unsafe { jk_continuation_machine_trampoline(handle) }, 0);
    assert!(continuation.resume());
    assert_eq!(unsafe { jk_continuation_machine_trampoline(handle) }, 1);
    assert_eq!(DISPATCH_COUNT.load(Ordering::SeqCst), 1);
    assert!(continuation.complete());
    assert_eq!(unsafe { jk_continuation_machine_trampoline(handle) }, 0);
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn registered_machine_entry_is_bound_by_stable_resume_key() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    DISPATCH_COUNT.store(0, Ordering::SeqCst);
    let key = 0xC0DE_0001;
    assert_eq!(
        unsafe { jk_continuation_register_machine_entry(key, count_machine_entry as *mut c_void) },
        1
    );
    let handle = jk_continuation_new(14);
    let continuation =
        unsafe { Continuation::retain_registered(handle).expect("registered continuation handle") };
    assert!(continuation.set_resume_entry(key));
    assert!(continuation.resume());
    assert_eq!(unsafe { jk_continuation_machine_trampoline(handle) }, 1);
    assert_eq!(DISPATCH_COUNT.load(Ordering::SeqCst), 1);
    assert!(continuation.complete());
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn non_blocking_sleep_dispatches_machine_entry_after_wake() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    let _machine_guard = MACHINE_COMPLETE_TEST_LOCK
        .lock()
        .expect("machine completion test mutex");
    MACHINE_COMPLETE_COUNT.store(0, Ordering::SeqCst);
    let handle = jk_continuation_new(19);
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .set_machine_entry(complete_machine_entry as *mut c_void)
    });
    assert_eq!(unsafe { jk_continuation_begin_sleep(handle, 1) }, 1);
    for _ in 0..100 {
        if unsafe { jk_continuation_state(handle) } == ContinuationState::Completed as u8 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(MACHINE_COMPLETE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe { jk_continuation_state(handle) },
        ContinuationState::Completed as u8
    );
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn freeing_a_continuation_from_its_resume_callback_is_deferred() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    FREE_IN_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    let handle = jk_continuation_new(20);
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .set_machine_entry(complete_and_free_machine_entry as *mut c_void)
    });
    assert_eq!(unsafe { jk_continuation_begin_sleep(handle, 1) }, 1);
    for _ in 0..100 {
        if FREE_IN_CALLBACK_COUNT.load(Ordering::SeqCst) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(FREE_IN_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn machine_entry_registration_rejects_null_addresses() {
    assert_eq!(
        unsafe { jk_continuation_register_machine_entry(0xC0DE_0002, std::ptr::null_mut()) },
        0
    );
}
