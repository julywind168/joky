use super::super::*;
use super::support::*;
use std::ffi::c_void;
use std::sync::atomic::Ordering;

#[test]
fn cancellation_runs_registered_owned_cleanup_once() {
    let _guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    CLEANUP_COUNT.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(20);
    let frame = continuation.allocate_frame(8);
    assert!(!frame.is_null());
    unsafe { frame.cast::<*mut c_void>().write(std::ptr::dangling_mut()) };
    assert!(continuation.register_cleanup(
        CONTINUATION_FRAME_STORAGE,
        0,
        count_cleanup as *mut c_void,
        false
    ));
    assert!(continuation.cancel());
    assert_eq!(CLEANUP_COUNT.load(Ordering::SeqCst), 1);
    assert!(!continuation.cancel());
}

#[test]
fn cancellation_runs_region_cleanup_at_the_registered_storage_offset() {
    let _guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    CLEANUP_COUNT.store(0, Ordering::SeqCst);
    REGION_CLEANUP_POINTER.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(20);
    let frame = continuation.allocate_frame(16);
    assert!(!frame.is_null());
    let expected = unsafe { frame.add(8) as usize };
    assert!(continuation.register_cleanup(
        CONTINUATION_FRAME_STORAGE,
        8,
        record_region_cleanup as *mut c_void,
        true
    ));
    assert!(continuation.cancel());
    assert_eq!(CLEANUP_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(REGION_CLEANUP_POINTER.load(Ordering::SeqCst), expected);
}

#[test]
fn cancellation_runs_registered_result_cleanup() {
    let _guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    CLEANUP_COUNT.store(0, Ordering::SeqCst);
    let continuation = Continuation::new(20);
    let result = continuation.allocate_result(8);
    assert!(!result.is_null());
    unsafe { result.cast::<*mut c_void>().write(std::ptr::dangling_mut()) };
    assert!(continuation.register_cleanup(
        CONTINUATION_RESULT_STORAGE,
        0,
        count_cleanup as *mut c_void,
        false
    ));
    assert!(continuation.cancel());
    assert_eq!(CLEANUP_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn completed_machine_entry_runs_registered_cleanup_once() {
    let _guard = SHARED_STATE_TEST_LOCK
        .lock()
        .expect("continuation shared-state test mutex");
    let _machine_guard = MACHINE_COMPLETE_TEST_LOCK
        .lock()
        .expect("machine completion test mutex");
    let _cleanup_guard = CLEANUP_TEST_LOCK.lock().expect("cleanup test mutex");
    CLEANUP_COUNT.store(0, Ordering::SeqCst);
    let handle = jk_continuation_new(21);
    let continuation =
        unsafe { Continuation::retain_registered(handle).expect("registered continuation handle") };
    let frame = continuation.allocate_frame(8);
    assert!(!frame.is_null());
    unsafe { frame.cast::<*mut c_void>().write(std::ptr::dangling_mut()) };
    assert!(continuation.register_cleanup(
        CONTINUATION_FRAME_STORAGE,
        0,
        count_cleanup as *mut c_void,
        false
    ));
    assert!(continuation.set_machine_entry(complete_machine_entry as *mut c_void));
    assert!(continuation.resume());
    assert_eq!(unsafe { jk_continuation_machine_trampoline(handle) }, 1);
    assert_eq!(CLEANUP_COUNT.load(Ordering::SeqCst), 1);
    unsafe { jk_continuation_free(handle) };
}

#[test]
fn frame_storage_tracks_program_counter_and_releases_once() {
    let continuation = Continuation::new(15);
    let pointer = continuation.allocate_frame(24);
    assert!(!pointer.is_null());
    assert_eq!(continuation.frame_pointer(), pointer);
    assert_eq!(continuation.frame_size(), 24);
    assert!(continuation.set_program_counter(0x1234));
    assert_eq!(continuation.program_counter(), 0x1234);
    assert!(continuation.release_frame());
    assert!(continuation.frame_pointer().is_null());
    assert_eq!(continuation.frame_size(), 0);
    assert_eq!(continuation.program_counter(), 0);
    assert!(!continuation.release_frame());
    assert!(!continuation.set_program_counter(0x5678));
}

#[test]
fn cancelling_a_continuation_releases_frame_storage() {
    let continuation = Continuation::new(16);
    assert!(!continuation.allocate_frame(8).is_null());
    assert!(continuation.set_program_counter(3));
    assert!(continuation.cancel());
    assert!(continuation.frame_pointer().is_null());
    assert_eq!(continuation.frame_size(), 0);
    assert_eq!(continuation.program_counter(), 0);
}

#[test]
fn result_storage_tracks_pointer_and_releases_once() {
    let continuation = Continuation::new(17);
    let pointer = continuation.allocate_result(16);
    assert!(!pointer.is_null());
    assert_eq!(continuation.result_pointer(), pointer);
    assert_eq!(continuation.result_size(), 16);
    unsafe { *pointer = 0x2a };
    assert_eq!(unsafe { *continuation.result_pointer() }, 0x2a);
    assert!(continuation.release_result());
    assert!(continuation.result_pointer().is_null());
    assert_eq!(continuation.result_size(), 0);
    assert!(!continuation.release_result());
}

#[test]
fn completing_a_continuation_releases_result_storage() {
    let continuation = Continuation::new(18);
    assert!(!continuation.allocate_result(8).is_null());
    assert!(continuation.resume());
    assert!(continuation.complete());
    assert!(!continuation.result_pointer().is_null());
    assert_eq!(continuation.result_size(), 8);
    assert!(continuation.release_result());
    assert!(continuation.result_pointer().is_null());
}

#[test]
fn suspend_result_storage_accepts_a_provider_payload() {
    let continuation = Continuation::new(22);
    let result = continuation.allocate_suspend_result(8);
    assert!(!result.is_null());
    assert_eq!(continuation.suspend_result_size(), 8);
    assert!(continuation.start_suspend(std::ptr::null_mut(), 91, 60_000, false));
    let payload = 42_u64.to_ne_bytes();
    assert!(continuation.complete_suspend_with_payload(
        std::ptr::null_mut(),
        91,
        payload.as_ptr(),
        payload.len(),
    ));
    assert_eq!(continuation.state(), ContinuationState::Ready);
    assert_eq!(unsafe { result.cast::<u64>().read() }, 42);
    assert!(continuation.resume());
    assert!(continuation.complete_operation(91));
    continuation.wait_for_timers();
    assert!(continuation.suspend_result_pointer().is_null());
}
