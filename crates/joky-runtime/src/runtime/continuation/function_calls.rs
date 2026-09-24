use super::*;
use crate::runtime::abi::FunctionCallStatus;
use std::cell::RefCell;

thread_local! {
    /// Only the outer native boundary publishes. Nested generated calls may
    /// return Pending while their callers still have active native frames.
    static NATIVE_FUNCTION_PENDING: RefCell<Option<Vec<NativePending>>> = const { RefCell::new(None) };
    static TASK_FUNCTION_STATUS: Cell<Option<u8>> = const { Cell::new(None) };
}

#[no_mangle]
pub(crate) extern "C" fn jk_task_function_status(status: u8) {
    TASK_FUNCTION_STATUS.with(|current| current.set(Some(status)));
}

/// A worker may run a child task while helping a blocked parent. Its native
/// handoff is independent of the parent's still-active boundary.
pub(crate) unsafe fn with_task_function_pending_boundary(
    context: *mut crate::runtime::task::TaskContext,
    invoke: impl FnOnce(),
) -> bool {
    struct Restore {
        pending: Option<Vec<NativePending>>,
        status: Option<u8>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            NATIVE_FUNCTION_PENDING.with(|pending| *pending.borrow_mut() = self.pending.take());
            TASK_FUNCTION_STATUS.with(|status| status.set(self.status));
        }
    }
    let _restore = Restore {
        pending: NATIVE_FUNCTION_PENDING.with(|pending| pending.borrow_mut().take()),
        status: TASK_FUNCTION_STATUS.with(|status| status.take()),
    };
    let mut handed_off = false;
    let status = with_function_pending_boundary(|| {
        invoke();
        match TASK_FUNCTION_STATUS.with(Cell::get) {
            Some(1) => {
                let root = NATIVE_FUNCTION_PENDING.with(|pending| {
                    pending.borrow().as_ref().and_then(|entries| {
                        entries.iter().find_map(|entry| {
                            let value = unsafe { retain_handle(entry.handle()) }?;
                            (value.inner.function_parent.load(Ordering::Acquire) == 0)
                                .then_some((entry.handle(), value))
                        })
                    })
                });
                let Some((handle, root)) = root else {
                    return FunctionCallStatus::Failed;
                };
                let state = root.inner.state.lock().expect("continuation state mutex");
                let cancelled = crate::runtime::task::task_context_is_cancelled(context)
                    || root.inner.cancellation_requested.load(Ordering::Acquire) != 0;
                if matches!(
                    *state,
                    ContinuationState::Cancelled | ContinuationState::Completed
                ) || (cancelled && !root.has_pending_task_wait())
                {
                    return FunctionCallStatus::Cancelled;
                }
                root.bind_task_context(context, handle);
                // The invocation restored its caller's TLS before returning.
                // Use the chain captured by the activation, not that TLS.
                unsafe {
                    (*context).handler_frame = root.inner.handler_frame.load(Ordering::Acquire)
                        as *const crate::runtime::handler::HandlerFrame;
                    (*context)
                        .continuation_pending
                        .store(true, Ordering::Release);
                }
                handed_off = true;
                drop(state);
                // Even cancellation before handoff must retain the task until
                // internal child waits have drained. Bind first so the root's
                // eventual cancellation publishes the task's terminal state.
                if crate::runtime::task::task_context_is_cancelled(context)
                    || root.inner.cancellation_requested.load(Ordering::Acquire) != 0
                {
                    root.cancel();
                }
                FunctionCallStatus::Pending
            }
            Some(3) => FunctionCallStatus::Cancelled,
            Some(2) => FunctionCallStatus::Failed,
            _ => FunctionCallStatus::Ready,
        }
    });
    if !handed_off
        && matches!(
            status,
            FunctionCallStatus::Failed | FunctionCallStatus::Cancelled
        )
    {
        unsafe { crate::runtime::task::cancel_task_context(context) };
    }
    handed_off
}

#[derive(Clone, Copy)]
enum NativePending {
    Function(usize),
    Operation(usize),
}

impl NativePending {
    fn handle(self) -> *mut Continuation {
        let (Self::Function(handle) | Self::Operation(handle)) = self;
        handle as *mut Continuation
    }

    fn publish(self) -> bool {
        match self {
            Self::Function(_) => unsafe {
                jk_continuation_publish_function_pending(self.handle()) != 0
            },
            Self::Operation(_) => unsafe { retain_handle(self.handle()) }
                .is_some_and(|continuation| continuation.publish_pending_operation(self.handle())),
        }
    }
}

/// Invoke a Pending-ABI root or resume thunk and hand its activations to the
/// scheduler only after the thunk returns. Unwinding cancels unpublished work.
pub(crate) fn with_function_pending_boundary(
    invoke: impl FnOnce() -> FunctionCallStatus,
) -> FunctionCallStatus {
    let _region_guard = crate::runtime::region::install(crate::runtime::region::current());
    let outermost = NATIVE_FUNCTION_PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.is_some() {
            false
        } else {
            *pending = Some(Vec::new());
            true
        }
    });
    let mut boundary = NativeFunctionBoundary { outermost };
    let status = invoke();
    if !outermost {
        return status;
    }
    let pending = NATIVE_FUNCTION_PENDING.with(|pending| pending.borrow_mut().take().unwrap());
    boundary.outermost = false;
    if pending.is_empty() && status == FunctionCallStatus::Pending {
        return FunctionCallStatus::Failed;
    }
    // Native execution has unwound: every deferred activation now belongs
    // to the runtime, including cancellation paths that never dispatch.
    for activation in &pending {
        if let Some(value) = unsafe { retain_handle(activation.handle()) } {
            value.inner.runtime_owned.store(true, Ordering::Release);
        }
    }
    if status != FunctionCallStatus::Pending {
        let unexpected_pending = !pending.is_empty() && status == FunctionCallStatus::Ready;
        cancel_unpublished(pending);
        return if unexpected_pending {
            FunctionCallStatus::Failed
        } else {
            status
        };
    }
    for &activation in &pending {
        // Retain through the public registry: freeing/cancelling an activation
        // before handoff must not resurrect its frame or stale machine entry.
        if !activation.publish() {
            cancel_unpublished(pending);
            return FunctionCallStatus::Cancelled;
        }
    }
    status
}

struct NativeFunctionBoundary {
    outermost: bool,
}

/// A C callback may wait inside an outer Joky -> C call. Publish its own
/// activations independently, after its Joky thunk has left the native stack.
pub(crate) fn with_callback_pending_boundary(
    invoke: impl FnOnce() -> FunctionCallStatus,
) -> FunctionCallStatus {
    struct Restore(Option<Vec<NativePending>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            NATIVE_FUNCTION_PENDING.with(|pending| *pending.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(NATIVE_FUNCTION_PENDING.with(|pending| pending.borrow_mut().take()));
    with_function_pending_boundary(invoke)
}

/// Machine entries return void; a new operation or function poll records its
/// handoff in this boundary. Publish it only after the whole entry has exited.
pub(super) fn with_function_pending_resume_boundary(invoke: impl FnOnce()) {
    struct Restore(Option<Vec<NativePending>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            NATIVE_FUNCTION_PENDING.with(|pending| *pending.borrow_mut() = self.0.take());
        }
    }
    // A worker helping a join can execute an unrelated continuation while
    // another entry's native boundary is still live on the same thread.
    let _restore = Restore(NATIVE_FUNCTION_PENDING.with(|pending| pending.borrow_mut().take()));
    with_function_pending_boundary(|| {
        invoke();
        if NATIVE_FUNCTION_PENDING.with(|pending| {
            pending
                .borrow()
                .as_ref()
                .is_some_and(|entries| !entries.is_empty())
        }) {
            FunctionCallStatus::Pending
        } else {
            FunctionCallStatus::Ready
        }
    });
}

impl Drop for NativeFunctionBoundary {
    fn drop(&mut self) {
        if self.outermost {
            let pending = NATIVE_FUNCTION_PENDING
                .with(|pending| pending.borrow_mut().take().unwrap_or_default());
            cancel_unpublished(pending);
        }
    }
}

fn cancel_unpublished(pending: Vec<NativePending>) {
    for activation in pending {
        if let Some(value) = unsafe { retain_handle(activation.handle()) } {
            value.inner.runtime_owned.store(true, Ordering::Release);
            if value.internal_wait_active() {
                // This cancellation runs after the native boundary unwound;
                // an unpublished wait may now drain and release its frame.
                *value
                    .inner
                    .operation_handoff
                    .lock()
                    .expect("operation handoff mutex") = OperationHandoff::Inactive;
            }
            value.cancel();
        }
    }
}

pub(super) fn defer_function_pending(handle: *mut Continuation) {
    NATIVE_FUNCTION_PENDING.with(|pending| {
        if let Some(pending) = pending.borrow_mut().as_mut() {
            pending.push(NativePending::Function(handle as usize));
        }
    });
}

pub(super) fn defer_pending_operation(handle: *mut Continuation) {
    NATIVE_FUNCTION_PENDING.with(|pending| {
        pending
            .borrow_mut()
            .as_mut()
            .expect("Pending operation native boundary")
            .push(NativePending::Operation(handle as usize));
    });
}

pub(super) fn function_handoff_allowed() -> bool {
    NATIVE_FUNCTION_PENDING.with(|pending| pending.borrow().is_none())
}

// Always lock the continuation state before this phase. Provider completion
// and cancellation then serialize both the result copy and its cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FunctionCallPhase {
    /// The handle has not been used for a function call.
    Inactive,
    /// The callee's native invocation is still active.
    Calling,
    /// Completion arrived before the caller polled its result.
    CompletedInline,
    /// Poll returned Pending, but the native chain has not handed off yet.
    ReturnedPending,
    /// Completion arrived after poll and must wait for the handoff.
    CompletedPending,
    /// The native chain has returned; completion may enqueue a resume.
    Armed,
    /// Either poll consumed Ready or exactly one resume was enqueued.
    Delivered,
    /// The caller atomically took ownership of the result payload.
    Claimed,
}

impl Continuation {
    fn has_pending_task_wait(&self) -> bool {
        let mut activations = vec![self.clone()];
        while let Some(activation) = activations.pop() {
            if activation
                .inner
                .task_wait_group
                .lock()
                .expect("task wait group mutex")
                .is_some()
            {
                return true;
            }
            activations.extend(
                activation
                    .inner
                    .function_children
                    .lock()
                    .expect("function children mutex")
                    .iter()
                    .cloned(),
            );
        }
        false
    }

    pub(super) fn detach_function_parent(&self, parent: usize) {
        if parent == 0 {
            return;
        }
        // Internal ownership links outlive a public free request: the parent
        // cannot release its frame until this child has finished draining.
        let Some(parent) = registries::retain_owner(parent) else {
            return;
        };
        parent
            .inner
            .function_children
            .lock()
            .expect("function children mutex")
            .retain(|child| !Arc::ptr_eq(&child.inner, &self.inner));
        if parent.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
            let scope = Arc::clone(&parent.inner.scope);
            scope.enqueue_resume(Box::new(move || {
                parent.cancel();
                if parent.inner.free_requested.load(Ordering::Acquire) {
                    parent.finish_free();
                }
            }));
        } else if parent.inner.free_requested.load(Ordering::Acquire) {
            // A normal completion may request public-handle release while its
            // last child is still detaching. This transition is the final
            // opportunity to retry the deferred release.
            parent.finish_free();
        }
    }

    pub(super) fn attach_function_parent(
        &self,
        handle: *mut Continuation,
        parent: *mut Continuation,
    ) -> bool {
        if parent.is_null() {
            return true;
        }
        if handle == parent {
            return false;
        }
        let Some(owner) = (unsafe { retain_handle(parent) }) else {
            return false;
        };
        let mut children = owner
            .inner
            .function_children
            .lock()
            .expect("function children mutex");
        if owner.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
            return false;
        }
        if !children
            .iter()
            .any(|child| Arc::ptr_eq(&child.inner, &self.inner))
        {
            children.push(self.clone());
        }
        self.inner
            .inherited_task_context
            .store(owner.function_task_context(), Ordering::Release);
        self.inner
            .function_parent
            .store(parent as usize, Ordering::Release);
        true
    }

    pub(super) fn function_task_context(&self) -> usize {
        let context = self.inner.task_context.load(Ordering::Acquire);
        if context != 0 {
            return context;
        }
        // Suspended recursion builds parent chains as deep as the recursion
        // itself; activations inherit the enclosing task's context when they
        // begin, so lookups stay O(1) instead of walking that chain.
        self.inner.inherited_task_context.load(Ordering::Acquire)
    }

    /// Reserve one call activation without rebinding the surrounding task.
    /// The caller must initialize its frame/result layouts before invoking the
    /// callee. Each activation uses a fresh handle, including calls in loops.
    pub(super) fn begin_function_pending(&self, handle: *mut Continuation) -> bool {
        self.begin_function_pending_with_parent(handle, std::ptr::null_mut())
    }

    pub(super) fn begin_function_pending_with_parent(
        &self,
        handle: *mut Continuation,
        parent: *mut Continuation,
    ) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        if !self.attach_function_parent(handle, parent) {
            return false;
        }
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if handle == parent
            || *state != ContinuationState::Ready
            || *phase != FunctionCallPhase::Inactive
            || self.inner.active_operation.load(Ordering::Acquire) != 0
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return false;
        }
        if !self.begin_scope_work() {
            return false;
        }
        self.remember_owner_handle(handle);
        self.inner.handle.store(handle as usize, Ordering::Release);
        self.inner
            .function_parent
            .store(parent as usize, Ordering::Release);
        self.inner.handler_frame.store(
            crate::runtime::handler::current() as usize,
            Ordering::Release,
        );
        *phase = FunctionCallPhase::Calling;
        *state = ContinuationState::Sleeping;
        true
    }

    /// Inspect completion after the callee's native invocation returns. A
    /// Pending result does not arm a callback: the enclosing call chain must
    /// finish unwinding before its scheduler boundary publishes the activation.
    pub(super) fn poll_function_pending(&self) -> FunctionCallStatus {
        self.poll_function_pending_mode(false)
    }

    pub(super) fn poll_function_pending_resume(&self) -> FunctionCallStatus {
        self.poll_function_pending_mode(true)
    }

    fn poll_function_pending_mode(&self, defer_ready: bool) -> FunctionCallStatus {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        if *state == ContinuationState::Cancelled
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return FunctionCallStatus::Cancelled;
        }
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        match *phase {
            FunctionCallPhase::CompletedInline if defer_ready => {
                *phase = FunctionCallPhase::CompletedPending;
                FunctionCallStatus::Pending
            }
            FunctionCallPhase::CompletedInline => {
                *phase = FunctionCallPhase::Delivered;
                *state = ContinuationState::Ready;
                self.release_scope_work();
                self.inner.wake.notify_all();
                if self.inner.failure_pending.load(Ordering::Acquire) {
                    FunctionCallStatus::Failed
                } else {
                    FunctionCallStatus::Ready
                }
            }
            FunctionCallPhase::Calling => {
                *phase = FunctionCallPhase::ReturnedPending;
                FunctionCallStatus::Pending
            }
            _ => FunctionCallStatus::Failed,
        }
    }

    /// The scheduler calls this only after the entire native invocation has
    /// returned Pending. Completion racing with this handoff queues exactly
    /// one resume. No generated caller may access its frame after handoff.
    pub(super) fn publish_function_pending(&self, handle: *mut Continuation) -> bool {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if *state != ContinuationState::Sleeping
            || (self.inner.machine_entry.load(Ordering::Acquire).is_null()
                && self.inner.resume_callback.load(Ordering::Acquire).is_null())
        {
            return false;
        }
        let dispatch = match *phase {
            FunctionCallPhase::ReturnedPending => {
                *phase = FunctionCallPhase::Armed;
                false
            }
            FunctionCallPhase::CompletedPending => {
                *phase = FunctionCallPhase::Delivered;
                *state = ContinuationState::Ready;
                true
            }
            _ => return false,
        };
        drop(phase);
        drop(state);
        if dispatch {
            self.enqueue_resume(handle);
        }
        true
    }

    /// Copy a callee's result while holding the cancellation lock. Success
    /// transfers payload ownership into the caller's suspend-result slot;
    /// rejection leaves ownership with the callee. The enclosing function's
    /// final-result slot is separate and is never overwritten here.
    pub(super) unsafe fn complete_function_pending(
        &self,
        handle: *mut Continuation,
        payload: *const u8,
        payload_size: usize,
    ) -> bool {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if *state != ContinuationState::Sleeping
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
            || !matches!(
                *phase,
                FunctionCallPhase::Calling
                    | FunctionCallPhase::ReturnedPending
                    | FunctionCallPhase::Armed
            )
            || payload_size != self.suspend_result_size()
            || (payload_size != 0 && (payload.is_null() || self.suspend_result_pointer().is_null()))
        {
            return false;
        }
        if payload_size != 0 {
            // SAFETY: the ABI caller supplies a readable, non-overlapping
            // buffer. The state lock keeps the destination live until copied.
            unsafe {
                std::ptr::copy_nonoverlapping(payload, self.suspend_result_pointer(), payload_size);
            }
        }
        let dispatch = match *phase {
            FunctionCallPhase::Calling => {
                *phase = FunctionCallPhase::CompletedInline;
                false
            }
            FunctionCallPhase::ReturnedPending => {
                *phase = FunctionCallPhase::CompletedPending;
                false
            }
            FunctionCallPhase::Armed => {
                *phase = FunctionCallPhase::Delivered;
                *state = ContinuationState::Ready;
                true
            }
            _ => unreachable!(),
        };
        drop(phase);
        drop(state);
        if dispatch {
            self.enqueue_resume(handle);
        }
        true
    }

    pub(super) unsafe fn complete_function_pending_failure(
        &self,
        handle: *mut Continuation,
        _operation: u64,
        payload: *const u8,
        payload_size: usize,
    ) -> bool {
        let mut state = self.inner.state.lock().expect("continuation state mutex");
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if *state != ContinuationState::Sleeping
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
            || !matches!(
                *phase,
                FunctionCallPhase::Calling
                    | FunctionCallPhase::ReturnedPending
                    | FunctionCallPhase::Armed
            )
            || payload_size != self.failure_size()
            || (payload_size != 0 && (payload.is_null() || self.failure_pointer().is_null()))
        {
            return false;
        }
        if payload_size != 0 {
            std::ptr::copy_nonoverlapping(payload, self.failure_pointer(), payload_size);
        }
        self.inner.failure_pending.store(true, Ordering::Release);
        let dispatch = match *phase {
            FunctionCallPhase::Calling => {
                *phase = FunctionCallPhase::CompletedInline;
                false
            }
            FunctionCallPhase::ReturnedPending => {
                *phase = FunctionCallPhase::CompletedPending;
                false
            }
            FunctionCallPhase::Armed => {
                *phase = FunctionCallPhase::Delivered;
                *state = ContinuationState::Ready;
                self.release_scope_work();
                true
            }
            _ => unreachable!(),
        };
        drop(phase);
        drop(state);
        if dispatch {
            self.enqueue_resume(handle);
        }
        true
    }

    /// Move a delivered result into caller-owned storage. Poll alone cannot
    /// protect a later raw load from cancellation; copying and clearing the
    /// source under the state lock makes the ownership decision indivisible.
    pub(super) unsafe fn take_function_pending_result(
        &self,
        output: *mut u8,
        size: usize,
    ) -> FunctionCallStatus {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if *state == ContinuationState::Cancelled
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return FunctionCallStatus::Cancelled;
        }
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if !matches!(
            *state,
            ContinuationState::Ready | ContinuationState::Resuming
        ) || *phase != FunctionCallPhase::Delivered
            || self.inner.failure_pending.load(Ordering::Acquire)
            || size != self.suspend_result_size()
            || (size != 0 && (output.is_null() || self.suspend_result_pointer().is_null()))
        {
            return FunctionCallStatus::Failed;
        }
        if size != 0 {
            // SAFETY: the ABI caller supplies a writable, non-overlapping
            // output buffer. Cancellation cannot reclaim the source here.
            unsafe {
                std::ptr::copy_nonoverlapping(self.suspend_result_pointer(), output, size);
                std::ptr::write_bytes(self.suspend_result_pointer(), 0, size);
            }
        }
        *phase = FunctionCallPhase::Claimed;
        FunctionCallStatus::Ready
    }

    pub(super) unsafe fn take_function_pending_failure(
        &self,
        output: *mut u8,
        size: usize,
    ) -> FunctionCallStatus {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if *state == ContinuationState::Cancelled
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            return FunctionCallStatus::Cancelled;
        }
        let mut phase = self
            .inner
            .function_call
            .lock()
            .expect("function call mutex");
        if !matches!(
            *state,
            ContinuationState::Ready | ContinuationState::Resuming
        ) || *phase != FunctionCallPhase::Delivered
            || !self.inner.failure_pending.load(Ordering::Acquire)
            || size != self.failure_size()
            || (size != 0 && (output.is_null() || self.failure_pointer().is_null()))
        {
            return FunctionCallStatus::Failed;
        }
        if size != 0 {
            std::ptr::copy_nonoverlapping(self.failure_pointer(), output, size);
            std::ptr::write_bytes(self.failure_pointer(), 0, size);
        }
        self.inner.failure_pending.store(false, Ordering::Release);
        *phase = FunctionCallPhase::Claimed;
        FunctionCallStatus::Failed
    }
}
