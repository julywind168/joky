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

/// One dynamic task scope on a frame: registered child frames in creation
/// order (`None` once finished) plus the polling cursor, which is preserved
/// across host returns so instruction slicing cannot reorder polling. A
/// joining scope is driven by its join site; the scheduler round skips it.
#[repr(C)]
pub struct DynamicTaskScope {
    pub children: Vec<Option<Box<CpsFrame>>>,
    pub cursor: usize,
    pub joining: bool,
}

impl DynamicTaskScope {
    fn compact(&mut self) {
        self.cursor = self.children[..self.cursor].iter().flatten().count();
        self.children.retain(Option::is_some);
        if self.children.capacity() > self.children.len().saturating_mul(4).max(16) {
            self.children.shrink_to(self.children.len().max(8));
        }
    }
}

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
    pub(crate) tcp_request: Option<Box<super::tcp::Request>>,
    pub(crate) byte_request: Option<Box<super::byte_request::ByteRequest>>,
    pub(crate) host_stop: Option<BorrowingHostStop>,
    pub(crate) wake: Option<super::driver::Wake>,
    children: Vec<Option<Box<CpsFrame>>>,
    /// Open dynamic task scopes; joins pop from the top.
    pub(crate) task_scopes: Vec<DynamicTaskScope>,
    /// Entry of a dynamically registered child; static children are polled by
    /// generated code that already knows the callee.
    pub(crate) task_entry: Option<CpsEntry>,
    // Lazily created by allocation-capable generated entries, then inherited by
    // every child. All polls in a task are sequential; generated allocation
    // helpers borrow the UnsafeCell only for the duration of one allocation.
    arena: Option<Rc<UnsafeCell<Arena>>>,
    pub(crate) cleanup: *mut super::cleanup::NativeFrame,
    pub(crate) text_views: Vec<super::string::JokyText>,
    pub(crate) domain_return: Option<(crate::Type, usize)>,
    pub(crate) domain_result: Option<Vec<u64>>,
    pub(crate) root_text: bool,
    pub(crate) text_result: Option<String>,
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
            byte_request: None,
            tcp_request: None,
            wake: None,
            children: Vec::new(),
            task_scopes: Vec::new(),
            task_entry: None,
            arena: None,
            cleanup: std::ptr::null_mut(),
            text_views: Vec::new(),
            domain_return: None,
            domain_result: None,
            root_text: false,
            text_result: None,
        }
    }

    pub(crate) fn release(&mut self) {
        self.stop_host(STOP_CANCELLED);
        if self.state != DONE {
            self.state = FAILED;
        }
        self.children.clear();
        self.task_scopes.clear();
        self.task_entry = None;
        self.text_views.clear();
        if !self.cleanup.is_null() {
            let cleanup = std::mem::replace(&mut self.cleanup, std::ptr::null_mut());
            unsafe {
                super::cleanup::finish(cleanup);
            }
        }
        if self.state != DONE || super::error::joky_runtime_error_code() != 0 {
            self.text_result = None;
            self.domain_result = None;
        }
        self.wake = None;
        self.arena = None;
    }

    pub(crate) fn wait_plan(&self) -> super::driver::WaitPlan {
        let mut plan = super::driver::WaitPlan::default();
        let mut frames = vec![self];
        while let Some(frame) = frames.pop() {
            if matches!(frame.state, DONE | FAILED) {
                continue;
            }
            plan.add(frame.wake);
            frames.extend(frame.children.iter().filter_map(Option::as_deref));
            for scope in &frame.task_scopes {
                frames.extend(scope.children.iter().filter_map(Option::as_deref));
            }
        }
        plan
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
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_scope_enter"]
pub unsafe extern "C" fn task_scope_enter(frame: *mut CpsFrame) {
    (*frame).task_scopes.push(DynamicTaskScope {
        children: Vec::new(),
        cursor: 0,
        joining: false,
    });
}

/// Upper bound on dynamically registered children of one frame. Registration
/// beyond this returns a controlled runtime error instead of growing without
/// bound. Completed frames are freed immediately and their registrations are
/// compacted between rounds, so history does not consume the live quota.
const MAX_DYNAMIC_TASK_CHILDREN: usize = 65536;

/// Create (but do not yet publish) one dynamic child frame. Generated code
/// copies captured inputs into the returned frame and then publishes it with
/// `task_branch_commit`; no poll may happen in between. Returns null and
/// records a controlled runtime error when the frame's live child quota is
/// exhausted.
/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_child"]
pub unsafe extern "C" fn task_child(frame: *mut CpsFrame, slot_count: usize) -> *mut CpsFrame {
    let frame = &mut *frame;
    let live: usize = frame
        .task_scopes
        .iter()
        .map(|scope| scope.children.iter().flatten().count())
        .sum();
    if live >= MAX_DYNAMIC_TASK_CHILDREN {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::ResourceLimit,
            "Dynamic task quota exceeded",
        ));
        return std::ptr::null_mut();
    }
    let mut child = CpsFrame::new(slot_count);
    child.arena = frame.arena.clone();
    Box::into_raw(Box::new(child))
}

/// Publish a prepared dynamic child under the innermost open scope. The
/// cleanup responsibilities for owning captures were transferred before this
/// call, so the child is fully accountable once it is visible here.
/// # Safety
/// `child` must come from `task_child` for this task and not be committed yet.
#[export_name = "__joky_cps_task_branch_commit"]
pub unsafe extern "C" fn task_branch_commit(
    frame: *mut CpsFrame,
    child: *mut CpsFrame,
    entry: CpsEntry,
) {
    let frame = &mut *frame;
    let mut child = Box::from_raw(child);
    child.task_entry = Some(entry);
    let scope = frame
        .task_scopes
        .last_mut()
        .expect("branch requires an open task scope");
    scope.children.push(Some(child));
}

/// Advance the innermost open scope by one polling round. Returns 0 once the
/// scope is complete and closed, 1 when some child reported Pending, and the
/// failing child's status otherwise (the scope is cancelled first).
/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_join"]
pub unsafe extern "C" fn task_join(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    let Some(scope) = frame.task_scopes.last_mut() else {
        return INVALID_STATE;
    };
    scope.joining = true;
    loop {
        while scope.cursor < scope.children.len() && scope.children[scope.cursor].is_none() {
            scope.cursor += 1;
        }
        if scope.cursor >= scope.children.len() {
            if scope.children.iter().all(Option::is_none) {
                frame.task_scopes.pop();
                return 0;
            }
            // Every remaining child was polled this round and reported Pending;
            // the next round starts over from the registration order.
            scope.compact();
            scope.cursor = 0;
            return 1;
        }
        let mut child = scope.children[scope.cursor].take().expect("live child");
        let entry = child.task_entry.expect("registered child entry");
        let status = entry(&mut *child);
        match status {
            0 => {
                // Completed children return their frame immediately; keep
                // polling the rest of the round.
                drop(child);
                scope.cursor += 1;
            }
            1 => {
                // Pending children keep their frame and are retried next round.
                scope.children[scope.cursor] = Some(child);
                scope.cursor += 1;
            }
            failure => {
                // Cancel the whole scope: pending siblings and the failed
                // child drop here, running their stop hooks and cleanup.
                drop(child);
                frame.task_scopes.pop();
                return failure;
            }
        }
    }
}

/// # Safety
/// The frame must be exclusively borrowed during this poll. Generated callers
/// initialize an allocation-capable task's arena before creating any children.
/// The returned pointer is private to checked code and cannot outlive the task.
#[export_name = "__joky_cps_arena"]
pub(crate) unsafe extern "C" fn arena(frame: *mut CpsFrame) -> *mut Arena {
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

/// Publish an owning return only after local destructors succeed.
/// # Safety
/// The live frame owns the checked return registration in its cleanup table.
#[export_name = "__joky_cps_finish_return_v1"]
pub(crate) unsafe extern "C" fn finish_return(frame: *mut CpsFrame, slot: usize) {
    let cleanup = std::mem::replace(&mut (*frame).cleanup, std::ptr::null_mut());
    super::cleanup::finish_return(cleanup, slot);
    if super::error::joky_runtime_error_code() == 0 {
        if let Some((ty, offset)) = (*frame).domain_return.clone() {
            let raw = if ty.is_aggregate() {
                (&(*frame).slots)[offset..offset + ty.scalar_types().len()].to_vec()
            } else {
                vec![(*frame).result]
            };
            match super::domain_packet::encode_result(&ty, &raw, &mut *arena(frame)) {
                Ok(words) => {
                    (*frame).domain_result = Some(words);
                    (*frame).result = 0;
                }
                Err(error) => super::error::record_error(error),
            }
        }
    }
}

/// # Safety
/// Frame is live and exclusive on the serialized task thread.
#[export_name = "__joky_cps_cleanup_v2"]
pub(crate) unsafe extern "C" fn cleanup(frame: *mut CpsFrame) -> *mut super::cleanup::NativeFrame {
    if (*frame).cleanup.is_null() {
        (*frame).cleanup = super::cleanup::new(arena(frame));
    }
    (*frame).cleanup
}

/// Transfer before polling: an unpolled scope child still owns its inputs on cancellation.
/// # Safety
/// Frames share their task arena; indices name the checked owning cleanup group.
#[export_name = "__joky_cps_transfer_owner_v2"]
pub(crate) unsafe extern "C" fn transfer_owner(
    parent: *mut CpsFrame,
    child: *mut CpsFrame,
    from: usize,
    to: usize,
) {
    let source = cleanup(parent);
    let target = cleanup(child);
    if let Some(entry) = (*source).entries.take(from) {
        (*target).entries.insert(to, entry);
    }
}

/// Capture an opted-in root result before task cleanup. Internal calls keep their task handle.
/// # Safety
/// Frame and owner are live checked compiler values.
#[export_name = "__joky_cps_capture_text_v2"]
pub(crate) unsafe extern "C" fn capture_text(
    frame: *mut CpsFrame,
    owner: *const super::buffer::Buffer,
) {
    if (*frame).root_text && (*frame).domain_return.is_none() {
        match super::owned_text::borrow(owner).and_then(super::owned_text::copy_to_host) {
            Ok(text) => (*frame).text_result = Some(text),
            Err(error) => super::error::record_error(error),
        }
    }
}

/// # Safety
/// Frame contains param_count checked scalar inputs; slot_count covers them.
#[export_name = "__joky_text_cps_prepare_v2"]
pub(crate) unsafe extern "C" fn text_prepare(
    frame: *mut CpsFrame,
    slot_count: usize,
    param_count: usize,
) -> *mut CpsFrame {
    // Initialize the shared arena before creating the child.
    arena(frame);
    let child = child(frame, slot_count);
    (&mut (*child).slots)[..param_count].copy_from_slice(&(&(*frame).slots)[..param_count]);
    (*child).text_views = vec![super::string::JokyText::default(); param_count];
    (*frame).state = 1;
    child
}
/// # Safety
/// Child was prepared above, index names a live String owner in its slots.
#[export_name = "__joky_text_cps_argument_v2"]
pub(crate) unsafe extern "C" fn text_argument(child: *mut CpsFrame, index: usize) {
    let owner = (&(*child).slots)[index] as usize as *const super::buffer::Buffer;
    let view = &mut (&mut (*child).text_views)[index];
    super::owned_text::view(owner, view);
    (&mut (*child).slots)[index] = view.as_ptr() as usize as u64;
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 19] {
    [
        ("__joky_cps_cleanup_v2", cleanup as *const u8),
        ("__joky_cps_transfer_owner_v2", transfer_owner as *const u8),
        ("__joky_cps_capture_text_v2", capture_text as *const u8),
        ("__joky_text_cps_prepare_v2", text_prepare as *const u8),
        ("__joky_text_cps_argument_v2", text_argument as *const u8),
        (
            "joky_cps_poll_borrowing_v1",
            super::borrowed_host::joky_cps_poll_borrowing_v1 as *const u8,
        ),
        ("__joky_cps_arena", arena as *const u8),
        ("__joky_cps_finish", finish as *const u8),
        ("__joky_cps_finish_return_v1", finish_return as *const u8),
        ("__joky_cps_slots", slots as *const u8),
        ("__joky_cps_child", child as *const u8),
        ("__joky_cps_release_child", release_child as *const u8),
        ("__joky_cps_child_at", child_at as *const u8),
        ("__joky_cps_release_children", release_children as *const u8),
        ("__joky_cps_task_scope_enter", task_scope_enter as *const u8),
        ("__joky_cps_task_child", task_child as *const u8),
        (
            "__joky_cps_task_branch_commit",
            task_branch_commit as *const u8,
        ),
        ("__joky_cps_task_join", task_join as *const u8),
        ("__joky_cps_task_round", task_round as *const u8),
    ]
}

unsafe fn round_select(root: *mut CpsFrame) -> Option<(*mut DynamicTaskScope, usize)> {
    enum Visit {
        Frame(*mut CpsFrame),
        Child(*mut DynamicTaskScope, usize),
    }
    let mut pending = vec![Visit::Frame(root)];
    while let Some(visit) = pending.pop() {
        let frame = match visit {
            Visit::Child(scope, index) => return Some((scope, index)),
            Visit::Frame(frame) => &mut *frame,
        };
        // Reverse insertion preserves pre-order: dynamic scopes in source
        // order, then the suspended body's ordinary call/join children.
        pending.extend(
            frame
                .children
                .iter_mut()
                .rev()
                .flatten()
                .map(|child| Visit::Frame(&mut **child)),
        );
        for scope in frame.task_scopes.iter_mut().rev() {
            let scope_ptr = scope as *mut DynamicTaskScope;
            for (index, child) in scope.children.iter_mut().enumerate().rev() {
                if let Some(child) = child {
                    if scope.joining || index < scope.cursor {
                        // The join site already polled this child, but its
                        // suspended body may have registered dynamic descendants.
                        pending.push(Visit::Frame(&mut **child));
                    } else {
                        pending.push(Visit::Child(scope_ptr, index));
                    }
                }
            }
        }
    }
    None
}

unsafe fn round_reset(root: *mut CpsFrame) {
    let mut pending = vec![root];
    while let Some(frame) = pending.pop() {
        let frame = &mut *frame;
        pending.extend(
            frame
                .children
                .iter_mut()
                .flatten()
                .map(|child| &mut **child as *mut _),
        );
        for scope in &mut frame.task_scopes {
            // There is no selected child across this boundary. Remove finished
            // registrations without reordering live children or losing a join's cursor.
            scope.compact();
            if !scope.joining {
                scope.cursor = 0;
            }
            pending.extend(
                scope
                    .children
                    .iter_mut()
                    .flatten()
                    .map(|child| &mut **child as *mut _),
            );
        }
    }
}

/// Advance every registered dynamic child once, in pre-order. Runs when the
/// body is suspended so children make progress before the task surfaces
/// Pending. Returns 0 when the round completes, or the failing child's status.
/// # Safety
/// `root` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_round"]
pub unsafe extern "C" fn task_round(root: *mut CpsFrame) -> i32 {
    loop {
        let selected = round_select(root);
        let Some((scope_ptr, index)) = selected else {
            round_reset(&mut *root);
            return 0;
        };
        let scope = &mut *scope_ptr;
        let Some(child) = scope.children[index].as_deref_mut() else {
            continue;
        };
        let entry = child.task_entry.expect("registered child entry");
        let status = entry(child);
        match status {
            0 => {
                // Completed children return their frame immediately.
                scope.children[index] = None;
                scope.cursor = index + 1;
            }
            1 => {
                scope.cursor = index + 1;
            }
            failure => return failure,
        }
    }
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
    if (*frame).root_text {
        0
    } else {
        (*frame).result
    }
}

/// Copy a text parameter into a fresh, unpolled v2 root frame.
/// # Safety
/// Frame is live and exclusive; index is a String parameter from compiler metadata.
/// The bounded, nonempty input buffer is readable for this call only.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_set_text_input_v2(
    frame: *mut CpsFrame,
    index: usize,
    data: *const u8,
    length: u64,
) -> i32 {
    if frame.is_null() || (*frame).state != 0 || index >= (*frame).slots.len() {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Abi,
            crate::ErrorCode::AbiError,
            "Invalid unpolled text root frame or parameter index",
        ));
        return super::error::joky_runtime_error_code();
    }
    let owner = super::owned_text::input(arena(frame), data, length);
    if owner.is_null() {
        return super::error::joky_runtime_error_code();
    }
    let cleanup = cleanup(frame);
    if let Err(error) = (*cleanup).entries.run(index, &mut *arena(frame)) {
        super::error::record_error(error);
        return super::error::joky_runtime_error_code();
    }
    (*cleanup).entries.insert(
        index,
        super::cleanup::Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners: vec![Some(owner)],
            hooks: vec![],
        },
    );
    (&mut (*frame).slots)[index] = owner as usize as u64;
    0
}

/// Move the successfully captured host result out of a terminal text root.
/// Returns 1 without a result on pending, failure, or after a previous take.
/// # Safety
/// Frame and zeroed output are live and exclusive. Release successful output via joky_string_result_free_v2.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_take_text_result_v2(
    frame: *mut CpsFrame,
    output: *mut super::owned_text::JokyOwnedTextV2,
) -> i32 {
    let Some(output) = output.as_mut() else {
        return 1;
    };
    *output = super::owned_text::JokyOwnedTextV2::default();
    let Some(frame) = frame.as_mut() else {
        return 1;
    };
    if frame.state != DONE || !frame.root_text {
        return 1;
    }
    match frame.text_result.take() {
        Some(text) => {
            *output = super::owned_text::host_result(text);
            0
        }
        None => 1,
    }
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
    fn task_scope_reclaims_registrations_while_an_older_child_is_pending() {
        thread_local! {
            static POLLS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
            let frame = &mut *frame;
            POLLS.with(|polls| polls.borrow_mut().push(frame.slots[0]));
            if frame.slots[0] == 0 {
                1
            } else {
                frame.state = DONE;
                0
            }
        }
        let mut frame = CpsFrame::new(0);
        unsafe {
            task_scope_enter(&mut frame);
            let pending = task_child(&mut frame, 1);
            task_branch_commit(&mut frame, pending, poll);
            for id in 1..=4000 {
                let child = task_child(&mut frame, 1);
                (&mut (*child).slots)[0] = id;
                task_branch_commit(&mut frame, child, poll);
                assert_eq!(task_round(&mut frame), 0);
                POLLS.with(|polls| {
                    assert_eq!(&*polls.borrow(), &[0, id]);
                    polls.borrow_mut().clear();
                });
                let scope = &frame.task_scopes[0];
                assert_eq!(scope.children.len(), 1);
                assert!(scope.children.capacity() <= 16);
            }
            // The oldest child keeps its identity through every compaction.
            (&mut (*pending).slots)[0] = 4001;
            assert_eq!(task_join(&mut frame), 0);
            assert!(frame.task_scopes.is_empty());
        }
    }

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
