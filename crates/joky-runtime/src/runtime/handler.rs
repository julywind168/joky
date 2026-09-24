//! Runtime metadata for lexical `do ... with` handler frames.
//!
//! Typed MIR remains responsible for payload binding and its CFG join.  This
//! layer owns the dynamic parent chain so tasks inherit their nearest handler
//! across ordinary calls and suspension.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::managed::{jk_drop, valid_header, RuntimeValueKind};
pub(crate) use joky_runtime_abi::HandlerEnvOwnership;
#[cfg(test)]
use joky_runtime_abi::{
    HANDLER_CALL_CAPTURES_OFFSET, HANDLER_CALL_REQUEST_OFFSET, HANDLER_CALL_RESULT_OFFSET,
    HANDLER_ENV_SLOT_SIZE,
};

mod payload;
mod resumption;
mod thunks;

use payload::{
    release_managed_bytes, HandlerEnv, HandlerThunkContext, OwnedHandlerEnvSlot, ResumptionPayload,
};
pub(crate) use resumption::*;
pub(crate) use thunks::*;

const NO_PENDING_OPERATION: u64 = u64::MAX;

#[cfg(test)]
pub(crate) fn handler_abi_descriptor() -> crate::runtime::abi::HandlerAbiDescriptor {
    crate::runtime::abi::HandlerAbiDescriptor {
        version: 1,
        call_size: std::mem::size_of::<HandlerCall>() as u32,
        env_slot_size: HANDLER_ENV_SLOT_SIZE as u32,
        request_offset: HANDLER_CALL_REQUEST_OFFSET,
        captures_offset: HANDLER_CALL_CAPTURES_OFFSET,
        result_offset: HANDLER_CALL_RESULT_OFFSET,
    }
}

thread_local! {
    static CURRENT_HANDLER: Cell<*const HandlerFrame> = const { Cell::new(std::ptr::null()) };
}

struct HandlerFrameState {
    _resource: crate::runtime::resources::Lease,
    operations: Box<[u64]>,
    /// Canonical handler thunk context pointers, keyed by operation.
    resumable_thunks: Mutex<HashMap<u64, usize>>,
    resumable_owned_thunk_contexts: Mutex<HashMap<u64, Box<HandlerThunkContext>>>,
    resumable_payloads: Mutex<HashMap<u64, ResumptionPayload>>,
    active: AtomicBool,
    /// A one-shot resumption is owned by its lexical frame, not the worker
    /// that raised it. MIR will later attach a typed continuation handle.
    pending_operation: AtomicU64,
    resumed: AtomicBool,
    resume_payload: Mutex<Vec<u8>>,
}

/// Canonical ABI for generated resumable handler bodies. The thunk decodes
/// request and capture bytes according to its generated layout, writes the
/// flattened result, and sets `result_size` before returning success.
#[repr(C)]
pub(crate) struct HandlerCall {
    pub(crate) operation: u64,
    pub(crate) request: *const u8,
    pub(crate) request_size: usize,
    pub(crate) captures: *const u8,
    pub(crate) captures_size: usize,
    pub(crate) result: *mut u8,
    pub(crate) result_capacity: usize,
    pub(crate) result_size: usize,
}

/// C-compatible descriptor used when a generated thunk installs its capture
/// environment. Offsets point at pointer-sized words in `captures`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct HandlerEnvSlot {
    pub(crate) offset: usize,
    pub(crate) ownership: u8,
    pub(crate) drop_callback: *mut std::ffi::c_void,
}

type HandlerThunk = unsafe extern "C" fn(*mut HandlerCall) -> u8;

/// Canonical thunk for a precomputed, non-managed response payload.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_static_thunk(call: *mut HandlerCall) -> u8 {
    let Some(call) = (unsafe { call.as_mut() }) else {
        return 0;
    };
    if call.captures_size > call.result_capacity
        || (call.captures_size != 0 && (call.captures.is_null() || call.result.is_null()))
    {
        return 0;
    }
    if call.captures_size != 0 {
        // SAFETY: the runtime-owned capture and result buffers do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(call.captures, call.result, call.captures_size);
        }
    }
    call.result_size = call.captures_size;
    1
}

impl Drop for HandlerFrameState {
    fn drop(&mut self) {
        let payloads = match self.resumable_payloads.get_mut() {
            Ok(payloads) => payloads,
            Err(error) => error.into_inner(),
        };
        for payload in payloads.drain().map(|(_, payload)| payload) {
            payload.release();
        }
    }
}

pub(crate) struct HandlerFrame {
    parent: *const HandlerFrame,
    /// Request tokens retain this state after the lexical wrapper is freed.
    /// This prevents an in-flight continuation from dereferencing a stale
    /// `HandlerFrame` pointer during cancellation or late resume.
    state: Arc<HandlerFrameState>,
}

/// A single resumable request reservation. Unlike the legacy frame-level
/// reservation API, each handle has its own one-shot lifecycle, allowing a
/// handler to service multiple sequential requests (including recursive
/// calls) without sharing a consumed flag.
pub(crate) struct ResumptionHandle {
    state: Arc<HandlerFrameState>,
    operation: u64,
    continuation: AtomicU64,
    resumed: AtomicBool,
    dispatched: AtomicBool,
    request_payload: Mutex<Vec<u8>>,
    /// Managed pointer words transferred from the effect call site. These are
    /// released if no thunk accepts the request; a successful thunk consumes
    /// the descriptors as ownership moves into its typed parameters.
    request_managed: Mutex<Vec<(usize, usize)>>,
    /// Duplicated shared pointer words sent to a borrowed handler parameter.
    /// The request owns these duplicate references until the thunk returns.
    request_shared: Mutex<Vec<(usize, usize)>>,
    payload: Mutex<Vec<u8>>,
    /// Managed pointer words returned by a generated handler thunk. The
    /// offsets stay attached to the token so cancellation can release a
    /// result that was never reconstructed by the resumed MIR.
    payload_managed: Mutex<Vec<(usize, usize)>>,
}

unsafe impl Send for ResumptionHandle {}
unsafe impl Sync for ResumptionHandle {}

impl Drop for ResumptionHandle {
    fn drop(&mut self) {
        let request_managed = self
            .request_managed
            .get_mut()
            .expect("handler request ownership mutex");
        if !request_managed.is_empty() {
            let request = self
                .request_payload
                .get_mut()
                .expect("handler request payload mutex");
            release_managed_bytes(request, request_managed);
            request_managed.clear();
        }
        let request_shared = self
            .request_shared
            .get_mut()
            .expect("handler shared request ownership mutex");
        if !request_shared.is_empty() {
            let request = self
                .request_payload
                .get_mut()
                .expect("handler request payload mutex");
            release_managed_bytes(request, request_shared);
            request_shared.clear();
        }
        let managed = self
            .payload_managed
            .get_mut()
            .expect("handler payload ownership mutex");
        if managed.is_empty() {
            return;
        }
        let payload = self.payload.get_mut().expect("handler payload mutex");
        release_managed_bytes(payload, managed);
        managed.clear();
    }
}

unsafe impl Send for HandlerFrame {}
unsafe impl Sync for HandlerFrame {}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_new(
    operations: *const u64,
    count: usize,
) -> *mut HandlerFrame {
    let operations = if count == 0 {
        Vec::new()
    } else if operations.is_null() {
        return std::ptr::null_mut();
    } else {
        // SAFETY: generated code supplies `count` initialized operation IDs.
        unsafe { std::slice::from_raw_parts(operations, count) }.to_vec()
    };
    Box::into_raw(Box::new(HandlerFrame {
        parent: current(),
        state: Arc::new(HandlerFrameState {
            _resource: crate::runtime::resources::Lease::new(
                &crate::runtime::scope::current_or_default(),
                crate::runtime::resources::Kind::HandlerFrames,
                1,
            ),
            operations: operations.into_boxed_slice(),
            active: AtomicBool::new(false),
            resumable_thunks: Mutex::new(HashMap::new()),
            resumable_owned_thunk_contexts: Mutex::new(HashMap::new()),
            resumable_payloads: Mutex::new(HashMap::new()),
            pending_operation: AtomicU64::new(NO_PENDING_OPERATION),
            resumed: AtomicBool::new(false),
            resume_payload: Mutex::new(Vec::new()),
        }),
    }))
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_enter(frame: *mut HandlerFrame) -> u8 {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return 0;
    };
    frame.state.active.store(true, Ordering::Release);
    unsafe { install(frame) };
    1
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_exit(frame: *mut HandlerFrame) -> u8 {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return 0;
    };
    if !frame.state.active.swap(false, Ordering::AcqRel) {
        return 0;
    }
    CURRENT_HANDLER.with(|current| {
        if std::ptr::eq(current.get(), frame) {
            current.set(frame.parent);
        }
    });
    1
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_handler_frame_free(frame: *mut HandlerFrame) {
    if !frame.is_null() {
        // SAFETY: codegen pairs this with HandlerExit after joining children.
        unsafe { drop(Box::from_raw(frame)) };
    }
}

#[cfg(test)]
mod tests;
