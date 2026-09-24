use super::*;

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_alloc_frame(
    continuation: *mut Continuation,
    size: usize,
) -> *mut u8 {
    unsafe { retain_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.allocate_frame(size)
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_release_frame(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.release_frame())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_frame_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.frame_pointer()
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_frame_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }.map_or(0, |continuation| continuation.frame_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_set_program_counter(
    continuation: *mut Continuation,
    program_counter: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.set_program_counter(program_counter))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_program_counter(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }
        .map_or(0, |continuation| continuation.program_counter())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_alloc_result(
    continuation: *mut Continuation,
    size: usize,
) -> *mut u8 {
    unsafe { retain_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.allocate_result(size)
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_release_result(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.release_result())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_result_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.result_pointer()
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_result_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }
        .map_or(0, |continuation| continuation.result_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_alloc_suspend_result(
    continuation: *mut Continuation,
    size: usize,
) -> *mut u8 {
    unsafe { retain_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.allocate_suspend_result(size)
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_alloc_failure(
    continuation: *mut Continuation,
    size: usize,
) -> *mut u8 {
    unsafe { retain_handle(continuation) }
        .map_or(std::ptr::null_mut(), |value| value.allocate_failure(size))
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_release_failure(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|value| value.release_failure())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_failure_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }
        .map_or(std::ptr::null_mut(), |value| value.failure_pointer())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_failure_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }.map_or(0, |value| value.failure_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_release_suspend_result(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.release_suspend_result())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_suspend_result_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.suspend_result_pointer()
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_suspend_result_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }
        .map_or(0, |continuation| continuation.suspend_result_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_suspend_arguments_pointer(
    continuation: *const Continuation,
) -> *mut u8 {
    unsafe { retain_const_handle(continuation) }.map_or(std::ptr::null_mut(), |continuation| {
        continuation.suspend_argument_pointer()
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_suspend_arguments_size(
    continuation: *const Continuation,
) -> usize {
    unsafe { retain_const_handle(continuation) }
        .map_or(0, |continuation| continuation.suspend_argument_size())
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_clear_suspend_cleanups(
    continuation: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.clear_suspend_cleanups())
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_register_cleanup(
    continuation: *mut Continuation,
    storage: u8,
    offset: usize,
    callback: *mut c_void,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.register_cleanup(storage, offset, callback, false))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_register_cleanup_region(
    continuation: *mut Continuation,
    storage: u8,
    offset: usize,
    callback: *mut c_void,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.register_cleanup(storage, offset, callback, true))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_disarm_cleanup(
    continuation: *mut Continuation,
    storage: u8,
    offset: usize,
) -> u8 {
    unsafe { retain_handle(continuation) }
        .is_some_and(|continuation| continuation.disarm_cleanup(storage, offset))
        .into()
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_free(continuation: *mut Continuation) {
    let Some(value) = (unsafe { retain_handle(continuation) }) else {
        return;
    };
    // Keep the runtime state alive independently of the public token. A resume
    // callback may retire the token while this caller waits for its lease.
    let owned = value;
    let first_request = {
        let _gate = owned
            .inner
            .lifecycle_gate
            .lock()
            .expect("continuation lifecycle mutex");
        owned.remember_owner_handle(continuation);
        owned
            .inner
            .free_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    };
    if first_request {
        owned.cancel();
    }
    // Generated resume entries free their continuation from inside the
    // scheduler callback. Waiting here would wait on the callback itself; its
    // `CallbackLease` will call `finish_free` after returning instead.
    if owned.in_current_callback() {
        return;
    }
    owned.wait_for_idle();
    owned.finish_free();
}
