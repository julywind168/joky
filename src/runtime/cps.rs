//! Shared ABI for generated CPS code and native, nonblocking host callbacks.
//!
//! Entries receive one frame pointer and return 0 (complete), 1 (pending), or
//! another value (failure). A pending callback is polled again with the same
//! frame after the caller signals readiness; it must preserve its own progress.

use super::arena::Arena;
use super::borrowed_host::{BorrowingHostStop, STOP_CANCELLED};
use std::{cell::UnsafeCell, rc::Rc};

pub const DONE: u32 = u32::MAX;
pub const INVALID_STATE: i32 = 2;
pub const RUNTIME_ERROR: i32 = 3;
pub const FAILED: u32 = u32::MAX - 1;

#[repr(C)]
pub struct CpsFrame {
    pub state: u32,
    pub result: u64,
    /// Parameters first, followed by compiler-generated local/temporary slots.
    pub slots: Vec<u64>,
    /// Native callbacks may keep additional state here across polls.
    pub host_data: Vec<u8>,
    /// Optional operation-owned context, disposed by the borrowing host stop hook.
    pub host_context: *mut std::ffi::c_void,
    pub(crate) host_stop: Option<BorrowingHostStop>,
    children: Vec<Option<Box<CpsFrame>>>,
    // Lazily created by allocation-capable generated entries, then inherited by
    // every child. All polls in a task are sequential; generated allocation
    // helpers borrow the UnsafeCell only for the duration of one allocation.
    arena: Option<Rc<UnsafeCell<Arena>>>,
}

impl CpsFrame {
    pub fn new(slot_count: usize) -> Self {
        Self {
            state: 0,
            result: 0,
            slots: vec![0; slot_count],
            host_data: Vec::new(),
            host_context: std::ptr::null_mut(),
            host_stop: None,
            children: Vec::new(),
            arena: None,
        }
    }

    pub(crate) fn release(&mut self) {
        self.stop_host(STOP_CANCELLED);
        if self.state != DONE {
            self.state = FAILED;
        }
        self.children.clear();
        self.arena = None;
    }

    pub(crate) fn stop_host(&mut self, reason: u32) {
        if let Some(stop) = self.host_stop.take() {
            // Stop while slots, context, the arena and callback owner are live.
            // A cleanup callback cannot overwrite an enclosing runtime error.
            let _ = super::error::checked(|| unsafe { stop(self, reason) });
            self.host_context = std::ptr::null_mut();
            self.host_data.clear();
        }
    }
}

impl Drop for CpsFrame {
    fn drop(&mut self) {
        self.release();
    }
}

pub type CpsEntry = unsafe extern "C" fn(*mut CpsFrame) -> i32;

// Exported C ABI helpers for generated code; not part of the Rust public API.
// Generated code supplies a live, exclusively borrowed frame and never retains
// pointers after a poll.
/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_slots"]
pub unsafe extern "C" fn slots(frame: *mut CpsFrame) -> *mut u64 {
    (*frame).slots.as_mut_ptr()
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_child"]
pub unsafe extern "C" fn child(frame: *mut CpsFrame, slot_count: usize) -> *mut CpsFrame {
    child_at(frame, 0, slot_count)
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_release_child"]
pub unsafe extern "C" fn release_child(frame: *mut CpsFrame) {
    if !(*frame).children.is_empty() {
        (&mut (*frame).children)[0] = None;
    }
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame. `index` is an
/// internal compiler slot and is created lazily with the requested frame size.
#[export_name = "__joky_cps_child_at"]
pub unsafe extern "C" fn child_at(
    frame: *mut CpsFrame,
    index: usize,
    slot_count: usize,
) -> *mut CpsFrame {
    let frame = &mut *frame;
    let children = &mut frame.children;
    if children.len() <= index {
        children.resize_with(index + 1, || None);
    }
    let child = children[index]
        .get_or_insert_with(|| {
            let mut child = CpsFrame::new(slot_count);
            child.arena = frame.arena.clone();
            Box::new(child)
        })
        .as_mut();
    child
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_release_children"]
pub unsafe extern "C" fn release_children(frame: *mut CpsFrame) {
    (*frame).children.clear();
}

/// # Safety
/// The frame must be exclusively borrowed during this poll. Generated callers
/// initialize an allocation-capable task's arena before creating any children.
/// The returned pointer is private to checked code and cannot outlive the task.
#[export_name = "__joky_cps_arena"]
unsafe extern "C" fn arena(frame: *mut CpsFrame) -> *mut Arena {
    (*frame)
        .arena
        .get_or_insert_with(|| Rc::new(UnsafeCell::new(Arena::default())))
        .get()
}

/// # Safety
/// The frame must be exclusively borrowed and terminal. Result slots remain
/// readable, but generated code must not access its arena after this call.
#[export_name = "__joky_cps_finish"]
pub(crate) unsafe extern "C" fn finish(frame: *mut CpsFrame) {
    (*frame).release();
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 8] {
    [
        (
            "joky_cps_poll_borrowing_v1",
            super::borrowed_host::joky_cps_poll_borrowing_v1 as *const u8,
        ),
        ("__joky_cps_arena", arena as *const u8),
        ("__joky_cps_finish", finish as *const u8),
        ("__joky_cps_slots", slots as *const u8),
        ("__joky_cps_child", child as *const u8),
        ("__joky_cps_release_child", release_child as *const u8),
        ("__joky_cps_child_at", child_at as *const u8),
        ("__joky_cps_release_children", release_children as *const u8),
    ]
}

/// Allocate a runtime frame. Allocation failure aborts the process.
#[no_mangle]
pub extern "C" fn joky_cps_frame_new(slot_count: usize) -> *mut CpsFrame {
    Box::into_raw(Box::new(CpsFrame::new(slot_count)))
}

/// Free a frame and all its children. A null pointer is accepted.
/// # Safety
/// The pointer must come from joky_cps_frame_new and must not have been freed.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_frame_free(frame: *mut CpsFrame) {
    if !frame.is_null() {
        drop(Box::from_raw(frame));
    }
}

/// # Safety
/// `frame` must point to a live runtime frame.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_state(frame: *const CpsFrame) -> u32 {
    (*frame).state
}

/// # Safety
/// `frame` must point to a live, exclusively borrowed runtime frame.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_set_state(frame: *mut CpsFrame, state: u32) {
    (*frame).state = state;
}

/// # Safety
/// `frame` must point to a live runtime frame.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_result(frame: *const CpsFrame) -> u64 {
    (*frame).result
}

/// # Safety
/// `frame` must point to a live, exclusively borrowed runtime frame.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_set_result(frame: *mut CpsFrame, result: u64) {
    (*frame).result = result;
}

/// # Safety
/// `frame` must point to a live runtime frame. The context belongs to its host.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_host_context(frame: *const CpsFrame) -> *mut std::ffi::c_void {
    (*frame).host_context
}

/// # Safety
/// `frame` must be live and exclusively borrowed. The host stop hook must dispose
/// of the context; the runtime never dereferences or frees it independently.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_set_host_context(
    frame: *mut CpsFrame,
    context: *mut std::ffi::c_void,
) {
    (*frame).host_context = context;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        arena::LIVE,
        suspension::{Continuation, ModuleId},
    };

    #[test]
    fn malformed_terminal_status_releases_allocations_before_continuation_drop() {
        unsafe extern "C" fn malformed(frame: *mut CpsFrame) -> i32 {
            (*arena(frame)).allocate(4, 4).unwrap();
            (&(*frame).slots)[0] as i32
        }
        for status in [0, 7] {
            let mut frame = CpsFrame::new(1);
            frame.slots[0] = status;
            let mut task = unsafe { Continuation::new(malformed, frame, ModuleId(0), Rc::new(())) };
            assert!(task.resume().is_err());
            assert!(task.is_finished());
            assert_eq!(task.cps_result(), None);
            LIVE.with(|live| assert_eq!(live.get(), 0));
            drop(task);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
