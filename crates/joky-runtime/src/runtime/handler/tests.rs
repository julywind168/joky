use std::ffi::c_void;

use super::*;

unsafe extern "C" fn complete_bound_continuation(
    continuation: *mut crate::runtime::continuation::Continuation,
) {
    let _ = unsafe { crate::runtime::continuation::jk_continuation_complete(continuation) };
}

unsafe extern "C" fn canonical_echo_thunk(call: *mut HandlerCall) -> u8 {
    let Some(call) = (unsafe { call.as_mut() }) else {
        return 0;
    };
    if call.request_size != call.captures_size
        || call.request_size > call.result_capacity
        || (call.request_size != 0
            && (call.request.is_null() || call.captures.is_null() || call.result.is_null()))
    {
        return 0;
    }
    if call.request_size != 0 {
        // SAFETY: the runtime supplied non-overlapping buffers with the
        // exact lengths advertised by HandlerCall.
        unsafe {
            std::ptr::copy_nonoverlapping(call.request, call.result, call.request_size);
        }
    }
    call.result_size = call.request_size;
    1
}

unsafe extern "C" fn canonical_return_capture_thunk(call: *mut HandlerCall) -> u8 {
    let Some(call) = (unsafe { call.as_mut() }) else {
        return 0;
    };
    if call.captures_size != 8
        || call.result_capacity < 8
        || call.captures.is_null()
        || call.result.is_null()
    {
        return 0;
    }
    // SAFETY: the test installs an eight-byte capture and result buffer.
    unsafe { std::ptr::copy_nonoverlapping(call.captures, call.result, 8) };
    call.result_size = 8;
    1
}

unsafe extern "C" fn drop_environment_pointer(pointer: *mut u8) {
    jk_drop(pointer);
}

#[test]
fn nested_frames_choose_the_nearest_matching_operation() {
    let outer = unsafe { jk_handler_frame_new([7_u64, 9].as_ptr(), 2) };
    assert_eq!(unsafe { jk_handler_frame_enter(outer) }, 1);
    assert!(std::ptr::eq(nearest_operation(7).unwrap(), outer));
    let inner = unsafe { jk_handler_frame_new([7_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(inner) }, 1);
    assert!(std::ptr::eq(nearest_operation(7).unwrap(), inner));
    assert!(std::ptr::eq(nearest_operation(9).unwrap(), outer));
    assert_eq!(unsafe { jk_handler_frame_exit(inner) }, 1);
    unsafe { jk_handler_frame_free(inner) };
    assert_eq!(unsafe { jk_handler_frame_exit(outer) }, 1);
    unsafe { jk_handler_frame_free(outer) };
}

#[test]
fn configured_resumption_payload_is_copied_into_new_handles() {
    let frame = unsafe { jk_handler_frame_new([19_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let payload = 42_u64.to_ne_bytes();
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_payload(frame, 19, payload.as_ptr(), payload.len())
        },
        1
    );
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(19) };
    assert!(!handle.is_null());
    let mut copied = [0_u8; 8];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(handle, copied.as_mut_ptr(), copied.len())
        },
        payload.len()
    );
    assert_eq!(copied, payload);
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn managed_string_resumption_payload_is_owned_by_the_frame() {
    let baseline = crate::runtime::managed::live_object_count();
    let bytes = b"Joky";
    let string = crate::runtime::string::jk_string_from_utf8(bytes.as_ptr(), bytes.len());
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);

    let frame = unsafe { jk_handler_frame_new([47_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(
        unsafe { jk_handler_frame_set_resumption_string(frame, 47, string, bytes.len()) },
        1
    );
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(47) };
    assert!(!handle.is_null());

    let mut payload = [0_u8; 16];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(handle, payload.as_mut_ptr(), payload.len())
        },
        payload.len()
    );
    let pointer = u64::from_ne_bytes(payload[..8].try_into().expect("pointer bytes"));
    let length = u64::from_ne_bytes(payload[8..].try_into().expect("length bytes"));
    assert_eq!(pointer, string as usize as u64);
    assert_eq!(length, bytes.len() as u64);

    let returned = crate::runtime::managed::jk_dup(pointer as usize as *mut u8);
    assert_eq!(returned, string);
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);
    jk_drop(returned);
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
}

#[test]
fn managed_aggregate_resumption_payload_releases_all_pointer_words() {
    let baseline = crate::runtime::managed::live_object_count();
    let ok_bytes = b"ok";
    let err_bytes = b"err";
    let ok = crate::runtime::string::jk_string_from_utf8(ok_bytes.as_ptr(), ok_bytes.len());
    let err = crate::runtime::string::jk_string_from_utf8(err_bytes.as_ptr(), err_bytes.len());
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 2);

    // Result<String, String>: tag, ok pointer/length, err pointer/length.
    let mut payload = [0_u8; 40];
    payload[0..8].copy_from_slice(&0_u64.to_ne_bytes());
    payload[8..16].copy_from_slice(&(ok as usize as u64).to_ne_bytes());
    payload[16..24].copy_from_slice(&(ok_bytes.len() as u64).to_ne_bytes());
    payload[24..32].copy_from_slice(&(err as usize as u64).to_ne_bytes());
    payload[32..40].copy_from_slice(&(err_bytes.len() as u64).to_ne_bytes());
    let offsets = [8_usize, 24_usize];

    let frame = unsafe { jk_handler_frame_new([53_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_managed_payload(
                frame,
                53,
                payload.as_ptr(),
                payload.len(),
                offsets.as_ptr(),
                offsets.len(),
            )
        },
        1
    );

    // The frame owns both references. Keep one duplicate of each alive so
    // the post-frame count proves both original words were released.
    let kept_ok = crate::runtime::managed::jk_dup(ok);
    let kept_err = crate::runtime::managed::jk_dup(err);
    assert_eq!(kept_ok, ok);
    assert_eq!(kept_err, err);
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(53) };
    assert!(!handle.is_null());
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 2);
    crate::runtime::managed::jk_drop(kept_ok);
    crate::runtime::managed::jk_drop(kept_err);
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
}

#[test]
fn managed_resumption_payload_rejects_invalid_offsets_without_leaking() {
    let baseline = crate::runtime::managed::live_object_count();
    let bytes = b"offset";
    let string = crate::runtime::string::jk_string_from_utf8(bytes.as_ptr(), bytes.len());
    let payload = (string as usize as u64).to_ne_bytes();
    let offsets = [0_usize, 8_usize];
    let frame = unsafe { jk_handler_frame_new([59_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_managed_payload(
                frame,
                59,
                payload.as_ptr(),
                payload.len(),
                offsets.as_ptr(),
                offsets.len(),
            )
        },
        0
    );
    // The valid word is released even though the second offset is out of
    // bounds, so the rejected transfer does not leak its managed payload.
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn managed_resumption_payload_keeps_ownership_when_offsets_are_missing() {
    let baseline = crate::runtime::managed::live_object_count();
    let bytes = b"missing";
    let string = crate::runtime::string::jk_string_from_utf8(bytes.as_ptr(), bytes.len());
    let payload = (string as usize as u64).to_ne_bytes();
    let frame = unsafe { jk_handler_frame_new([61_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_managed_payload(
                frame,
                61,
                payload.as_ptr(),
                payload.len(),
                std::ptr::null(),
                1,
            )
        },
        0
    );
    // A missing offset list rejects the transfer; the caller still owns
    // the String and can release it normally.
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);
    crate::runtime::managed::jk_drop(string);
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn a_frame_accepts_exactly_one_resumption_payload() {
    let frame = unsafe { jk_handler_frame_new([17_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let selected = begin_resumption(17).expect("matching frame");
    assert!(std::ptr::eq(selected, frame));
    let payload = 42_u64.to_ne_bytes();
    assert!(unsafe { resume(selected, 17, payload.as_ptr(), payload.len()) });
    assert!(!unsafe { resume(selected, 17, payload.as_ptr(), payload.len()) });
    assert_eq!(
        unsafe { &*frame }
            .state
            .resume_payload
            .lock()
            .expect("handler resume payload mutex")
            .as_slice(),
        payload
    );
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn independent_resumption_handles_are_each_one_shot() {
    let frame = unsafe { jk_handler_frame_new([23_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let first = unsafe { jk_handler_frame_begin_resumption_handle(23) };
    let second = unsafe { jk_handler_frame_begin_resumption_handle(23) };
    assert!(!first.is_null());
    assert!(!second.is_null());
    let payload = 7_u64.to_ne_bytes();
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(first, 23, payload.as_ptr(), payload.len()) },
        1
    );
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(first, 23, payload.as_ptr(), payload.len()) },
        0
    );
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(second, 23, payload.as_ptr(), payload.len()) },
        1
    );
    let mut copied = [0_u8; 8];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(second, copied.as_mut_ptr(), copied.len())
        },
        payload.len()
    );
    assert_eq!(&copied[..payload.len()], payload);
    unsafe { jk_handler_frame_free_resumption_handle(first) };
    unsafe { jk_handler_frame_free_resumption_handle(second) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn a_resumption_handle_dispatches_its_bound_continuation() {
    let frame = unsafe { jk_handler_frame_new([31_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(31) };
    let continuation = crate::runtime::continuation::jk_continuation_new(0);
    assert_eq!(
        unsafe {
            crate::runtime::continuation::jk_continuation_set_resume_callback(
                continuation,
                complete_bound_continuation as *mut c_void,
            )
        },
        1
    );
    assert_eq!(
        unsafe { jk_handler_frame_bind_resumption_handle(handle, continuation) },
        1
    );
    let payload = 9_u64.to_ne_bytes();
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(handle, 31, payload.as_ptr(), payload.len()) },
        1
    );
    assert_eq!(
        unsafe { crate::runtime::continuation::jk_continuation_state(continuation) },
        crate::runtime::continuation::ContinuationState::Completed as u8
    );
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    unsafe { crate::runtime::continuation::jk_continuation_free(continuation) };
}

#[test]
fn canonical_forward_thunk_receives_request_payload() {
    let frame = unsafe { jk_handler_frame_new([37_u64].as_ptr(), 1) };
    let context = 8_u64 << 32;
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk(
                frame,
                37,
                jk_handler_frame_forward_thunk as *mut c_void,
                context.to_ne_bytes().as_ptr(),
                std::mem::size_of::<u64>(),
                8,
            )
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let request = 123_u64.to_ne_bytes();
    let handle = unsafe {
        jk_handler_frame_begin_resumption_handle_with_payload(37, request.as_ptr(), request.len())
    };
    assert!(!handle.is_null());
    assert_eq!(
        unsafe { jk_handler_frame_dispatch_resumption_handle(handle) },
        1
    );
    let mut response = [0_u8; 8];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(handle, response.as_mut_ptr(), response.len())
        },
        request.len()
    );
    assert_eq!(&response, &request);
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn canonical_thunk_uses_one_stable_abi() {
    let frame = unsafe { jk_handler_frame_new([71_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let captures = [9_u8, 8_u8];
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk(
                frame,
                71,
                canonical_echo_thunk as *mut c_void,
                captures.as_ptr(),
                captures.len(),
                16,
            )
        },
        1
    );
    let request = [3_u8, 4_u8];
    let handle = unsafe {
        jk_handler_frame_begin_resumption_handle_with_payload(71, request.as_ptr(), request.len())
    };
    assert!(!handle.is_null());
    assert_eq!(
        unsafe { jk_handler_frame_dispatch_resumption_handle(handle) },
        1
    );
    let mut result = [0_u8; 8];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(handle, result.as_mut_ptr(), result.len())
        },
        request.len()
    );
    assert_eq!(&result[..request.len()], &request);
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn handler_environment_releases_owned_pointer_slots() {
    let baseline = crate::runtime::managed::live_object_count();
    let object = crate::runtime::managed::jk_alloc_object(RuntimeValueKind::String as u8, 0, 1);
    assert!(!object.is_null());
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);

    let frame = unsafe { jk_handler_frame_new([73_u64].as_ptr(), 1) };
    let capture = (object as usize).to_ne_bytes();
    let slot = HandlerEnvSlot {
        offset: 0,
        ownership: HandlerEnvOwnership::Owned as u8,
        drop_callback: drop_environment_pointer as *mut c_void,
    };
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk_with_env(
                frame,
                73,
                canonical_echo_thunk as *mut c_void,
                capture.as_ptr(),
                capture.len(),
                8,
                &slot,
                1,
            )
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
}

#[test]
fn handler_environment_does_not_release_borrowed_class_slots() {
    let baseline = crate::runtime::managed::live_object_count();
    let object = crate::runtime::managed::jk_alloc_object(RuntimeValueKind::Class as u8, 0, 1);
    assert!(!object.is_null());
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);

    let frame = unsafe { jk_handler_frame_new([75_u64].as_ptr(), 1) };
    let capture = (object as usize).to_ne_bytes();
    let slot = HandlerEnvSlot {
        offset: 0,
        ownership: HandlerEnvOwnership::Borrowed as u8,
        drop_callback: std::ptr::null_mut(),
    };
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk_with_env(
                frame,
                75,
                canonical_echo_thunk as *mut c_void,
                capture.as_ptr(),
                capture.len(),
                8,
                &slot,
                1,
            )
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };

    // A borrowed class remains owned by its lexical scope; freeing the
    // handler environment must not release it.
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);
    crate::runtime::managed::jk_drop(object);
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
}

#[test]
fn handler_environment_transfers_an_owned_capture_returned_by_thunk() {
    let baseline = crate::runtime::managed::live_object_count();
    let object = crate::runtime::managed::jk_alloc_object(RuntimeValueKind::MutList as u8, 0, 1);
    assert!(!object.is_null());

    let frame = unsafe { jk_handler_frame_new([74_u64].as_ptr(), 1) };
    let capture = (object as usize).to_ne_bytes();
    let slot = HandlerEnvSlot {
        offset: 0,
        ownership: HandlerEnvOwnership::Owned as u8,
        drop_callback: drop_environment_pointer as *mut c_void,
    };
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk_with_env(
                frame,
                74,
                canonical_return_capture_thunk as *mut c_void,
                capture.as_ptr(),
                capture.len(),
                8,
                &slot,
                1,
            )
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let handle =
        unsafe { jk_handler_frame_begin_resumption_handle_with_payload(74, std::ptr::null(), 0) };
    assert!(!handle.is_null());
    assert_eq!(
        unsafe { jk_handler_frame_dispatch_resumption_handle(handle) },
        1
    );
    let mut result = [0_u8; 8];
    assert_eq!(
        unsafe { jk_handler_frame_resumption_payload_copy(handle, result.as_mut_ptr(), 8) },
        8
    );
    let returned = usize::from_ne_bytes(result);
    assert_eq!(returned, object as usize);
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    // The handle pins the environment until it is freed. Its owned slot
    // was cleared when the thunk returned the capture, leaving the result
    // as the sole owner.
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(crate::runtime::managed::live_object_count(), baseline + 1);
    jk_drop(returned as *mut u8);
    assert_eq!(crate::runtime::managed::live_object_count(), baseline);
}

#[test]
fn canonical_transform_thunk_preserves_context() {
    let frame = unsafe { jk_handler_frame_new([61_u64].as_ptr(), 1) };
    let context = 77_u64 << 32;
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_thunk(
                frame,
                61,
                jk_handler_frame_transform_i64_thunk as *mut c_void,
                context.to_ne_bytes().as_ptr(),
                std::mem::size_of::<u64>(),
                8,
            )
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let request = 5_u64.to_ne_bytes();
    let handle = unsafe {
        jk_handler_frame_begin_resumption_handle_with_payload(61, request.as_ptr(), request.len())
    };
    assert!(!handle.is_null());
    assert_eq!(
        unsafe { jk_handler_frame_request_payload_size(handle) },
        request.len()
    );
    assert_eq!(
        unsafe { jk_handler_frame_dispatch_resumption_handle(handle) },
        1
    );
    let mut response = [0_u8; 8];
    assert_eq!(
        unsafe { jk_handler_frame_resumption_payload_copy(handle, response.as_mut_ptr(), 8) },
        8
    );
    assert_eq!(u64::from_ne_bytes(response), 82);
    assert_eq!(
        unsafe { jk_handler_frame_resumption_payload_size(handle) },
        response.len()
    );
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn static_thunk_preserves_payloads_larger_than_legacy_buffer() {
    let frame = unsafe { jk_handler_frame_new([62_u64].as_ptr(), 1) };
    let payload = vec![0x5a_u8; 5001];
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_payload(frame, 62, payload.as_ptr(), payload.len())
        },
        1
    );
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(62) };
    assert!(!handle.is_null());
    assert_eq!(
        unsafe {
            jk_handler_frame_set_resumption_payload_thunk(
                frame,
                62,
                jk_handler_frame_static_thunk as *mut c_void,
                payload.len(),
            )
        },
        1
    );
    assert_eq!(
        unsafe { jk_handler_frame_dispatch_resumption_handle(handle) },
        1
    );
    assert_eq!(
        unsafe { jk_handler_frame_resumption_payload_size(handle) },
        payload.len()
    );
    let mut output = vec![0_u8; payload.len()];
    assert_eq!(
        unsafe {
            jk_handler_frame_resumption_payload_copy(handle, output.as_mut_ptr(), output.len())
        },
        payload.len()
    );
    assert_eq!(output, payload);
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
}

#[test]
fn binding_after_resume_dispatches_the_pending_continuation() {
    let frame = unsafe { jk_handler_frame_new([33_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(33) };
    let payload = 5_u64.to_ne_bytes();
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(handle, 33, payload.as_ptr(), payload.len()) },
        1
    );
    let continuation = crate::runtime::continuation::jk_continuation_new(0);
    assert_eq!(
        unsafe {
            crate::runtime::continuation::jk_continuation_set_resume_callback(
                continuation,
                complete_bound_continuation as *mut c_void,
            )
        },
        1
    );
    assert_eq!(
        unsafe { jk_handler_frame_bind_resumption_handle(handle, continuation) },
        1
    );
    assert_eq!(
        unsafe { crate::runtime::continuation::jk_continuation_state(continuation) },
        crate::runtime::continuation::ContinuationState::Completed as u8
    );
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };
    unsafe { crate::runtime::continuation::jk_continuation_free(continuation) };
}

#[test]
fn a_resumption_handle_survives_its_frame_exit() {
    let frame = unsafe { jk_handler_frame_new([41_u64].as_ptr(), 1) };
    assert_eq!(unsafe { jk_handler_frame_enter(frame) }, 1);
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(41) };
    assert!(!handle.is_null());
    assert_eq!(unsafe { jk_handler_frame_exit(frame) }, 1);
    unsafe { jk_handler_frame_free(frame) };

    let payload = 8_u64.to_ne_bytes();
    assert_eq!(
        unsafe { jk_handler_frame_resume_handle(handle, 41, payload.as_ptr(), payload.len()) },
        1
    );
    unsafe { jk_handler_frame_free_resumption_handle(handle) };
}
