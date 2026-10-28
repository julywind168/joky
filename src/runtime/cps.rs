//! Shared ABI for generated CPS code and native, nonblocking host callbacks.
//!
//! Entries receive one frame pointer and return 0 (complete), 1 (pending), or
//! another value (failure). A pending callback is polled again with the same
//! frame after the caller signals readiness; it must preserve its own progress.

pub(crate) mod concurrent;
mod isolate;
pub(crate) mod parallel;
pub(crate) mod protocol;
pub(crate) mod task;

use super::arena::Arena;
use super::borrowed_host::{BorrowingHostStop, STOP_CANCELLED};
use std::{cell::UnsafeCell, rc::Rc};

pub const DONE: u32 = u32::MAX;
pub const INVALID_STATE: i32 = 2;
pub const RUNTIME_ERROR: i32 = 3;
pub const FAILED: u32 = u32::MAX - 1;

use super::task_scope::{ChildId, LocalTaskGroup, LocalTaskScope};
type DynamicTaskScope = LocalTaskScope<Box<CpsFrame>>;

struct ReturnedPlace {
    arena: Rc<UnsafeCell<Arena>>,
    pointer: std::ptr::NonNull<u8>,
}
impl Drop for ReturnedPlace {
    fn drop(&mut self) {
        unsafe {
            (*self.arena.get()).release(self.pointer);
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
    pub(crate) parallel_return: bool,
    pub(crate) returned_cleanup: Option<super::cleanup::Entry>,
    pub(crate) text_views: Vec<super::string::JokyText>,
    pub(crate) domain_return: Option<(crate::Type, usize)>,
    pub(crate) domain_result: Option<Vec<u64>>,
    pub(crate) root_text: bool,
    pub(crate) text_result: Option<String>,
    place_result: Option<ReturnedPlace>,
    concurrent: Option<LocalTaskGroup>,
    isolated_failure: u64,
    concurrent_results: Vec<Option<Box<CpsFrame>>>,
    prepared: Option<task::Prepared>,
    task_polling: bool,
    task_entries: std::collections::BTreeMap<(usize, usize, usize), Rc<task::CheckedEntry>>,
    task_code: Option<Rc<task::CheckedEntry>>,
    task_protocol: Vec<protocol::Scope>,
    parallel_charge: Option<super::parallel::Lease>,
    pub(crate) parallel_result: Option<super::parallel::Lease>,
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
            parallel_return: false,
            returned_cleanup: None,
            text_views: Vec::new(),
            domain_return: None,
            domain_result: None,
            root_text: false,
            text_result: None,
            place_result: None,
            concurrent: None,
            isolated_failure: 0,
            concurrent_results: Vec::new(),
            prepared: None,
            task_polling: false,
            task_entries: Default::default(),
            task_code: None,
            task_protocol: Vec::new(),
            parallel_charge: None,
            parallel_result: None,
        }
    }

    pub(crate) fn try_new(slot_count: usize) -> Result<Self, crate::Error> {
        let bytes = Self::charged_size(slot_count)?;
        let charge = super::parallel::reserve_current(bytes)?;
        let mut frame = Self::new(slot_count);
        frame.parallel_charge = charge;
        Ok(frame)
    }
    fn charged_size(slot_count: usize) -> Result<usize, crate::Error> {
        slot_count
            .checked_mul(8)
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()))
            .ok_or_else(|| {
                crate::Error::new_with_code(
                    crate::ErrorPhase::Runtime,
                    crate::ErrorCode::ResourceLimit,
                    "CPS frame size overflow",
                )
            })
    }
    pub(crate) fn reserve_slots(&mut self, slots: usize) -> Result<(), crate::Error> {
        if let Some(charge) = &mut self.parallel_charge {
            charge.resize(Self::charged_size(slots)?)?;
        }
        Ok(())
    }

    fn release_concurrent_results(&mut self) {
        if let Some(group) = &self.concurrent {
            // A failed packet initialization is newer than every published result.
            for index in group
                .active_index()
                .into_iter()
                .chain(group.completion_order().iter().rev().copied())
            {
                self.concurrent_results[index].take();
            }
        }
        self.concurrent_results.clear();
    }

    pub(crate) fn release(&mut self) {
        self.stop_host(STOP_CANCELLED);
        if self.state != DONE {
            self.state = FAILED;
        }
        let primary = super::error::current_error();
        if let Some(group) = &mut self.concurrent {
            group.stop(primary.clone());
            let _ = group.begin_drain();
        }
        for scope in &mut self.task_scopes {
            match &primary {
                Some(error) => scope.fail(error.clone()),
                None => scope.cancel(),
            }
            let _ = scope.begin_drain();
        }
        let prepared: Vec<_> = self
            .task_protocol
            .iter_mut()
            .flat_map(|scope| scope.take_prepared())
            .collect();
        drop(prepared);
        self.children.clear();
        if let Some(group) = &mut self.concurrent {
            if !group.drain_finished() {
                let cleanup = super::error::current_error().map_or(Ok(()), Err);
                let _ = group.finish_drain(cleanup);
            }
        }
        for index in 0..self.task_scopes.len() {
            let children = self.task_scopes[index].take_children();
            drop(children);
            let cleanup = super::error::current_error().map_or(Ok(()), Err);
            let _ = self.task_scopes[index].finish_drain(cleanup);
        }
        self.task_scopes.clear();
        self.release_concurrent_results();
        self.concurrent = None;
        self.task_entry = None;
        self.prepared = None;
        self.text_views.clear();
        if !self.cleanup.is_null() {
            let cleanup = std::mem::replace(&mut self.cleanup, std::ptr::null_mut());
            unsafe {
                super::cleanup::finish(cleanup);
            }
        }
        if self.state != DONE || super::error::joky_runtime_error_code() != 0 {
            self.text_result = None;
            self.place_result = None;
            self.domain_result = None;
        }
        if let Some(returned) = self.returned_cleanup.take() {
            if let Some(arena) = &self.arena {
                if let Err(error) = unsafe { returned.run(&mut *arena.get()) } {
                    super::error::record_error(error);
                }
            }
        }
        self.wake = None;
        self.arena = None;
        self.task_code = None;
        self.task_entries.clear();
        self.task_protocol.clear();
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
                frames.extend(scope.entries().map(|(_, child, _)| child.as_ref()));
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
    if children[index].is_none() {
        let mut child = match CpsFrame::try_new(slot_count) {
            Ok(child) => child,
            Err(error) => {
                super::error::record_error(error);
                return std::ptr::null_mut();
            }
        };
        child.arena = frame.arena.clone();
        children[index] = Some(Box::new(child));
    }
    children[index].as_mut().unwrap().as_mut()
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_release_children"]
pub unsafe extern "C" fn release_children(frame: *mut CpsFrame) {
    if (*frame)
        .concurrent
        .as_ref()
        .is_some_and(|group| !group.drain_finished())
    {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidState,
            "Cannot release a task group before child cleanup completes",
        ));
        return;
    }
    (*frame).children.clear();
    (*frame).release_concurrent_results();
    (*frame).concurrent = None;
}

/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_scope_enter"]
pub unsafe extern "C" fn task_scope_enter(frame: *mut CpsFrame) {
    if (*frame).task_scopes.iter().any(LocalTaskScope::draining) {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidState,
            "Cannot open a task scope during cleanup",
        ));
        return;
    }
    (*frame).task_scopes.push(DynamicTaskScope::default());
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
    let live: usize = frame.task_scopes.iter().map(LocalTaskScope::live_len).sum();
    if live >= MAX_DYNAMIC_TASK_CHILDREN {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::ResourceLimit,
            "Dynamic task quota exceeded",
        ));
        return std::ptr::null_mut();
    }
    let mut child = match CpsFrame::try_new(slot_count) {
        Ok(child) => child,
        Err(error) => {
            super::error::record_error(error);
            return std::ptr::null_mut();
        }
    };
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
    if child
        .task_code
        .as_ref()
        .is_some_and(|code| !std::ptr::fn_addr_eq(code.entry, entry))
    {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Abi,
            crate::ErrorCode::AbiError,
            "Published task entry differs from its checked descriptor",
        ));
        drop(child);
        return;
    }
    child.task_entry = Some(entry);
    let scope = frame
        .task_scopes
        .last_mut()
        .expect("branch requires an open task scope");
    if let Err((error, child)) = scope.publish(child) {
        super::error::record_error(error);
        drop(child);
    }
}

/// Advance the innermost open scope by one polling round. Returns 0 once the
/// scope is complete and closed, 1 when some child reported Pending, and the
/// failing child's status otherwise (the scope is cancelled first).
/// # Safety
/// `frame` must be a live, exclusively borrowed runtime frame.
#[export_name = "__joky_cps_task_join"]
pub unsafe extern "C" fn task_join(frame: *mut CpsFrame) -> i32 {
    if (*frame).task_scopes.iter().any(LocalTaskScope::draining) {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidState,
            "Cannot reenter a draining task scope",
        ));
        return INVALID_STATE;
    }
    let Some(index) = (*frame).task_scopes.len().checked_sub(1) else {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidState,
            "Missing joining task scope",
        ));
        return INVALID_STATE;
    };
    (&mut (*frame).task_scopes)[index].close();
    loop {
        let selected = {
            let scope = &mut (&mut (*frame).task_scopes)[index];
            scope.next().map(|id| {
                (
                    id,
                    &mut **scope.get_mut(id).expect("live child") as *mut CpsFrame,
                )
            })
        };
        let Some((id, child)) = selected else { break };
        let outcome = super::error::checked(|| task::poll(child));
        let failure = match outcome {
            Ok(status @ (0 | 1)) => {
                let completed = (&mut (*frame).task_scopes)[index]
                    .polled(id, status == 0)
                    .expect("selected child");
                super::error::checked(|| drop(completed)).err()
            }
            Ok(status) => Some(crate::Error::new_with_code(
                crate::ErrorPhase::Runtime,
                if super::isolate::active() {
                    crate::ErrorCode::HostError
                } else {
                    crate::ErrorCode::InvalidState
                },
                format!("Task child returned failure status {status}"),
            )),
            Err(error) => Some(error),
        };
        if let Some(error) = failure {
            let scope = &mut (&mut (*frame).task_scopes)[index];
            scope.fail(error.clone());
            let cleanup = drain_dynamic_scope(frame, index);
            (*frame).task_scopes.pop();
            super::error::record_error(cleanup.err().unwrap_or(error));
            return RUNTIME_ERROR;
        }
    }
    if (&(*frame).task_scopes)[index].is_empty() {
        (&mut (*frame).task_scopes)[index].close();
        let outcome = drain_dynamic_scope(frame, index);
        (*frame).task_scopes.pop();
        match outcome {
            Ok(()) => 0,
            Err(error) => {
                super::error::record_error(error);
                RUNTIME_ERROR
            }
        }
    } else {
        (&mut (*frame).task_scopes)[index].restart_join_round();
        1
    }
}

unsafe fn drain_dynamic_scope(frame: *mut CpsFrame, index: usize) -> Result<(), crate::Error> {
    let children = {
        let scope = &mut (&mut (*frame).task_scopes)[index];
        if !scope.begin_drain()? {
            return Ok(());
        }
        scope.take_children()
    };
    let cleanup = super::error::checked(|| drop(children));
    (&mut (*frame).task_scopes)[index].finish_drain(cleanup)
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
    let returned = (*frame).returned_cleanup.take();
    let retained_arena = returned.as_ref().and((*frame).arena.clone());
    (*frame).release();
    (*frame).returned_cleanup = returned;
    if retained_arena.is_some() {
        (*frame).arena = retained_arena;
    }
}

/// Retain a completed Copy result until the caller has copied it.
/// # Safety
/// The frame owns a fully initialized place at the compiler-selected result slot.
#[export_name = "__joky_cps_finish_place_v1"]
pub(crate) unsafe extern "C" fn finish_place(frame: *mut CpsFrame, slot: usize) {
    let pointer = (*cleanup(frame))
        .entries
        .places
        .take(slot)
        .expect("registered Copy result");
    (*frame).place_result = Some(ReturnedPlace {
        arena: (*frame).arena.as_ref().unwrap().clone(),
        pointer,
    });
}

/// Publish an owning return only after local destructors succeed.
/// # Safety
/// The live frame owns the checked return registration in its cleanup table.
#[export_name = "__joky_cps_finish_return_v1"]
pub(crate) unsafe extern "C" fn finish_return(frame: *mut CpsFrame, slot: usize) {
    let cleanup = std::mem::replace(&mut (*frame).cleanup, std::ptr::null_mut());
    if (*frame).parallel_return {
        (*frame).returned_cleanup = super::cleanup::take_return(cleanup, slot);
    } else {
        super::cleanup::finish_return(cleanup, slot);
    }
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
        let result = super::owned_text::borrow(owner).and_then(|text| {
            let charge = super::parallel::reserve_current(
                text.len() + std::mem::size_of::<crate::vm::TaskValue>(),
            )?;
            let text = super::owned_text::copy_to_host(text)?;
            (*frame).parallel_result = charge;
            Ok(text)
        });
        match result {
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
    if child.is_null() {
        return child;
    }
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

pub(crate) fn symbols() -> [(&'static str, *const u8); 44] {
    [
        ("__joky_task_context_v1", protocol::context as *const u8),
        (
            "__joky_task_current_scope_v1",
            protocol::current as *const u8,
        ),
        ("__joky_task_scope_open_v1", protocol::open as *const u8),
        (
            "__joky_task_child_prepare_v1",
            protocol::prepare as *const u8,
        ),
        ("__joky_task_publish_v1", protocol::publish as *const u8),
        ("__joky_task_join_v1", protocol::join as *const u8),
        ("__joky_task_take_result_v1", protocol::take as *const u8),
        (
            "__joky_task_request_cancel_v1",
            protocol::cancel as *const u8,
        ),
        ("__joky_task_drain_v1", protocol::drain as *const u8),
        (
            "__joky_task_scope_release_v1",
            protocol::release as *const u8,
        ),
        ("__joky_task_entry_v1", task::register as *const u8),
        ("__joky_cps_task_prepare_v1", task::prepare as *const u8),
        ("__joky_cps_task_inputs_v1", task::inputs as *const u8),
        ("__joky_cps_task_poll_v1", task::poll as *const u8),
        ("__joky_isolate_poll_v1", isolate::poll as *const u8),
        ("__joky_isolate_drain_v1", isolate::drain as *const u8),
        (
            "__joky_cps_concurrent_prepare_v1",
            task::prepare_static as *const u8,
        ),
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
        ("__joky_cps_finish_place_v1", finish_place as *const u8),
        ("__joky_cps_slots", slots as *const u8),
        ("__joky_cps_child", child as *const u8),
        ("__joky_cps_release_child", release_child as *const u8),
        ("__joky_cps_child_at", child_at as *const u8),
        ("__joky_cps_release_children", release_children as *const u8),
        (
            "__joky_cps_concurrent_open_v1",
            concurrent::open as *const u8,
        ),
        (
            "__joky_cps_concurrent_next_v1",
            concurrent::next as *const u8,
        ),
        (
            "__joky_cps_concurrent_polled_v1",
            concurrent::polled as *const u8,
        ),
        (
            "__joky_cps_concurrent_result_v1",
            concurrent::result as *const u8,
        ),
        (
            "__joky_cps_concurrent_take_v1",
            concurrent::take as *const u8,
        ),
        (
            "__joky_cps_concurrent_next_result_v1",
            concurrent::next_result as *const u8,
        ),
        (
            "__joky_cps_concurrent_drain_v1",
            concurrent::drain as *const u8,
        ),
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

unsafe fn round_select(root: *mut CpsFrame) -> Option<(*mut DynamicTaskScope, ChildId)> {
    enum Visit {
        Frame(*mut CpsFrame),
        Child(*mut DynamicTaskScope, ChildId),
    }
    let mut pending = vec![Visit::Frame(root)];
    while let Some(visit) = pending.pop() {
        let frame = match visit {
            Visit::Child(scope, index) => return Some((scope, index)),
            Visit::Frame(frame) => &mut *frame,
        };
        // Reverse insertion preserves pre-order: dynamic scopes in source
        // order, then the suspended body's ordinary call/join children.
        if !frame
            .task_protocol
            .last()
            .is_some_and(|scope| scope.kind == 3)
        {
            pending.extend(
                frame
                    .children
                    .iter_mut()
                    .rev()
                    .flatten()
                    .map(|child| Visit::Frame(&mut **child)),
            );
        }
        for scope in frame.task_scopes.iter_mut().rev() {
            if scope.terminal() {
                continue;
            }
            let scope_ptr = scope as *mut DynamicTaskScope;
            for (id, child, ready) in scope.entries_mut().rev() {
                if ready {
                    pending.push(Visit::Child(scope_ptr, id));
                } else {
                    pending.push(Visit::Frame(&mut **child));
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
        if !frame
            .task_protocol
            .last()
            .is_some_and(|scope| scope.kind == 3)
        {
            pending.extend(
                frame
                    .children
                    .iter_mut()
                    .flatten()
                    .map(|child| &mut **child as *mut _),
            );
        }
        for scope in &mut frame.task_scopes {
            // There is no selected child across this boundary. Remove finished
            // registrations without reordering live children or losing a join's cursor.
            scope.finish_round();
            pending.extend(
                scope
                    .entries_mut()
                    .map(|(_, child, _)| &mut **child as *mut _),
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
    if (*root).task_scopes.iter().any(LocalTaskScope::draining) {
        super::error::record_error(crate::Error::new_with_code(
            crate::ErrorPhase::Runtime,
            crate::ErrorCode::InvalidState,
            "Cannot advance task scopes during cleanup",
        ));
        return INVALID_STATE;
    }
    loop {
        let selected = round_select(root);
        let Some((scope_ptr, index)) = selected else {
            round_reset(&mut *root);
            return 0;
        };
        let child = match (*scope_ptr).get_mut(index) {
            Some(child) => &mut **child as *mut CpsFrame,
            None => continue,
        };
        let failure = match super::error::checked(|| task::poll(child)) {
            Ok(status @ (0 | 1)) => {
                let completed = (*scope_ptr)
                    .polled(index, status == 0)
                    .expect("selected child");
                super::error::checked(|| drop(completed)).err()
            }
            Ok(status) => Some(crate::Error::new_with_code(
                crate::ErrorPhase::Runtime,
                if super::isolate::active() {
                    crate::ErrorCode::HostError
                } else {
                    crate::ErrorCode::InvalidState
                },
                format!("Task child returned failure status {status}"),
            )),
            Err(error) => Some(error),
        };
        if let Some(error) = failure {
            (*scope_ptr).fail(error.clone());
            super::error::record_error(error);
            return RUNTIME_ERROR;
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
                assert_eq!(scope.storage().0, 1);
                assert!(scope.storage().1 <= 16);
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
