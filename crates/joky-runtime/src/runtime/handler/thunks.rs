//! Handler thunk registration and invocation ABI.

use super::*;

/// Register a generated handler body and its canonical capture environment.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_thunk_with_env(
    frame: *mut HandlerFrame,
    operation: u64,
    thunk: *mut std::ffi::c_void,
    captures: *const u8,
    captures_size: usize,
    result_capacity: usize,
    slots: *const HandlerEnvSlot,
    slot_count: usize,
) -> u8 {
    unsafe {
        set_resumption_thunk_with_env_impl(
            frame,
            operation,
            thunk,
            captures,
            captures_size,
            result_capacity,
            slots,
            slot_count,
            std::ptr::null(),
            0,
        )
    }
}

/// Register a generated thunk together with ownership descriptors for its
/// flattened result. Result descriptors use offsets relative to the result
/// buffer, unlike capture descriptors which are relative to `captures`.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_thunk_with_result_env(
    frame: *mut HandlerFrame,
    operation: u64,
    thunk: *mut std::ffi::c_void,
    captures: *const u8,
    captures_size: usize,
    result_capacity: usize,
    slots: *const HandlerEnvSlot,
    slot_count: usize,
    result_slots: *const HandlerEnvSlot,
    result_slot_count: usize,
) -> u8 {
    unsafe {
        set_resumption_thunk_with_env_impl(
            frame,
            operation,
            thunk,
            captures,
            captures_size,
            result_capacity,
            slots,
            slot_count,
            result_slots,
            result_slot_count,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn set_resumption_thunk_with_env_impl(
    frame: *mut HandlerFrame,
    operation: u64,
    thunk: *mut std::ffi::c_void,
    captures: *const u8,
    captures_size: usize,
    result_capacity: usize,
    slots: *const HandlerEnvSlot,
    slot_count: usize,
    result_slots: *const HandlerEnvSlot,
    result_slot_count: usize,
) -> u8 {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return 0;
    };
    if !frame.state.operations.contains(&operation)
        || thunk.is_null()
        || (captures_size != 0 && captures.is_null())
        || (slot_count != 0 && slots.is_null())
        || (result_slot_count != 0 && result_slots.is_null())
        || result_capacity == 0
    {
        return 0;
    }
    let raw_slots = if slot_count == 0 {
        &[][..]
    } else {
        // SAFETY: caller supplies exactly `slot_count` descriptors.
        unsafe { std::slice::from_raw_parts(slots, slot_count) }
    };
    let raw_result_slots = if result_slot_count == 0 {
        &[][..]
    } else {
        // SAFETY: caller supplies exactly `result_slot_count` descriptors.
        unsafe { std::slice::from_raw_parts(result_slots, result_slot_count) }
    };
    if raw_slots.iter().any(|slot| {
        slot.offset % std::mem::size_of::<usize>() != 0
            || slot.offset > captures_size
            || captures_size.saturating_sub(slot.offset) < std::mem::size_of::<usize>()
            || ((slot.ownership == HandlerEnvOwnership::Owned as u8
                || slot.ownership == HandlerEnvOwnership::Shared as u8)
                && slot.drop_callback.is_null())
            || (slot.ownership == HandlerEnvOwnership::Borrowed as u8
                && !slot.drop_callback.is_null())
            || (slot.ownership != HandlerEnvOwnership::Owned as u8
                && slot.ownership != HandlerEnvOwnership::Borrowed as u8
                && slot.ownership != HandlerEnvOwnership::Shared as u8)
    }) {
        return 0;
    }
    if raw_result_slots.iter().any(|slot| {
        slot.offset % std::mem::size_of::<usize>() != 0
            || slot.offset > result_capacity
            || result_capacity.saturating_sub(slot.offset) < std::mem::size_of::<usize>()
            || slot.ownership != HandlerEnvOwnership::Owned as u8
            || slot.drop_callback.is_null()
    }) {
        return 0;
    }
    let environment_slots = raw_slots
        .iter()
        .map(|slot| OwnedHandlerEnvSlot {
            offset: slot.offset,
            ownership: match slot.ownership {
                value if value == HandlerEnvOwnership::Borrowed as u8 => {
                    HandlerEnvOwnership::Borrowed
                }
                value if value == HandlerEnvOwnership::Shared as u8 => HandlerEnvOwnership::Shared,
                _ => HandlerEnvOwnership::Owned,
            },
            drop_callback: slot.drop_callback as usize,
        })
        .collect::<Vec<_>>();
    let capture_bytes = if captures_size == 0 {
        Vec::new()
    } else {
        // SAFETY: caller supplies a valid, read-only capture byte region.
        unsafe { std::slice::from_raw_parts(captures, captures_size) }.to_vec()
    };
    let result_slots = raw_result_slots
        .iter()
        .map(|slot| (slot.offset, slot.drop_callback as usize))
        .collect::<Vec<_>>();
    frame
        .state
        .resumable_thunks
        .lock()
        .expect("handler thunk mutex")
        .remove(&operation);
    let context = Box::new(HandlerThunkContext {
        thunk: thunk as usize,
        environment: HandlerEnv {
            captures: capture_bytes,
            slots: environment_slots,
        },
        result_capacity,
        registered_payload: false,
        result_slots,
    });
    let context_pointer = (&*context as *const HandlerThunkContext)
        .cast_mut()
        .cast::<std::ffi::c_void>();
    frame
        .state
        .resumable_owned_thunk_contexts
        .lock()
        .expect("handler thunk context ownership mutex")
        .insert(operation, context);
    frame
        .state
        .resumable_thunks
        .lock()
        .expect("handler thunk mutex")
        .insert(operation, context_pointer as usize);
    1
}

/// Register a generated handler body with a capture-free environment.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_thunk(
    frame: *mut HandlerFrame,
    operation: u64,
    thunk: *mut std::ffi::c_void,
    captures: *const u8,
    captures_size: usize,
    result_capacity: usize,
) -> u8 {
    unsafe {
        jk_handler_frame_set_resumption_thunk_with_env(
            frame,
            operation,
            thunk,
            captures,
            captures_size,
            result_capacity,
            std::ptr::null(),
            0,
        )
    }
}

/// Register a canonical thunk that consumes the frame-owned response payload.
/// The payload remains owned by the frame until generated code calls
/// `jk_handler_frame_resumption_payload_consume` after dispatch.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_payload_thunk(
    frame: *mut HandlerFrame,
    operation: u64,
    thunk: *mut std::ffi::c_void,
    result_capacity: usize,
) -> u8 {
    let Some(frame_ref) = (unsafe { frame.as_ref() }) else {
        return 0;
    };
    if !frame_ref.state.operations.contains(&operation)
        || thunk.is_null()
        || result_capacity == 0
        || !frame_ref
            .state
            .resumable_payloads
            .lock()
            .expect("handler payload mutex")
            .contains_key(&operation)
    {
        return 0;
    }
    let context = Box::new(HandlerThunkContext {
        thunk: thunk as usize,
        environment: HandlerEnv {
            captures: Vec::new(),
            slots: Vec::new(),
        },
        result_capacity,
        registered_payload: true,
        result_slots: Vec::new(),
    });
    let context_pointer = (&*context as *const HandlerThunkContext)
        .cast_mut()
        .cast::<std::ffi::c_void>();
    frame_ref
        .state
        .resumable_owned_thunk_contexts
        .lock()
        .expect("handler thunk context ownership mutex")
        .insert(operation, context);
    frame_ref
        .state
        .resumable_thunks
        .lock()
        .expect("handler thunk mutex")
        .insert(operation, context_pointer as usize);
    1
}

/// Begin a request and retain its flattened operation arguments for the
/// canonical handler thunk. The parameterless entry point remains useful for
/// operations without arguments.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_begin_resumption_handle_with_payload(
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> *mut ResumptionHandle {
    let handle = unsafe { jk_handler_frame_begin_resumption_handle(operation) };
    if handle.is_null() {
        // Normal requests are lowered to this ABI even when the callee was
        // compiled without a statically visible handler (for example, a
        // recursive or indirect call). Preserve the language-level
        // unhandled-effect diagnostic instead of manufacturing a null
        // continuation result.
        crate::runtime::task::jk_task_abort(operation);
        return handle;
    }
    if payload_size != 0 && payload.is_null() {
        unsafe { jk_handler_frame_free_resumption_handle(handle) };
        return std::ptr::null_mut();
    }
    if payload_size != 0 {
        let bytes = unsafe { std::slice::from_raw_parts(payload, payload_size) };
        unsafe { &*handle }
            .request_payload
            .lock()
            .expect("handler request payload mutex")
            .extend_from_slice(bytes);
    }
    handle
}

/// Begin a request and transfer ownership descriptors for managed pointer
/// words embedded in its flattened payload. Descriptors are relative to the
/// copied request buffer and must all describe uniquely owned values.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_begin_resumption_handle_with_payload_env(
    operation: u64,
    payload: *const u8,
    payload_size: usize,
    slots: *const HandlerEnvSlot,
    slot_count: usize,
) -> *mut ResumptionHandle {
    let handle = unsafe {
        jk_handler_frame_begin_resumption_handle_with_payload(operation, payload, payload_size)
    };
    if handle.is_null() {
        // The payload was copied into no handle because no matching frame is
        // active. Release every owned pointer described by the caller before
        // returning; the base entry point has already recorded the failure.
        if slot_count != 0 && !slots.is_null() && (payload_size == 0 || !payload.is_null()) {
            let raw_slots = unsafe { std::slice::from_raw_parts(slots, slot_count) };
            if raw_slots.iter().all(|slot| {
                slot.offset % std::mem::size_of::<usize>() == 0
                    && slot.offset <= payload_size
                    && payload_size.saturating_sub(slot.offset) >= std::mem::size_of::<usize>()
                    && matches!(
                        slot.ownership,
                        value if value == HandlerEnvOwnership::Owned as u8
                            || value == HandlerEnvOwnership::Shared as u8
                    )
                    && !slot.drop_callback.is_null()
            }) {
                let managed = raw_slots
                    .iter()
                    .map(|slot| (slot.offset, slot.drop_callback as usize))
                    .collect::<Vec<_>>();
                let bytes = if payload_size == 0 {
                    &[][..]
                } else {
                    unsafe { std::slice::from_raw_parts(payload, payload_size) }
                };
                release_managed_bytes(bytes, &managed);
            }
        }
        return handle;
    }
    if slot_count != 0 && slots.is_null() {
        unsafe { jk_handler_frame_free_resumption_handle(handle) };
        return std::ptr::null_mut();
    }
    let raw_slots = if slot_count == 0 {
        &[][..]
    } else {
        // SAFETY: the generated caller supplies exactly `slot_count` entries.
        unsafe { std::slice::from_raw_parts(slots, slot_count) }
    };
    if raw_slots.iter().any(|slot| {
        slot.offset % std::mem::size_of::<usize>() != 0
            || slot.offset > payload_size
            || payload_size.saturating_sub(slot.offset) < std::mem::size_of::<usize>()
            || !matches!(
                slot.ownership,
                value if value == HandlerEnvOwnership::Owned as u8
                    || value == HandlerEnvOwnership::Shared as u8
            )
            || slot.drop_callback.is_null()
    }) {
        unsafe { jk_handler_frame_free_resumption_handle(handle) };
        return std::ptr::null_mut();
    }
    let handle_ref = unsafe { &*handle };
    let owned = raw_slots
        .iter()
        .filter(|slot| slot.ownership == HandlerEnvOwnership::Owned as u8)
        .map(|slot| (slot.offset, slot.drop_callback as usize))
        .collect::<Vec<_>>();
    let shared = raw_slots
        .iter()
        .filter(|slot| slot.ownership == HandlerEnvOwnership::Shared as u8)
        .map(|slot| (slot.offset, slot.drop_callback as usize))
        .collect::<Vec<_>>();
    handle_ref
        .request_managed
        .lock()
        .expect("handler request ownership mutex")
        .extend(owned);
    handle_ref
        .request_shared
        .lock()
        .expect("handler shared request ownership mutex")
        .extend(shared);
    handle
}

/// Invoke the canonical thunk installed for a request.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_dispatch_resumption_handle(
    handle: *mut ResumptionHandle,
) -> u8 {
    let Some(handle_ref) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    let context = handle_ref
        .state
        .resumable_thunks
        .lock()
        .expect("handler thunk mutex")
        .get(&handle_ref.operation)
        .copied()
        .unwrap_or(0);
    if context == 0 {
        return 0;
    }
    unsafe { jk_handler_frame_invoke_thunk(handle, context as *mut std::ffi::c_void) }
}

/// Invoke a canonical handler thunk. This is the single runtime ABI for
/// generated handler bodies and payload adapters.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_invoke_thunk(
    handle: *mut ResumptionHandle,
    context: *mut std::ffi::c_void,
) -> u8 {
    let Some(context) = (unsafe { (context as *mut HandlerThunkContext).as_mut() }) else {
        return 0;
    };
    let request_size = unsafe { jk_handler_frame_request_payload_size(handle) };
    let mut request = vec![0_u8; request_size];
    if request_size != 0
        && unsafe {
            jk_handler_frame_request_payload_copy(handle, request.as_mut_ptr(), request_size)
        } != request_size
    {
        return 0;
    }
    let configured = if context.registered_payload {
        unsafe { handle.as_ref() }
            .and_then(|handle| {
                handle
                    .state
                    .resumable_payloads
                    .lock()
                    .ok()
                    .and_then(|payloads| {
                        payloads
                            .get(&handle.operation)
                            .map(ResumptionPayload::flattened)
                    })
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let capture_bytes = if context.registered_payload {
        configured.as_slice()
    } else {
        context.environment.captures.as_slice()
    };
    let mut result = vec![0_u8; context.result_capacity];
    let mut call = HandlerCall {
        operation: unsafe { jk_handler_frame_resumption_handle_operation(handle) },
        request: request.as_ptr(),
        request_size,
        captures: capture_bytes.as_ptr(),
        captures_size: capture_bytes.len(),
        result: result.as_mut_ptr(),
        result_capacity: result.len(),
        result_size: 0,
    };
    // SAFETY: registration stores a generated function with this canonical
    // ABI and keeps its capture context alive for the handler frame lifetime.
    let thunk: HandlerThunk = unsafe { std::mem::transmute(context.thunk) };
    if unsafe { thunk(&mut call) } == 0 || call.result_size > call.result_capacity {
        release_managed_bytes(&result, &context.result_slots);
        return 0;
    }
    // Shared request arguments are borrowed by the typed thunk. Generated
    // shared-returning functions duplicate a borrowed value before returning
    // it, so the request-side duplicates are always released after a
    // successful thunk. This keeps request ownership independent from result
    // ownership and also handles aggregate arguments without alias guessing.
    if let Some(handle_ref) = unsafe { handle.as_ref() } {
        let mut shared = handle_ref
            .request_shared
            .lock()
            .expect("handler shared request ownership mutex");
        let request = handle_ref
            .request_payload
            .lock()
            .expect("handler request payload mutex");
        for (offset, callback) in shared.drain(..) {
            release_managed_bytes(&request, &[(offset, callback)]);
        }
    }
    // The typed thunk has now received the managed operation arguments. Their
    // ownership belongs to its parameter locals (and any returned value), so
    // the request token must no longer release those pointer words.
    let Some(handle_ref) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    handle_ref
        .request_managed
        .lock()
        .expect("handler request ownership mutex")
        .clear();
    let operation = call.operation;
    let resumed = unsafe {
        resume_handle_managed(
            handle,
            operation,
            result.as_ptr(),
            call.result_size,
            &context.result_slots,
        )
    };
    if resumed != 0 {
        context
            .environment
            .transfer_owned_result(&result[..call.result_size]);
    } else {
        release_managed_bytes(&result, &context.result_slots);
    }
    resumed
}
