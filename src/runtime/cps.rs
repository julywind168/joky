//! Shared ABI for generated CPS code and native, nonblocking host callbacks.
//!
//! Entries receive one frame pointer and return 0 (complete), 1 (pending), or
//! another value (failure). A pending callback is polled again with the same
//! frame after the caller signals readiness; it must preserve its own progress.

pub const DONE: u32 = u32::MAX;
pub const INVALID_STATE: i32 = 2;

#[repr(C)]
pub struct CpsFrame {
    pub state: u32,
    pub result: u64,
    /// Parameters first, followed by compiler-generated local/temporary slots.
    pub slots: Vec<u64>,
    /// Native callbacks may keep additional state here across polls.
    pub host_data: Vec<u8>,
    children: Vec<Option<Box<CpsFrame>>>,
}

impl CpsFrame {
    pub fn new(slot_count: usize) -> Self {
        Self {
            state: 0,
            result: 0,
            slots: vec![0; slot_count],
            host_data: Vec::new(),
            children: Vec::new(),
        }
    }
}

pub type CpsEntry = unsafe extern "C" fn(*mut CpsFrame) -> i32;

// These helpers are private compiler/runtime ABI. Generated code supplies a
// live, exclusively borrowed frame and never retains pointers after a poll.
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
    let children = &mut (*frame).children;
    if children.len() <= index {
        children.resize_with(index + 1, || None);
    }
    let child = children[index]
        .get_or_insert_with(|| Box::new(CpsFrame::new(slot_count)))
        .as_mut();
    child
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_release_children"]
pub unsafe extern "C" fn release_children(frame: *mut CpsFrame) {
    (*frame).children.clear();
}

pub fn symbols() -> [(&'static str, *const u8); 5] {
    [
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
