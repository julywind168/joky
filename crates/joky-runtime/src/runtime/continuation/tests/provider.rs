use super::super::*;
use super::support::*;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::thread;

#[test]
fn registered_provider_completes_a_typed_payload_asynchronously() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    const OPERATION: u64 = 0xfeed_1001;
    PROVIDER_START_COUNT.store(0, Ordering::SeqCst);
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                async_provider as *mut c_void,
                std::ptr::null_mut(),
            )
        },
        1
    );
    let handle = jk_continuation_new(31);
    assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 8) }.is_null());
    let argument = 42_u64.to_ne_bytes();
    let value = unsafe { retain_handle(handle) }.expect("live provider handle");
    assert!(!value
        .allocate_suspend_arguments(argument.as_ptr(), argument.len())
        .is_null());
    assert!(value.start_suspend_provider(handle, OPERATION));
    for _ in 0..50 {
        if unsafe { jk_continuation_state(handle) } == ContinuationState::Ready as u8 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(PROVIDER_START_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(
        unsafe { jk_continuation_state(handle) },
        ContinuationState::Ready as u8
    );
    let result = unsafe { jk_continuation_suspend_result_pointer(handle) };
    assert_eq!(unsafe { result.cast::<u64>().read_unaligned() }, 99);
    assert_eq!(unsafe { jk_continuation_resume(handle) }, 1);
    assert_eq!(
        unsafe { jk_continuation_complete_suspend(handle, OPERATION) },
        1
    );
    unsafe {
        jk_continuation_unregister_suspend_provider(OPERATION);
        jk_continuation_free(handle);
    }
}

#[test]
fn cancelling_provider_request_invokes_cancel_hook_once() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    const OPERATION: u64 = 0xfeed_1002;
    PROVIDER_CANCEL_COUNT.store(0, Ordering::SeqCst);
    assert_eq!(
        unsafe {
            jk_continuation_register_suspend_provider(
                OPERATION,
                cancellable_provider as *mut c_void,
                cancel_provider as *mut c_void,
            )
        },
        1
    );
    let handle = jk_continuation_new(32);
    assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 8) }.is_null());
    let value = unsafe { retain_handle(handle) }.expect("live provider handle");
    assert!(value.start_suspend_provider(handle, OPERATION));
    assert_eq!(unsafe { jk_continuation_cancel(handle) }, 1);
    assert_eq!(PROVIDER_CANCEL_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(unsafe { jk_continuation_cancel(handle) }, 0);
    unsafe {
        jk_continuation_unregister_suspend_provider(OPERATION);
        jk_continuation_free(handle);
    }
}

#[test]
fn unregistered_provider_request_is_rejected_without_timer() {
    const OPERATION: u64 = 0xfeed_1003;
    let handle = jk_continuation_new(33);
    assert!(!unsafe { jk_continuation_alloc_suspend_result(handle, 8) }.is_null());
    let value = unsafe { retain_handle(handle) }.expect("live provider handle");
    assert!(!value.start_suspend_provider(handle, OPERATION));
    assert_eq!(
        unsafe { jk_continuation_state(handle) },
        ContinuationState::Ready as u8
    );
    unsafe { jk_continuation_free(handle) };
}
