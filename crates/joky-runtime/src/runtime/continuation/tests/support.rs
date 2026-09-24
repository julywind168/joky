use super::super::*;
use std::thread;

pub(super) static DISPATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static SHARED_STATE_TEST_LOCK: Mutex<()> = Mutex::new(());
pub(super) static MACHINE_COMPLETE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static MACHINE_COMPLETE_TEST_LOCK: Mutex<()> = Mutex::new(());
pub(super) static PAYLOAD_MACHINE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static PAYLOAD_MACHINE_VALUE: AtomicUsize = AtomicUsize::new(0);
pub(super) static FREE_IN_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static STRING_PAYLOAD_SEEN: AtomicUsize = AtomicUsize::new(0);
pub(super) static STRING_PAYLOAD_DROPPED: AtomicUsize = AtomicUsize::new(0);
pub(super) static PROVIDER_START_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static PROVIDER_CANCEL_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static CLEANUP_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(super) static REGION_CLEANUP_POINTER: AtomicUsize = AtomicUsize::new(0);
pub(super) static CLEANUP_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(super) unsafe extern "C" fn count_dispatch(_continuation: *mut Continuation) {
    DISPATCH_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub(super) unsafe extern "C" fn count_machine_entry(_continuation: *mut Continuation) {
    DISPATCH_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub(super) unsafe extern "C" fn complete_machine_entry(continuation: *mut Continuation) {
    MACHINE_COMPLETE_COUNT.fetch_add(1, Ordering::SeqCst);
    let _ = unsafe { jk_continuation_complete(continuation) };
}

pub(super) unsafe extern "C" fn complete_and_free_machine_entry(continuation: *mut Continuation) {
    FREE_IN_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    let _ = unsafe { jk_continuation_complete(continuation) };
    unsafe { jk_continuation_free(continuation) };
}

pub(super) unsafe extern "C" fn inspect_scalar_payload_machine_entry(
    continuation: *mut Continuation,
) {
    let pointer = unsafe { jk_continuation_suspend_result_pointer(continuation) };
    let value = if pointer.is_null() {
        0
    } else {
        unsafe { std::ptr::read_unaligned(pointer.cast::<u64>()) }
    };
    PAYLOAD_MACHINE_VALUE.store(value as usize, Ordering::SeqCst);
    PAYLOAD_MACHINE_COUNT.fetch_add(1, Ordering::SeqCst);
    let _ = unsafe { jk_continuation_complete(continuation) };
}

pub(super) unsafe extern "C" fn inspect_string_payload_machine_entry(
    continuation: *mut Continuation,
) {
    let pointer = unsafe { jk_continuation_suspend_result_pointer(continuation) };
    if !pointer.is_null() {
        let object = unsafe { std::ptr::read_unaligned(pointer.cast::<*mut u8>()) };
        let length = unsafe { std::ptr::read_unaligned(pointer.add(8).cast::<usize>()) };
        if !object.is_null() && length == 5 {
            let bytes = unsafe { std::slice::from_raw_parts(object, length) };
            if bytes == b"hello" {
                STRING_PAYLOAD_SEEN.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let _ = unsafe { jk_continuation_complete(continuation) };
}

pub(super) unsafe extern "C" fn drop_string_payload(pointer: *mut c_void) {
    STRING_PAYLOAD_DROPPED.fetch_add(1, Ordering::SeqCst);
    if !pointer.is_null() {
        crate::runtime::managed::jk_drop(pointer.cast());
    }
}

pub(super) unsafe extern "C" fn count_cleanup(_: *mut c_void) {
    CLEANUP_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub(super) unsafe extern "C" fn record_region_cleanup(pointer: *mut c_void) {
    REGION_CLEANUP_POINTER.store(pointer as usize, Ordering::SeqCst);
    CLEANUP_COUNT.fetch_add(1, Ordering::SeqCst);
}

pub(super) unsafe extern "C" fn async_provider(
    continuation: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 8 || result_size != 8 {
        return 0;
    }
    let argument = unsafe { std::ptr::read_unaligned(arguments.cast::<u64>()) };
    if argument != 42 {
        return 0;
    }
    PROVIDER_START_COUNT.fetch_add(1, Ordering::SeqCst);
    let address = continuation as usize;
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(2));
        let payload = 99_u64.to_ne_bytes();
        unsafe {
            jk_continuation_complete_suspend_with_payload(
                address as *mut Continuation,
                operation,
                payload.as_ptr(),
                payload.len(),
            );
        }
    });
    1
}

pub(super) unsafe extern "C" fn cancellable_provider(
    _continuation: *mut Continuation,
    _operation: u64,
    _arguments: *const u8,
    _arguments_size: usize,
    _result: *mut u8,
    _result_size: usize,
) -> u8 {
    1
}

pub(super) unsafe extern "C" fn cancel_provider(
    _continuation: *mut Continuation,
    _operation: u64,
) -> u8 {
    PROVIDER_CANCEL_COUNT.fetch_add(1, Ordering::SeqCst);
    1
}
