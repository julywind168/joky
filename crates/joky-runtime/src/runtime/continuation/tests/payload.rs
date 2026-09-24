use super::super::*;
use super::support::*;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn suspend_completion_requires_the_active_operation_token() {
    let continuation = Continuation::new(0);
    assert!(continuation.start_suspend(std::ptr::null_mut(), 77, 1, false));
    thread::sleep(Duration::from_millis(5));
    assert_eq!(continuation.state(), ContinuationState::Ready);
    assert!(!continuation.complete_operation(78));
    assert!(continuation.resume());
    assert!(continuation.complete_operation(77));
    assert_eq!(continuation.state(), ContinuationState::Completed);
}

#[test]
fn provider_payload_completion_drives_machine_entry_for_scalar() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    PAYLOAD_MACHINE_COUNT.store(0, Ordering::SeqCst);
    PAYLOAD_MACHINE_VALUE.store(0, Ordering::SeqCst);
    let handle = jk_continuation_new(25);
    assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 8) }.is_null());
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .set_machine_entry(inspect_scalar_payload_machine_entry as *mut c_void)
    });
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .start_suspend(handle, 0x25, 60_000, true)
    });
    let payload = 42_u64.to_ne_bytes();
    assert_eq!(
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                handle,
                0x25,
                payload.as_ptr(),
                payload.len(),
            )
        },
        1
    );
    for _ in 0..100 {
        if PAYLOAD_MACHINE_COUNT.load(Ordering::SeqCst) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(PAYLOAD_MACHINE_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(PAYLOAD_MACHINE_VALUE.load(Ordering::SeqCst), 42);
    assert_eq!(
        unsafe { jk_continuation_state(handle) },
        ContinuationState::Completed as u8
    );
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn provider_payload_transfers_and_drops_a_managed_string() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    let _guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    STRING_PAYLOAD_SEEN.store(0, Ordering::SeqCst);
    STRING_PAYLOAD_DROPPED.store(0, Ordering::SeqCst);
    assert_eq!(crate::runtime::managed::live_object_count(), 0);
    let object = crate::runtime::managed::jk_string_from_utf8(b"hello".as_ptr(), 5);
    assert!(!object.is_null());
    let handle = jk_continuation_new(26);
    assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 16) }.is_null());
    assert!(
        unsafe {
            jk_continuation_register_cleanup(
                handle,
                CONTINUATION_SUSPEND_RESULT_STORAGE,
                0,
                drop_string_payload as *mut c_void,
            )
        } == 1
    );
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .set_machine_entry(inspect_string_payload_machine_entry as *mut c_void)
    });
    assert!(unsafe {
        Continuation::retain_registered(handle)
            .unwrap()
            .start_suspend(handle, 0x26, 60_000, true)
    });
    let mut payload = [0_u8; 16];
    payload[..8].copy_from_slice(&(object as usize).to_ne_bytes());
    payload[8..].copy_from_slice(&5_usize.to_ne_bytes());
    assert_eq!(
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                handle,
                0x26,
                payload.as_ptr(),
                payload.len(),
            )
        },
        1
    );
    for _ in 0..100 {
        if STRING_PAYLOAD_SEEN.load(Ordering::SeqCst) == 1
            && STRING_PAYLOAD_DROPPED.load(Ordering::SeqCst) == 1
        {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(STRING_PAYLOAD_SEEN.load(Ordering::SeqCst), 1);
    assert_eq!(STRING_PAYLOAD_DROPPED.load(Ordering::SeqCst), 1);
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn provider_completion_and_cancellation_race_is_one_shot() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    let _machine_guard = MACHINE_COMPLETE_TEST_LOCK
        .lock()
        .expect("machine completion test mutex");
    for generation in 30..46 {
        let handle = jk_continuation_new(generation);
        assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 8) }.is_null());
        assert!(unsafe {
            Continuation::retain_registered(handle)
                .unwrap()
                .set_machine_entry(complete_machine_entry as *mut c_void)
        });
        assert!(unsafe {
            Continuation::retain_registered(handle)
                .unwrap()
                .start_suspend(handle, generation, 60_000, true)
        });
        let barrier = Arc::new(Barrier::new(3));
        let provider_barrier = Arc::clone(&barrier);
        let provider_address = handle as usize;
        let provider = thread::spawn(move || {
            let payload = 7_u64.to_ne_bytes();
            provider_barrier.wait();
            unsafe {
                jk_continuation_complete_suspend_with_payload(
                    provider_address as *mut Continuation,
                    generation,
                    payload.as_ptr(),
                    payload.len(),
                )
            }
        });
        let cancel_barrier = Arc::clone(&barrier);
        let cancel_address = handle as usize;
        let canceller = thread::spawn(move || {
            cancel_barrier.wait();
            unsafe { jk_continuation_cancel(cancel_address as *mut Continuation) }
        });
        barrier.wait();
        let _ = provider.join().expect("provider thread should finish");
        let _ = canceller.join().expect("canceller thread should finish");
        unsafe {
            Continuation::retain_registered(handle)
                .unwrap()
                .wait_for_idle()
        };
        let state = unsafe { jk_continuation_state(handle) };
        assert!(matches!(
            state,
            value if value == ContinuationState::Completed as u8
                || value == ContinuationState::Cancelled as u8
        ));
        assert!(unsafe {
            Continuation::retain_registered(handle)
                .unwrap()
                .suspend_result_pointer()
        }
        .is_null());
        unsafe { jk_continuation_free(handle) };
    }
}

#[test]
fn suspend_result_payload_rejects_stale_or_malformed_completion() {
    let continuation = Continuation::new(23);
    assert!(!continuation.allocate_suspend_result(8).is_null());
    assert!(continuation.start_suspend(std::ptr::null_mut(), 92, 60_000, false));
    let payload = [0_u8; 8];
    assert!(!continuation.complete_suspend_with_payload(
        std::ptr::null_mut(),
        93,
        payload.as_ptr(),
        payload.len(),
    ));
    assert!(!continuation.complete_suspend_with_payload(
        std::ptr::null_mut(),
        92,
        payload.as_ptr(),
        payload.len() - 1,
    ));
    assert_eq!(continuation.state(), ContinuationState::Sleeping);
    assert!(continuation.cancel());
    continuation.wait_for_timers();
}

#[test]
fn cancelling_a_suspend_result_runs_owned_cleanup_once() {
    let _guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    CLEANUP_COUNT.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(24);
    let result = continuation.allocate_suspend_result(8);
    assert!(!result.is_null());
    unsafe { result.cast::<*mut c_void>().write(std::ptr::dangling_mut()) };
    assert!(continuation.register_cleanup(
        CONTINUATION_SUSPEND_RESULT_STORAGE,
        0,
        count_cleanup as *mut c_void,
        false,
    ));
    assert!(continuation.cancel());
    assert_eq!(CLEANUP_COUNT.load(Ordering::SeqCst), 1);
    assert!(continuation.suspend_result_pointer().is_null());
}
