//! Runtime continuation control state.
//!
//! This module owns the scheduler-facing part of a continuation: generation,
//! lifecycle state and timer completion.  It intentionally does not contain a
//! native stack; typed spill storage is owned by the continuation allocation.

mod abi;
mod cown_acquire;
pub(crate) use cown_acquire::{
    jk_continuation_start_cown_acquire, jk_continuation_start_cown_wait,
};
mod abi_storage;
mod function_calls;
mod lifecycle;
mod metadata;
mod pending_operations;
mod task_wait;
pub(crate) use task_wait::{
    jk_continuation_exit_handler, jk_continuation_own_handler, jk_continuation_start_task_wait,
};
pub(crate) mod registries;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use registries::describe_scope;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use registries::resource_snapshot;
mod storage;

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::scope::{self, RuntimeScope, ScopeId, ScopeWork};
pub(crate) use abi::*;
pub(crate) use abi_storage::*;
pub(crate) use function_calls::with_callback_pending_boundary;
pub(crate) use function_calls::with_function_pending_boundary;
use function_calls::FunctionCallPhase;
pub(crate) use function_calls::{jk_task_function_status, with_task_function_pending_boundary};
use pending_operations::OperationHandoff;
pub(crate) use pending_operations::{
    jk_continuation_start_pending_provider, jk_continuation_start_pending_timer,
};
use registries::{
    register_handle, register_machine_entry, register_suspend_provider, registered_machine_entry,
    retain_const_handle, retain_handle, suspend_providers, task_continuations, unregister_handle,
    unregister_machine_entries_for_scope as unregister_all_machine_entries_for_scope,
    unregister_machine_entry, unregister_suspend_provider,
};
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationState {
    Ready = 0,
    Sleeping = 1,
    Resuming = 2,
    Cancelled = 3,
    Completed = 4,
}

thread_local! {
    /// The scheduler callback currently executing on this thread, if any.
    /// `jk_continuation_free` uses this to defer freeing the public handle until
    /// the callback has returned instead of waiting on itself.
    static CURRENT_CALLBACK_INNER: Cell<usize> = const { Cell::new(0) };
}

struct TimerTicket {
    released: AtomicBool,
}

impl TimerTicket {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: AtomicBool::new(false),
        })
    }

    fn release(&self) -> bool {
        !self.released.swap(true, Ordering::AcqRel)
    }
}

struct CallbackLease {
    inner: Arc<ContinuationInner>,
    previous: usize,
}

impl CallbackLease {
    fn activate(&mut self) {
        self.previous = CURRENT_CALLBACK_INNER.with(|current| {
            let previous = current.get();
            current.set(Arc::as_ptr(&self.inner) as usize);
            previous
        });
    }
}

impl Drop for CallbackLease {
    fn drop(&mut self) {
        CURRENT_CALLBACK_INNER.with(|current| current.set(self.previous));
        let previous = self
            .inner
            .callbacks_in_flight
            .fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "continuation callback underflow");
        self.inner.wake.notify_all();
        if previous == 1 && self.inner.free_requested.load(Ordering::Acquire) {
            let continuation = Continuation {
                inner: Arc::clone(&self.inner),
            };
            continuation.finish_free();
        }
    }
}

// A published native Pending transfers its public handle to the runtime.
// Terminal state retires that ownership after callback/native/child leases
// drain. Legacy manually-owned ABI handles still require explicit free.
struct TerminalHandleRelease<'a>(&'a Continuation);

impl Drop for TerminalHandleRelease<'_> {
    fn drop(&mut self) {
        let value = self.0;
        if value.inner.runtime_owned.load(Ordering::Acquire)
            && matches!(
                value.state(),
                ContinuationState::Completed | ContinuationState::Cancelled
            )
        {
            value.inner.free_requested.store(true, Ordering::Release);
            value.finish_free();
        }
    }
}

struct ContinuationInner {
    region: Mutex<Arc<super::region::Region>>,
    _resource: crate::runtime::resources::Lease,
    state: Mutex<ContinuationState>,
    function_call: Mutex<FunctionCallPhase>,
    operation_handoff: Mutex<OperationHandoff>,
    native_operation_active: AtomicBool,
    function_parent: AtomicUsize,
    /// The enclosing task's context, resolved once when this activation began
    /// and inherited from its parent. Suspended recursion would otherwise turn
    /// every task-context lookup into a walk over a depth-long parent chain.
    inherited_task_context: AtomicUsize,
    function_children: Mutex<Vec<Continuation>>,
    wake: Condvar,
    generation: AtomicU64,
    resume_entry: AtomicUsize,
    resume_callback: AtomicPtr<c_void>,
    machine_entry: AtomicPtr<c_void>,
    frame_pointer: AtomicPtr<u8>,
    frame_size: AtomicUsize,
    program_counter: AtomicUsize,
    result_pointer: AtomicPtr<u8>,
    result_size: AtomicUsize,
    suspend_result_pointer: AtomicPtr<u8>,
    suspend_result_size: AtomicUsize,
    failure_pointer: AtomicPtr<u8>,
    failure_size: AtomicUsize,
    failure_pending: AtomicBool,
    suspend_argument_pointer: AtomicPtr<u8>,
    suspend_argument_size: AtomicUsize,
    spill_pointer: AtomicPtr<u8>,
    spill_size: AtomicUsize,
    frame_storage: Mutex<Option<Box<[u8]>>>,
    result_storage: Mutex<Option<Box<[u8]>>>,
    suspend_result_storage: Mutex<Option<Box<[u8]>>>,
    failure_storage: Mutex<Option<Box<[u8]>>>,
    suspend_argument_storage: Mutex<Option<Box<[u8]>>>,
    spill_storage: Mutex<Option<Box<[u8]>>>,
    timer_active: AtomicUsize,
    timer_id: AtomicU64,
    timer_ticket: Mutex<Option<Arc<TimerTicket>>>,
    active_operation: AtomicU64,
    provider_cancel: AtomicPtr<c_void>,
    handle: AtomicUsize,
    task_context: AtomicUsize,
    /// Handler chain pinned at suspend time so a machine-entry resume on any
    /// worker reinstalls the same dynamic handler frame chain.
    handler_frame: AtomicUsize,
    root_group: AtomicUsize,
    cancellation_requested: AtomicUsize,
    cancellation_draining: AtomicUsize,
    machine_entry_active: AtomicUsize,
    lifecycle_gate: Mutex<()>,
    callbacks_in_flight: AtomicUsize,
    free_requested: AtomicBool,
    runtime_owned: AtomicBool,
    handle_released: AtomicBool,
    owner_handle: AtomicUsize,
    cleanup_entries: Mutex<Vec<ContinuationCleanup>>,
    scope: Arc<RuntimeScope>,
    owned_handlers: Mutex<Vec<usize>>,
    task_wait_active: AtomicBool,
    cown_acquire_active: AtomicBool,
    cown_acquisition: Mutex<Option<cown_acquire::CownAcquisition>>,
    task_wait_group: Mutex<Option<Arc<crate::runtime::task::TaskGroupInner>>>,
    scope_work: Mutex<Option<ScopeWork>>,
}

#[derive(Clone, Copy)]
struct ContinuationCleanup {
    storage: u8,
    offset: usize,
    callback: usize,
    region: bool,
    armed: bool,
}

pub(crate) use joky_runtime_abi::{
    CONTINUATION_FAILURE_STORAGE, CONTINUATION_FRAME_STORAGE, CONTINUATION_RESULT_STORAGE,
    CONTINUATION_SPILL_STORAGE, CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
    CONTINUATION_SUSPEND_RESULT_STORAGE,
};
/// Version for the continuation C ABI consumed by generated code.
#[cfg(test)]
pub(crate) const CONTINUATION_ABI_VERSION: u32 = 1;

#[cfg(test)]
pub(crate) fn continuation_abi_descriptor() -> crate::runtime::abi::ContinuationAbiDescriptor {
    crate::runtime::abi::ContinuationAbiDescriptor {
        version: CONTINUATION_ABI_VERSION,
        state_ready: ContinuationState::Ready as u8,
        state_sleeping: ContinuationState::Sleeping as u8,
        state_resuming: ContinuationState::Resuming as u8,
        state_cancelled: ContinuationState::Cancelled as u8,
        state_completed: ContinuationState::Completed as u8,
    }
}
#[derive(Clone, Copy)]
struct SuspendProvider {
    start: usize,
    cancel: usize,
    registrations: usize,
}

/// Scheduler-owned continuation header. The allocation is stable while a
/// timer is in flight; timer threads retain the inner Arc rather than the raw
/// ABI pointer, so freeing the public handle cannot race with timer completion.
#[derive(Clone)]
pub(crate) struct Continuation {
    inner: Arc<ContinuationInner>,
}

impl Continuation {
    /// Retain the runtime state behind a live ABI handle for a native provider.
    /// This goes through the same registry as every exported continuation ABI
    /// entry point and therefore never dereferences a stale public handle.
    pub(crate) unsafe fn retain_registered(handle: *mut Continuation) -> Option<Self> {
        // SAFETY: `retain_handle` only performs a registry lookup and checks
        // the lifecycle gate; it never dereferences the raw handle.
        unsafe { retain_handle(handle) }
    }

    fn new(generation: u64) -> Self {
        let scope = scope::current_or_default();
        Self {
            inner: Arc::new(ContinuationInner {
                region: Mutex::new(super::region::current()),
                _resource: crate::runtime::resources::Lease::new(
                    &scope,
                    crate::runtime::resources::Kind::Continuations,
                    1,
                ),
                state: Mutex::new(ContinuationState::Ready),
                function_call: Mutex::new(FunctionCallPhase::Inactive),
                operation_handoff: Mutex::new(OperationHandoff::Inactive),
                native_operation_active: AtomicBool::new(false),
                function_parent: AtomicUsize::new(0),
                inherited_task_context: AtomicUsize::new(0),
                function_children: Mutex::new(Vec::new()),
                wake: Condvar::new(),
                generation: AtomicU64::new(generation),
                resume_entry: AtomicUsize::new(0),
                resume_callback: AtomicPtr::new(std::ptr::null_mut()),
                machine_entry: AtomicPtr::new(std::ptr::null_mut()),
                frame_pointer: AtomicPtr::new(std::ptr::null_mut()),
                frame_size: AtomicUsize::new(0),
                program_counter: AtomicUsize::new(0),
                result_pointer: AtomicPtr::new(std::ptr::null_mut()),
                result_size: AtomicUsize::new(0),
                suspend_result_pointer: AtomicPtr::new(std::ptr::null_mut()),
                suspend_result_size: AtomicUsize::new(0),
                failure_pointer: AtomicPtr::new(std::ptr::null_mut()),
                failure_size: AtomicUsize::new(0),
                failure_pending: AtomicBool::new(false),
                suspend_argument_pointer: AtomicPtr::new(std::ptr::null_mut()),
                suspend_argument_size: AtomicUsize::new(0),
                spill_pointer: AtomicPtr::new(std::ptr::null_mut()),
                spill_size: AtomicUsize::new(0),
                frame_storage: Mutex::new(None),
                result_storage: Mutex::new(None),
                suspend_result_storage: Mutex::new(None),
                failure_storage: Mutex::new(None),
                suspend_argument_storage: Mutex::new(None),
                spill_storage: Mutex::new(None),
                timer_active: AtomicUsize::new(0),
                timer_id: AtomicU64::new(0),
                timer_ticket: Mutex::new(None),
                active_operation: AtomicU64::new(0),
                provider_cancel: AtomicPtr::new(std::ptr::null_mut()),
                handle: AtomicUsize::new(0),
                task_context: AtomicUsize::new(0),
                handler_frame: AtomicUsize::new(0),
                root_group: AtomicUsize::new(0),
                cancellation_requested: AtomicUsize::new(0),
                cancellation_draining: AtomicUsize::new(0),
                machine_entry_active: AtomicUsize::new(0),
                lifecycle_gate: Mutex::new(()),
                callbacks_in_flight: AtomicUsize::new(0),
                free_requested: AtomicBool::new(false),
                runtime_owned: AtomicBool::new(false),
                handle_released: AtomicBool::new(false),
                owner_handle: AtomicUsize::new(0),
                cleanup_entries: Mutex::new(Vec::new()),
                scope,
                owned_handlers: Mutex::new(Vec::new()),
                task_wait_active: AtomicBool::new(false),
                cown_acquire_active: AtomicBool::new(false),
                cown_acquisition: Mutex::new(None),
                task_wait_group: Mutex::new(None),
                scope_work: Mutex::new(None),
            }),
        }
    }

    fn resume(&self) -> bool {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Ready {
            return false;
        }
        *state = ContinuationState::Resuming;
        true
    }

    fn resume_at(&self, generation: u64) -> bool {
        if self.generation() != generation {
            return false;
        }
        self.resume()
    }

    fn set_resume_callback(&self, callback: *mut c_void) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        self.inner
            .resume_callback
            .store(callback, Ordering::Release);
        true
    }

    fn dispatch(&self, handle: *mut Continuation, generation: u64) -> bool {
        if self.generation() != generation {
            return false;
        }
        let _scope_guard = self.inner.scope.enter();
        let _region_guard = super::region::install(
            self.inner
                .region
                .lock()
                .expect("continuation region")
                .clone(),
        );
        // Timer/provider scheduler jobs reserve a callback before enqueueing.
        // Direct ABI dispatch (used by the synchronous root and resumable
        // handler paths) needs the same lease so a generated entry may free
        // its public handle without invalidating the caller's `self` reference.
        let mut callback_lease = if self.in_current_callback() {
            None
        } else {
            self.reserve_callback(handle)
        };
        if let Some(lease) = callback_lease.as_mut() {
            lease.activate();
        }
        let task_context = self.function_task_context();
        if task_context != 0
            && crate::runtime::task::task_context_is_cancelled(
                task_context as *mut crate::runtime::task::TaskContext,
            )
        {
            unsafe {
                jk_continuation_fail_function_chain(
                    handle,
                    crate::runtime::abi::FunctionCallStatus::Cancelled as u8,
                );
            }
            return false;
        }
        let callback = self.inner.resume_callback.load(Ordering::Acquire);
        let machine_entry = self.inner.machine_entry.load(Ordering::Acquire);
        if (callback.is_null() && machine_entry.is_null()) || !self.resume() {
            return false;
        }

        if !machine_entry.is_null() {
            #[cfg(any(test, feature = "test-support"))]
            self.inner
                .scope
                .machine_resumptions
                .fetch_add(1, Ordering::Relaxed);
            // A finalized JIT entry is the scheduler's preferred path. It
            // owns the complete resume CFG and transitions the continuation
            // to Completed when the machine reaches its return.
            let entry: unsafe extern "C" fn(*mut Continuation) =
                unsafe { std::mem::transmute(machine_entry) };
            let context = self.function_task_context() as *mut crate::runtime::task::TaskContext;
            // SAFETY: `task_context` is retained by the enclosing TaskGroup
            // until this continuation completes or is cancelled. The resumed
            // tail may issue further Normal handler requests, so install the
            // chain pinned at suspend time inside the task context scope:
            // it wins over the context's own chain and is undone when the
            // context scope exits.
            let pinned = self.inner.handler_frame.load(Ordering::Acquire)
                as *const crate::runtime::handler::HandlerFrame;
            // Mark the entry active before any cancellation can observe the
            // `Resuming` state, so a racing cancel defers to this boundary
            // instead of reclaiming the storage the entry is about to read.
            self.inner.machine_entry_active.store(1, Ordering::Release);
            if self.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
                // A cancel won the race between `resume()` and the active
                // flag. It either already took the full terminal path or is
                // waiting on this boundary; converge on the terminal state
                // without executing the entry.
                self.finish_cancelled();
                self.inner.machine_entry_active.store(0, Ordering::Release);
                return false;
            }
            // SAFETY: the entry is a finalized C ABI function that must not
            // unwind; the pinned chain was captured from the suspending
            // thread while the pending continuation kept its handlers alive.
            function_calls::with_function_pending_resume_boundary(|| {
                unsafe {
                    crate::runtime::task::with_task_context(context, || {
                        let previous_handler = if pinned.is_null() {
                            None
                        } else {
                            Some(crate::runtime::handler::install(pinned))
                        };
                        if self.resume_cown_acquisition(handle) {
                            entry(handle);
                        }
                        if let Some(previous) = previous_handler {
                            crate::runtime::handler::install(previous);
                        }
                    });
                }
                self.inner.machine_entry_active.store(0, Ordering::Release);
            });
            return true;
        } // The scheduler owns the callback contract and registers a C ABI
          // function with the continuation lifetime. It must not unwind across
          // this runtime boundary.
        let callback: unsafe extern "C" fn(*mut Continuation) =
            unsafe { std::mem::transmute(callback) };
        unsafe { callback(handle) };
        true
    }

    fn set_machine_entry(&self, entry: *mut c_void) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if matches!(
            *state,
            ContinuationState::Completed | ContinuationState::Cancelled
        ) {
            return false;
        }
        self.inner.machine_entry.store(entry, Ordering::Release);
        true
    }

    fn invoke_machine_entry(&self, handle: *mut Continuation) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Resuming {
            return false;
        }
        if self.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
            drop(state);
            self.finish_cancelled();
            return false;
        }
        let entry = self.inner.machine_entry.load(Ordering::Acquire);
        if entry.is_null() {
            return false;
        }
        drop(state);

        let mut callback_lease = if self.in_current_callback() {
            None
        } else {
            self.reserve_callback(handle)
        };
        if let Some(lease) = callback_lease.as_mut() {
            lease.activate();
        }

        // The generated resume entry uses the stable C ABI and must not
        // unwind across the runtime boundary.
        let entry: unsafe extern "C" fn(*mut Continuation) = unsafe { std::mem::transmute(entry) };
        let _scope_guard = self.inner.scope.enter();
        let _region_guard = super::region::install(
            self.inner
                .region
                .lock()
                .expect("continuation region")
                .clone(),
        );
        // Mark the entry active before any cancellation can observe this
        // boundary, so a racing cancel defers to it instead of reclaiming
        // the storage the entry is about to read.
        self.inner.machine_entry_active.store(1, Ordering::Release);
        if self.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
            // A cancel won the race while this trampoline was being
            // admitted. Converge on the terminal state without executing
            // the entry.
            self.finish_cancelled();
            self.inner.machine_entry_active.store(0, Ordering::Release);
            return false;
        }
        // A machine entry frame is never a task-body publish frame.
        // A resumed tail may issue further Normal handler requests. Reinstall
        // the dynamic handler chain pinned at suspend time so those requests
        // reach the same lexical handlers, and restore the worker's previous
        // chain afterwards.
        let pinned = self.inner.handler_frame.load(Ordering::Acquire)
            as *const crate::runtime::handler::HandlerFrame;
        let previous_handler = if pinned.is_null() {
            None
        } else {
            // SAFETY: the chain was pinned from the suspending thread while
            // the pending continuation kept its lexical handlers alive.
            Some(unsafe { crate::runtime::handler::install(pinned) })
        };
        function_calls::with_function_pending_resume_boundary(|| {
            if self.resume_cown_acquisition(handle) {
                unsafe { entry(handle) };
            }
            self.inner.machine_entry_active.store(0, Ordering::Release);
            if let Some(previous) = previous_handler {
                // SAFETY: previous is the worker's own saved chain.
                unsafe { crate::runtime::handler::install(previous) };
            }
        });
        true
    }

    /// Move a completed continuation result into the enclosing task's result
    /// allocation. The continuation and task buffers are distinct ownership
    /// slots, so the byte copy is followed by releasing only the continuation
    /// backing allocation. Managed result ownership remains with the task.
    fn transfer_result_to_task(&self) -> bool {
        let context = self.inner.task_context.load(Ordering::Acquire);
        if context == 0 {
            return false;
        }
        let source = self.result_pointer();
        let source_size = self.result_size();
        if source.is_null() || source_size == 0 {
            return false;
        }
        // SAFETY: the task context is retained by its TaskGroup through join;
        // its result allocation is initialized for the function ABI.
        let context = context as *mut crate::runtime::task::TaskContext;
        let (target, target_size) = unsafe { ((*context).result, (*context).result_size) };
        if target.is_null() || target_size == 0 {
            return false;
        }
        let size = source_size.min(target_size);
        // SAFETY: both regions are live ABI buffers and are non-overlapping
        // allocations owned by the continuation and task respectively.
        unsafe { std::ptr::copy_nonoverlapping(source, target.cast::<u8>(), size) };
        true
    }

    pub(crate) fn cancel(&self) -> bool {
        let _release = TerminalHandleRelease(self);
        self.inner
            .cancellation_draining
            .fetch_add(1, Ordering::AcqRel);
        struct CancellationDrain<'a>(&'a ContinuationInner);
        impl Drop for CancellationDrain<'_> {
            fn drop(&mut self) {
                let previous = self.0.cancellation_draining.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "continuation cancellation drain underflow");
                self.0.wake.notify_all();
            }
        }
        let _drain = CancellationDrain(&self.inner);
        {
            let state = self.inner.state.lock().expect("continuation state mutex");
            if matches!(
                *state,
                ContinuationState::Cancelled | ContinuationState::Completed
            ) {
                return false;
            }
            self.inner
                .cancellation_requested
                .store(1, Ordering::Release);
        }
        if let Some(group) = self
            .inner
            .task_wait_group
            .lock()
            .expect("task wait group mutex")
            .clone()
        {
            crate::runtime::task::enqueue_scheduler_callback(Box::new(move || group.cancel_all()));
            return true;
        }
        // An already-complete group still cannot release the saved frame
        // until the native Pending boundary has returned and published it.
        if self.internal_wait_active()
            && matches!(
                *self
                    .inner
                    .operation_handoff
                    .lock()
                    .expect("operation handoff mutex"),
                OperationHandoff::Withheld | OperationHandoff::Completed
            )
        {
            return true;
        }
        let children = self
            .inner
            .function_children
            .lock()
            .expect("function children mutex")
            .clone();
        for child in &children {
            child.cancel();
        }
        // A child waiting on its own task group cancels asynchronously. Keep
        // this frame (and the enclosing task context) alive until it detaches;
        // detach_function_parent will retry cancellation after the group drains.
        if !self
            .inner
            .function_children
            .lock()
            .expect("function children mutex")
            .is_empty()
        {
            return true;
        }
        // Child entries can borrow this frame and its task context. Their
        // callbacks must stop before the parent's storage is reclaimed.
        for child in &children {
            if !child.in_current_callback() {
                child.wait_for_idle();
            }
        }
        let (provider_cancel, provider_operation) = {
            let mut state = self.inner.state.lock().expect("continuation state mutex");
            if matches!(
                *state,
                ContinuationState::Cancelled | ContinuationState::Completed
            ) {
                return true;
            }
            if self.inner.native_operation_active.load(Ordering::Acquire)
                || (*state == ContinuationState::Resuming
                    && (self.inner.machine_entry_active.load(Ordering::Acquire) != 0
                        || !self.inner.resume_callback.load(Ordering::Acquire).is_null()))
            {
                // A resume entry or Pending provider start hook may still be
                // using its ABI buffers. Its return boundary owns cleanup.
                self.inner
                    .cancellation_requested
                    .store(1, Ordering::Release);
                let task_context = self.inner.task_context.load(Ordering::Acquire);
                if task_context != 0 {
                    // SAFETY: TaskGroup retains the context through join/close.
                    unsafe {
                        crate::runtime::task::cancel_task_context(
                            task_context as *mut crate::runtime::task::TaskContext,
                        )
                    };
                }
                self.inner.wake.notify_all();
                return true;
            }
            let operation = self.inner.active_operation.load(Ordering::Acquire);
            *state = ContinuationState::Cancelled;
            self.inner.active_operation.store(0, Ordering::Release);
            self.inner
                .cancellation_requested
                .store(1, Ordering::Release);
            self.inner
                .resume_callback
                .store(std::ptr::null_mut(), Ordering::Release);
            self.inner
                .machine_entry
                .store(std::ptr::null_mut(), Ordering::Release);
            let provider_cancel = self
                .inner
                .provider_cancel
                .swap(std::ptr::null_mut(), Ordering::AcqRel);
            let provider_operation = if provider_cancel.is_null() {
                operation
            } else {
                operation.saturating_sub(1)
            };
            (provider_cancel, provider_operation)
        };
        self.cancel_active_timer();
        if !provider_cancel.is_null() {
            let cancel: unsafe extern "C" fn(*mut Continuation, u64) -> u8 =
                unsafe { std::mem::transmute(provider_cancel) };
            let handle = self.inner.handle.load(Ordering::Acquire) as *mut Continuation;
            // SAFETY: the provider callback was registered as a C ABI hook and
            // is invoked before its argument/result storage is reclaimed.
            unsafe {
                cancel(handle, provider_operation);
            }
        }
        self.cancel_cown_acquisition();
        self.cleanup_owned_storage(true);
        let mut frame_storage = self
            .inner
            .frame_storage
            .lock()
            .expect("continuation frame mutex");
        frame_storage.take();
        self.inner
            .frame_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.frame_size.store(0, Ordering::Release);
        self.inner.program_counter.store(0, Ordering::Release);
        let mut result_storage = self
            .inner
            .result_storage
            .lock()
            .expect("continuation result mutex");
        result_storage.take();
        self.inner
            .result_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.result_size.store(0, Ordering::Release);
        let mut suspend_result_storage = self
            .inner
            .suspend_result_storage
            .lock()
            .expect("continuation suspend result mutex");
        suspend_result_storage.take();
        self.inner
            .suspend_result_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.suspend_result_size.store(0, Ordering::Release);
        let mut suspend_argument_storage = self
            .inner
            .suspend_argument_storage
            .lock()
            .expect("continuation suspend argument mutex");
        suspend_argument_storage.take();
        self.inner
            .suspend_argument_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.suspend_argument_size.store(0, Ordering::Release);
        let mut storage = self
            .inner
            .spill_storage
            .lock()
            .expect("continuation spill mutex");
        storage.take();
        self.inner
            .spill_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.spill_size.store(0, Ordering::Release);
        let task_context = self.inner.task_context.swap(0, Ordering::AcqRel);
        if task_context != 0 {
            let context = task_context as *mut crate::runtime::task::TaskContext;
            self.unbind_task_context(
                context,
                self.inner.handle.load(Ordering::Acquire) as *mut Continuation,
            );
            // SAFETY: TaskGroup retains the context through join/close.
            unsafe { (*context).continuation.store(0, Ordering::Release) };
            // SAFETY: the terminal cancellation owns this suspended context.
            unsafe {
                (*context)
                    .continuation_pending
                    .store(false, Ordering::Release)
            };
            // A Cown acquired before suspension remains leased until this
            // continuation reaches a terminal state.
            unsafe { crate::runtime::task::cleanup_task_cown_leases(context) };
            // SAFETY: TaskGroup retains the context through join/close.
            unsafe { crate::runtime::task::cancel_task_context(context) };
            crate::runtime::task::complete_suspended_task(context);
        }
        let parent = self.inner.function_parent.swap(0, Ordering::AcqRel);
        self.detach_function_parent(parent);
        self.release_scope_work();
        self.inner.wake.notify_all();
        true
    }

    fn complete(&self) -> bool {
        let _release = TerminalHandleRelease(self);
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Resuming {
            return false;
        }
        let cancelled = self.inner.cancellation_requested.load(Ordering::Acquire) != 0;
        if cancelled
            && !self
                .inner
                .function_children
                .lock()
                .expect("function children mutex")
                .is_empty()
        {
            // Failure can arrive inside a resumed caller while its child is
            // cancelling an internal wait. The child's detach retries cleanup.
            drop(state);
            self.cancel();
            return false;
        }
        let callback_active = !self.inner.resume_callback.load(Ordering::Acquire).is_null();
        *state = if cancelled {
            ContinuationState::Cancelled
        } else {
            ContinuationState::Completed
        };
        self.inner.active_operation.store(0, Ordering::Release);
        self.inner
            .provider_cancel
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner
            .resume_callback
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner
            .machine_entry
            .store(std::ptr::null_mut(), Ordering::Release);
        let parent = self.inner.function_parent.swap(0, Ordering::AcqRel);
        if !cancelled && parent != 0 {
            // The parent either takes the entire payload or leaves it owned
            // here. Resolve that transfer before draining cleanup metadata:
            // normal completion discards final-result cleanup registrations.
            let accepted = unsafe {
                jk_continuation_complete_function_pending(
                    parent as *mut Continuation,
                    self.result_pointer(),
                    self.result_size(),
                ) != 0
            };
            if !accepted {
                self.cleanup_storage(CONTINUATION_RESULT_STORAGE);
            }
            // Success moved ownership to the parent; rejection just dropped
            // it. In either case this continuation no longer owns a result.
            self.release_result();
        }
        // A normal machine entry owns the frame/spill values until it has
        // finished executing, but the result buffer is transferred to the
        // caller and must remain available for task join.  Cancellation owns
        // all three storage areas.
        if cancelled
            || self.inner.machine_entry_active.load(Ordering::Acquire) != 0
            || callback_active
        {
            self.cleanup_owned_storage(cancelled);
        }
        let transferred_result = !cancelled && parent == 0 && self.transfer_result_to_task();
        let mut frame_storage = self
            .inner
            .frame_storage
            .lock()
            .expect("continuation frame mutex");
        frame_storage.take();
        self.inner
            .frame_pointer
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner.frame_size.store(0, Ordering::Release);
        self.inner.program_counter.store(0, Ordering::Release);
        if cancelled {
            self.release_result();
            self.release_suspend_result();
            self.release_failure();
            self.release_suspend_arguments();
            self.release_spill();
        } else {
            self.release_suspend_result();
            self.release_failure();
            self.release_suspend_arguments();
            if transferred_result {
                self.release_result();
            }
        }
        self.inner.wake.notify_all();
        let task_context = self.inner.task_context.swap(0, Ordering::AcqRel);
        if task_context != 0 {
            let context = task_context as *mut crate::runtime::task::TaskContext;
            self.unbind_task_context(
                context,
                self.inner.handle.load(Ordering::Acquire) as *mut Continuation,
            );
            // SAFETY: TaskGroup retains the context through join/close.
            unsafe { (*context).continuation.store(0, Ordering::Release) };
            // SAFETY: a terminal continuation is no longer pending.
            unsafe {
                (*context)
                    .continuation_pending
                    .store(false, Ordering::Release)
            };
            unsafe { crate::runtime::task::cleanup_task_cown_leases(context) };
            if cancelled {
                // SAFETY: TaskGroup retains the context through join/close.
                unsafe { crate::runtime::task::cancel_task_context(context) };
            }
            crate::runtime::task::complete_suspended_task(context);
        }
        drop(state);
        self.detach_function_parent(parent);
        true
    }

    fn complete_operation(&self, operation: u64) -> bool {
        let active = self.inner.active_operation.load(Ordering::Acquire);
        if active != operation.wrapping_add(1) {
            return false;
        }
        let handle = self.inner.handle.load(Ordering::Acquire) as *mut Continuation;
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state == ContinuationState::Resuming {
            drop(state);
            return self.complete();
        }
        if !matches!(
            *state,
            ContinuationState::Sleeping | ContinuationState::Ready
        ) {
            return false;
        }
        if *state == ContinuationState::Sleeping {
            *state = ContinuationState::Ready;
        }
        let machine_entry = self.inner.machine_entry.load(Ordering::Acquire);
        let callback = self.inner.resume_callback.load(Ordering::Acquire);
        let schedule_resume = !machine_entry.is_null() || !callback.is_null();
        if schedule_resume {
            self.inner.active_operation.store(0, Ordering::Release);
        }
        self.inner
            .provider_cancel
            .store(std::ptr::null_mut(), Ordering::Release);
        drop(state);
        if schedule_resume {
            self.enqueue_resume(handle);
        } else {
            self.release_scope_work();
        }
        self.inner.wake.notify_all();
        true
    }

    fn enqueue_resume(&self, handle: *mut Continuation) {
        if !self.operation_resume_allowed() {
            return;
        }
        let continuation = Continuation {
            inner: Arc::clone(&self.inner),
        };
        let generation = continuation.generation();
        let handle_address = handle as usize;
        let Some(mut callback_lease) = continuation.reserve_callback(handle) else {
            continuation.release_scope_work();
            continuation.inner.wake.notify_all();
            return;
        };
        self.inner.scope.enqueue_resume(Box::new(move || {
            callback_lease.activate();
            let _callback_lease = callback_lease;
            let _scope_guard = continuation.inner.scope.enter();
            if continuation.internal_wait_active()
                && continuation
                    .inner
                    .cancellation_requested
                    .load(Ordering::Acquire)
                    != 0
            {
                continuation.cancel();
            } else if continuation.state() == ContinuationState::Ready {
                let _ = continuation.dispatch(handle_address as *mut Continuation, generation);
            }
            continuation.release_scope_work_if_idle();
            continuation.inner.wake.notify_all();
        }));
    }

    /// Publish a provider-produced result for the active suspending operation
    /// and make the continuation runnable.  The payload is a flattened ABI
    /// buffer whose ownership (including managed pointer words) moves into
    /// the continuation result slot on success.
    pub(crate) fn complete_suspend_with_payload(
        &self,
        handle: *mut Continuation,
        operation: u64,
        payload: *const u8,
        payload_size: usize,
    ) -> bool {
        if self.inner.active_operation.load(Ordering::Acquire) != operation.wrapping_add(1) {
            return false;
        }
        if payload.is_null() && payload_size != 0 {
            return false;
        }
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if !matches!(
            *state,
            ContinuationState::Sleeping | ContinuationState::Ready
        ) {
            return false;
        }
        let expected_size = self.suspend_result_size();
        if expected_size != payload_size {
            return false;
        }
        if expected_size != 0 {
            let destination = self.suspend_result_pointer();
            if destination.is_null() {
                return false;
            }
            // SAFETY: the provider promises a live, non-overlapping payload
            // buffer of the exact ABI size advertised by the continuation.
            unsafe { std::ptr::copy_nonoverlapping(payload, destination, expected_size) };
        }
        // A provider has copied every argument it needs before publishing its
        // result, so release this operation's owned arguments before a
        // machine entry re-arms the continuation for another operation.
        self.cleanup_storage(CONTINUATION_SUSPEND_ARGUMENT_STORAGE);
        self.release_suspend_arguments();
        if *state == ContinuationState::Sleeping {
            *state = ContinuationState::Ready;
        }
        self.inner
            .provider_cancel
            .store(std::ptr::null_mut(), Ordering::Release);
        drop(state);
        self.cancel_active_timer();
        self.inner.wake.notify_all();

        let machine_entry = self.inner.machine_entry.load(Ordering::Acquire);
        let callback = self.inner.resume_callback.load(Ordering::Acquire);
        if !machine_entry.is_null() || !callback.is_null() {
            self.inner.active_operation.store(0, Ordering::Release);
            self.enqueue_resume(handle);
        } else {
            self.release_scope_work();
        }
        true
    }

    fn bind_task_context(
        &self,
        context: *mut crate::runtime::task::TaskContext,
        handle: *mut Continuation,
    ) {
        if !context.is_null() {
            task_continuations()
                .lock()
                .expect("task continuation mutex")
                .insert(context as usize, self.clone());
        }
        self.inner
            .task_context
            .store(context as usize, Ordering::Release);
        if !context.is_null() {
            // SAFETY: the task context belongs to the enclosing TaskGroup and
            // remains live until the task has joined or been closed.
            unsafe {
                (*context)
                    .continuation
                    .store(handle as usize, Ordering::Release);
                // A normal handler may be entered after the task was
                // spawned. Pin the currently active lexical frame alongside
                // the continuation so a machine-entry resume reinstalls the
                // same dynamic handler chain.
                (*context).handler_frame = crate::runtime::handler::current();
            }
            // Bindings happen when the native boundary hands the activation
            // chain to the scheduler, before any of its machine entries run.
            // Push the resolved context down the chain so each activation's
            // task-context lookup stays O(1) however deep the suspend
            // recursion that created it.
            let children = self
                .inner
                .function_children
                .lock()
                .expect("function children mutex")
                .clone();
            for child in children {
                child.propagate_inherited_task_context(context as usize);
            }
        }
    }

    fn propagate_inherited_task_context(&self, context: usize) {
        if self.inner.task_context.load(Ordering::Acquire) != 0 {
            return;
        }
        if self
            .inner
            .inherited_task_context
            .swap(context, Ordering::AcqRel)
            == context
        {
            return;
        }
        let children = self
            .inner
            .function_children
            .lock()
            .expect("function children mutex")
            .clone();
        for child in children {
            child.propagate_inherited_task_context(context);
        }
    }

    fn unbind_task_context(
        &self,
        context: *mut crate::runtime::task::TaskContext,
        handle: *mut Continuation,
    ) {
        let _ = self.inner.task_context.compare_exchange(
            context as usize,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if !context.is_null() {
            let mut continuations = task_continuations()
                .lock()
                .expect("task continuation mutex");
            if continuations
                .get(&(context as usize))
                .is_some_and(|bound| Arc::ptr_eq(&bound.inner, &self.inner))
            {
                continuations.remove(&(context as usize));
            }
            // SAFETY: the context remains owned by the current task until its
            // invocation returns.
            let _ = unsafe {
                (*context).continuation.compare_exchange(
                    handle as usize,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
            };
        }
    }

    fn finish_cancelled(&self) {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state != ContinuationState::Resuming {
            return;
        }
        *state = ContinuationState::Cancelled;
        self.inner.active_operation.store(0, Ordering::Release);
        self.inner
            .provider_cancel
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner
            .resume_callback
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner
            .machine_entry
            .store(std::ptr::null_mut(), Ordering::Release);
        drop(state);
        self.cancel_cown_acquisition();
        self.cleanup_owned_storage(true);
        self.release_frame();
        self.release_result();
        self.release_suspend_result();
        self.release_failure();
        self.release_suspend_arguments();
        self.release_spill();
        let task_context = self.inner.task_context.swap(0, Ordering::AcqRel);
        if task_context != 0 {
            let context = task_context as *mut crate::runtime::task::TaskContext;
            self.unbind_task_context(
                context,
                self.inner.handle.load(Ordering::Acquire) as *mut Continuation,
            );
            // SAFETY: TaskGroup retains the context through join/close.
            unsafe { (*context).continuation.store(0, Ordering::Release) };
            // SAFETY: the cancellation terminal path owns this context.
            unsafe {
                (*context)
                    .continuation_pending
                    .store(false, Ordering::Release)
            };
            unsafe { crate::runtime::task::cleanup_task_cown_leases(context) };
            // SAFETY: TaskGroup retains the context through join/close.
            unsafe { crate::runtime::task::cancel_task_context(context) };
            crate::runtime::task::complete_suspended_task(context);
        }
        let parent = self.inner.function_parent.swap(0, Ordering::AcqRel);
        self.detach_function_parent(parent);
        self.release_scope_work();
        self.inner.wake.notify_all();
    }

    fn state(&self) -> ContinuationState {
        *self.inner.state.lock().expect("continuation state mutex")
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancellation_requested.load(Ordering::Acquire) != 0
            || self.state() == ContinuationState::Cancelled
    }

    pub(crate) fn scope_id(&self) -> ScopeId {
        self.inner.scope.id()
    }

    pub(crate) fn enter_scope(&self) -> scope::ScopeGuard {
        self.inner.scope.enter()
    }

    pub(crate) fn is_suspend_pending(&self, operation: u64) -> bool {
        self.inner.active_operation.load(Ordering::Acquire) == operation.wrapping_add(1)
            && self.state() == ContinuationState::Sleeping
    }

    fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn wait_for_timers(&self) {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        while self.inner.timer_active.load(Ordering::Acquire) != 0 {
            state = self
                .inner
                .wake
                .wait(state)
                .expect("continuation state mutex");
        }
    }

    fn wait_for_idle(&self) {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        while *state == ContinuationState::Resuming
            || self.inner.native_operation_active.load(Ordering::Acquire)
            || self.inner.timer_active.load(Ordering::Acquire) != 0
            || self.inner.callbacks_in_flight.load(Ordering::Acquire) != 0
            || self.inner.cancellation_draining.load(Ordering::Acquire) != 0
        {
            let (next, timed_out) = self
                .inner
                .wake
                .wait_timeout(state, std::time::Duration::from_millis(1))
                .map(|(guard, timeout)| (guard, timeout.timed_out()))
                .expect("continuation state mutex");
            state = next;
            if timed_out {
                // The progress this continuation waits for is usually
                // delivered through a queued scheduler job. A worker that
                // simply blocks here starves that job, so lend the thread to
                // the queue; `help_one` re-enters nothing on this frame and
                // refuses nested help on its own.
                drop(state);
                if self.inner.scope.help_callback() {
                    // Callback scopes have an independent queue, including
                    // when cancellation is draining on a foreign thread.
                } else if crate::runtime::task::on_worker_thread() {
                    crate::runtime::task::help_current_worker();
                } else {
                    std::thread::yield_now();
                }
                state = self.inner.state.lock().expect("continuation state mutex");
            }
        }
    }

    fn in_current_callback(&self) -> bool {
        CURRENT_CALLBACK_INNER.with(|current| current.get() == Arc::as_ptr(&self.inner) as usize)
    }

    /// Retire the public continuation token once no executor callback
    /// can still observe its raw handle.  The CAS makes concurrent free calls
    /// harmless, while the lifecycle gate prevents a callback from being
    /// admitted after the handle has been retired.
    fn finish_free(&self) {
        let handle = self.inner.owner_handle.load(Ordering::Acquire) as *mut Continuation;
        if handle.is_null() {
            return;
        }
        let _gate = self
            .inner
            .lifecycle_gate
            .lock()
            .expect("continuation lifecycle mutex");
        if self
            .inner
            .task_wait_group
            .lock()
            .expect("task wait group mutex")
            .is_some()
            || !self
                .inner
                .function_children
                .lock()
                .expect("function children mutex")
                .is_empty()
            || self.inner.callbacks_in_flight.load(Ordering::Acquire) != 0
            || self.inner.cancellation_draining.load(Ordering::Acquire) != 0
            || self.inner.native_operation_active.load(Ordering::Acquire)
            || self.inner.timer_active.load(Ordering::Acquire) != 0
            || *self.inner.state.lock().expect("continuation state mutex")
                == ContinuationState::Resuming
        {
            return;
        }
        if self
            .inner
            .handle_released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Remove the public key before releasing any backing storage. A late
        // provider callback can therefore only observe a missing handle and
        // cannot race the cleanup of the continuation-owned buffers.
        unregister_handle(handle);
        drop(_gate);

        self.inner
            .resume_callback
            .store(std::ptr::null_mut(), Ordering::Release);
        self.inner
            .machine_entry
            .store(std::ptr::null_mut(), Ordering::Release);
        self.release_frame();
        self.release_result();
        self.release_suspend_result();
        self.release_failure();
        self.release_suspend_arguments();
        self.release_spill();
        // Registry ownership has been released. Existing callback/provider
        // Arc leases keep the state alive until their final references drop.
    }
}

#[cfg(test)]
mod tests {
    #[path = "dispatch.rs"]
    mod dispatch;
    #[path = "function_calls.rs"]
    mod function_calls;
    #[path = "lifecycle.rs"]
    mod lifecycle;
    #[path = "payload.rs"]
    mod payload;
    #[path = "provider.rs"]
    mod provider;
    #[path = "storage.rs"]
    mod storage;
    #[path = "support.rs"]
    mod support;
}
