//! Handler frame and resumption lifecycle ABI.

use super::*;

pub(crate) fn current() -> *const HandlerFrame {
    CURRENT_HANDLER.with(Cell::get)
}

pub(crate) unsafe fn install(frame: *const HandlerFrame) -> *const HandlerFrame {
    CURRENT_HANDLER.with(|current| {
        let previous = current.get();
        current.set(frame);
        previous
    })
}

pub(crate) fn nearest_operation(operation: u64) -> Option<*const HandlerFrame> {
    let mut frame = current();
    while let Some(value) = unsafe { frame.as_ref() } {
        if value.state.active.load(Ordering::Acquire) && value.state.operations.contains(&operation)
        {
            return Some(frame);
        }
        frame = value.parent;
    }
    None
}

/// Reserve the nearest matching frame for one resumable request. A frame may
/// hold at most one pending request; nested frames naturally win the lookup.
pub(crate) fn begin_resumption(operation: u64) -> Option<*const HandlerFrame> {
    let frame = nearest_operation(operation)?;
    let value = unsafe { &*frame };
    value
        .state
        .pending_operation
        .compare_exchange(
            NO_PENDING_OPERATION,
            operation,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .ok()
        .map(|_| frame)
}

/// Complete a reserved continuation exactly once and copy its ABI payload.
pub(crate) unsafe fn resume(
    frame: *const HandlerFrame,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> bool {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return false;
    };
    if !frame.state.active.load(Ordering::Acquire)
        || frame.state.pending_operation.load(Ordering::Acquire) != operation
        || frame
            .state
            .resumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return false;
    }
    let bytes = if payload_size == 0 {
        &[]
    } else if payload.is_null() {
        return false;
    } else {
        // SAFETY: caller owns the flattened ABI payload for this call.
        unsafe { std::slice::from_raw_parts(payload, payload_size) }
    };
    *frame
        .state
        .resume_payload
        .lock()
        .expect("handler resume payload mutex") = bytes.to_vec();
    true
}

/// Reserve the nearest matching handler frame for a resumable operation.
/// `0` means there is no active matching frame or it already has a request.
#[no_mangle]
pub(crate) extern "C" fn jk_handler_frame_begin_resumption(operation: u64) -> *const HandlerFrame {
    begin_resumption(operation).unwrap_or(std::ptr::null())
}

/// Publish a flattened ABI result for the reserved one-shot resumption.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_resume(
    frame: *const HandlerFrame,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    unsafe { resume(frame, operation, payload, payload_size) }.into()
}

/// Reserve a distinct continuation token for a resumable request. The token
/// owns the eventual payload and can coexist with other requests on the same
/// lexical frame.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_begin_resumption_handle(
    operation: u64,
) -> *mut ResumptionHandle {
    let Some(frame) = nearest_operation(operation) else {
        return std::ptr::null_mut();
    };
    let state = unsafe { Arc::clone(&(*frame).state) };
    let payload = state
        .resumable_payloads
        .lock()
        .expect("handler payload mutex")
        .get(&operation)
        .map(ResumptionPayload::flattened)
        .unwrap_or_default();
    Box::into_raw(Box::new(ResumptionHandle {
        state: Arc::clone(&state),
        operation,
        continuation: AtomicU64::new(0),
        resumed: AtomicBool::new(false),
        dispatched: AtomicBool::new(false),
        request_payload: Mutex::new(Vec::new()),
        request_managed: Mutex::new(Vec::new()),
        request_shared: Mutex::new(Vec::new()),
        payload: Mutex::new(payload),
        payload_managed: Mutex::new(Vec::new()),
    }))
}

/// Register a synchronous scalar payload for a resumable operation. This is
/// the first runtime handler implementation used by generated code; richer
/// payload layouts will use the same byte-oriented ABI.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_payload(
    frame: *mut HandlerFrame,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return 0;
    };
    if !frame.state.operations.contains(&operation) || (payload_size != 0 && payload.is_null()) {
        return 0;
    }
    let bytes = if payload_size == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(payload, payload_size) }.to_vec()
    };
    let previous = frame
        .state
        .resumable_payloads
        .lock()
        .expect("handler payload mutex")
        .insert(operation, ResumptionPayload::Raw(bytes));
    if let Some(previous) = previous {
        previous.release();
    }
    1
}

/// Register a managed String payload for a synchronous resumable operation.
/// The frame takes ownership of the supplied String reference and releases it
/// when the frame state is no longer reachable by any continuation handle.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_string(
    frame: *mut HandlerFrame,
    operation: u64,
    pointer: *mut u8,
    length: usize,
) -> u8 {
    let valid_string = (unsafe { valid_header(pointer) })
        .filter(|header| {
            header.kind == RuntimeValueKind::String as u8 && header.payload_size == length
        })
        .is_some();
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        if valid_string {
            jk_drop(pointer);
        }
        return 0;
    };
    if !valid_string || !frame.state.operations.contains(&operation) {
        if valid_string {
            jk_drop(pointer);
        }
        return 0;
    }
    let previous = frame
        .state
        .resumable_payloads
        .lock()
        .expect("handler payload mutex")
        .insert(
            operation,
            ResumptionPayload::String {
                pointer: pointer as usize as u64,
                length: length as u64,
            },
        );
    if let Some(previous) = previous {
        previous.release();
    }
    1
}

/// Register a flattened aggregate payload and transfer ownership of the
/// managed pointer words identified by `managed_offsets` to the handler frame.
/// The frame releases those references when its state is no longer reachable
/// by a continuation handle.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_set_resumption_managed_payload(
    frame: *mut HandlerFrame,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
    managed_offsets: *const usize,
    managed_count: usize,
) -> u8 {
    // A null offset list cannot describe ownership. Keep the caller's
    // payload untouched on this rejected transfer so it can release it.
    if managed_count != 0 && managed_offsets.is_null() {
        return 0;
    }
    let bytes = if payload_size == 0 {
        Vec::new()
    } else if payload.is_null() {
        return 0;
    } else {
        // SAFETY: generated code supplies a valid flattened payload region.
        unsafe { std::slice::from_raw_parts(payload, payload_size) }.to_vec()
    };
    let offsets = if managed_count == 0 {
        Vec::new()
    } else {
        // SAFETY: generated code supplies exactly `managed_count` offsets.
        unsafe { std::slice::from_raw_parts(managed_offsets, managed_count) }.to_vec()
    };
    if offsets.iter().any(|offset| {
        *offset % std::mem::size_of::<u64>() != 0
            || *offset > payload_size
            || payload_size.saturating_sub(*offset) < std::mem::size_of::<u64>()
    }) {
        let payload = ResumptionPayload::Managed {
            bytes,
            managed_offsets: offsets,
        };
        payload.release();
        return 0;
    }
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        let payload = ResumptionPayload::Managed {
            bytes,
            managed_offsets: offsets,
        };
        payload.release();
        return 0;
    };
    if !frame.state.operations.contains(&operation) {
        let payload = ResumptionPayload::Managed {
            bytes,
            managed_offsets: offsets,
        };
        payload.release();
        return 0;
    }
    let previous = frame
        .state
        .resumable_payloads
        .lock()
        .expect("handler payload mutex")
        .insert(
            operation,
            ResumptionPayload::Managed {
                bytes,
                managed_offsets: offsets,
            },
        );
    if let Some(previous) = previous {
        previous.release();
    }
    1
}

pub(crate) unsafe extern "C" fn jk_handler_frame_resumption_handle_operation(
    handle: *const ResumptionHandle,
) -> u64 {
    unsafe { handle.as_ref() }.map_or(0, |handle| handle.operation)
}

/// Canonical thunk that forwards one request parameter. The context packs a
/// byte offset in the low 32 bits and a byte length in the high 32 bits.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_forward_thunk(call: *mut HandlerCall) -> u8 {
    let Some(call) = (unsafe { call.as_mut() }) else {
        return 0;
    };
    if call.captures_size < 8 || call.captures.is_null() {
        return 0;
    }
    let packed = unsafe { std::ptr::read_unaligned(call.captures.cast::<u64>()) };
    let offset = (packed & u64::from(u32::MAX)) as usize;
    let length = (packed >> 32) as usize;
    if offset > call.request_size
        || call.request_size - offset < length
        || length > call.result_capacity
        || (length != 0 && (call.request.is_null() || call.result.is_null()))
    {
        return 0;
    }
    if length != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(call.request.add(offset), call.result, length);
        }
    }
    call.result_size = length;
    1
}

/// Canonical thunk that applies a small integer transformation to one request
/// word. The context packs byte offset (low 16 bits), operator (next 8) and a
/// signed 32-bit immediate (high 32 bits).
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_transform_i64_thunk(call: *mut HandlerCall) -> u8 {
    let Some(call) = (unsafe { call.as_mut() }) else {
        return 0;
    };
    if call.captures_size < 8 || call.captures.is_null() || call.result_capacity < 8 {
        return 0;
    }
    let packed = unsafe { std::ptr::read_unaligned(call.captures.cast::<u64>()) };
    let offset = (packed & 0xffff) as usize;
    let operator = ((packed >> 16) & 0xff) as u8;
    let immediate = (packed >> 32) as u32 as i32 as i64;
    if offset > call.request_size
        || call.request_size - offset < 8
        || call.request.is_null()
        || call.result.is_null()
    {
        return 0;
    }
    let value = unsafe {
        i64::from_ne_bytes(
            std::slice::from_raw_parts(call.request.add(offset), 8)
                .try_into()
                .expect("8-byte word"),
        )
    };
    let result = match operator {
        0 => value.wrapping_add(immediate),
        1 => value.wrapping_sub(immediate),
        2 => value.wrapping_mul(immediate),
        _ => return 0,
    };
    unsafe { std::ptr::copy_nonoverlapping(result.to_ne_bytes().as_ptr(), call.result, 8) };
    call.result_size = 8;
    1
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_request_payload_copy(
    handle: *const ResumptionHandle,
    destination: *mut u8,
    destination_size: usize,
) -> usize {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    if destination_size != 0 && destination.is_null() {
        return 0;
    }
    let payload = handle
        .request_payload
        .lock()
        .expect("handler request payload mutex");
    let length = payload.len().min(destination_size);
    if length != 0 {
        unsafe { std::ptr::copy_nonoverlapping(payload.as_ptr(), destination, length) };
    }
    length
}

/// Return the exact flattened request payload size available to a thunk.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_request_payload_size(
    handle: *const ResumptionHandle,
) -> usize {
    unsafe { handle.as_ref() }
        .and_then(|handle| {
            handle
                .request_payload
                .lock()
                .ok()
                .map(|payload| payload.len())
        })
        .unwrap_or(0)
}

/// Complete one continuation token exactly once and retain its ABI payload.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_resume_handle(
    handle: *mut ResumptionHandle,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
) -> u8 {
    unsafe { resume_handle_managed(handle, operation, payload, payload_size, &[]) }
}

pub(crate) unsafe fn resume_handle_managed(
    handle: *mut ResumptionHandle,
    operation: u64,
    payload: *const u8,
    payload_size: usize,
    managed_slots: &[(usize, usize)],
) -> u8 {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    // The lexical frame may already have exited after transferring this
    // token to a scheduler. Its Arc-pinned state remains valid and the
    // outstanding request is still allowed to resume exactly once.
    if handle.operation != operation {
        return 0;
    }
    let bytes = if payload_size == 0 {
        &[]
    } else if payload.is_null() {
        return 0;
    } else {
        // SAFETY: caller owns the flattened ABI payload for this call.
        unsafe { std::slice::from_raw_parts(payload, payload_size) }
    };
    if handle
        .resumed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return 0;
    }
    *handle
        .payload
        .lock()
        .expect("handler resumption payload mutex") = bytes.to_vec();
    *handle
        .payload_managed
        .lock()
        .expect("handler payload ownership mutex") = managed_slots.to_vec();
    if unsafe { dispatch_bound_resumption(handle) } == 0
        && handle.continuation.load(Ordering::Acquire) != 0
    {
        let managed = handle
            .payload_managed
            .lock()
            .expect("handler payload ownership mutex");
        let payload = handle.payload.lock().expect("handler payload mutex");
        release_managed_bytes(&payload, &managed);
        drop(payload);
        drop(managed);
        handle
            .payload_managed
            .lock()
            .expect("handler payload ownership mutex")
            .clear();
        return 0;
    }
    1
}

unsafe fn dispatch_bound_resumption(handle: &ResumptionHandle) -> u8 {
    let continuation = handle.continuation.load(Ordering::Acquire);
    if continuation == 0 || !handle.resumed.load(Ordering::Acquire) {
        return 1;
    }
    if handle
        .dispatched
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return 1;
    }
    let continuation = continuation as *mut crate::runtime::continuation::Continuation;
    let generation =
        unsafe { crate::runtime::continuation::jk_continuation_generation(continuation) };
    // SAFETY: the owning task retains the continuation through dispatch.
    unsafe { crate::runtime::continuation::jk_continuation_dispatch(continuation, generation) }
}

/// Attach a reserved request to its scheduler-owned continuation.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_bind_resumption_handle(
    handle: *mut ResumptionHandle,
    continuation: *mut crate::runtime::continuation::Continuation,
) -> u8 {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    if continuation.is_null() {
        return 0;
    }
    if handle
        .continuation
        .compare_exchange(
            0,
            continuation as usize as u64,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        unsafe { dispatch_bound_resumption(handle) }
    } else {
        0
    }
}

/// Copy a completed continuation payload into caller-owned ABI storage.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_resumption_payload_copy(
    handle: *const ResumptionHandle,
    destination: *mut u8,
    destination_size: usize,
) -> usize {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    if destination_size != 0 && destination.is_null() {
        return 0;
    }
    let payload = handle
        .payload
        .lock()
        .expect("handler resumption payload mutex");
    let length = payload.len().min(destination_size);
    if length != 0 {
        // SAFETY: caller supplied a valid destination region of this size.
        unsafe { std::ptr::copy_nonoverlapping(payload.as_ptr(), destination, length) };
    }
    length
}

/// Return the exact flattened response payload size currently held by a
/// request handle. Thunks can use this before allocating a response buffer.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_resumption_payload_size(
    handle: *const ResumptionHandle,
) -> usize {
    unsafe { handle.as_ref() }
        .and_then(|handle| handle.payload.lock().ok().map(|payload| payload.len()))
        .unwrap_or(0)
}

/// Transfer ownership of the registered payload to the resumed value. This
/// is called after the ABI bytes have been copied into its result storage.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_resumption_payload_consume(
    handle: *const ResumptionHandle,
) -> u8 {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return 0;
    };
    let payload = handle
        .state
        .resumable_payloads
        .lock()
        .expect("handler payload mutex")
        .remove(&handle.operation)
        .map(|payload| payload.take_flattened());
    if payload.is_some() {
        return 1;
    }
    let mut managed = handle
        .payload_managed
        .lock()
        .expect("handler payload ownership mutex");
    if managed.is_empty() {
        return 0;
    }
    // The generated caller has reconstructed the value from the copied
    // bytes, so ownership now belongs to that value rather than the token.
    managed.clear();
    1
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_free_resumption_handle(
    handle: *mut ResumptionHandle,
) {
    if !handle.is_null() {
        // SAFETY: the creator owns this token and frees it after dispatch.
        unsafe { drop(Box::from_raw(handle)) };
    }
}
