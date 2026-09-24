use super::*;
use crate::runtime::abi::FunctionCallStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationHandoff {
    Inactive,
    Withheld,
    Completed,
    Published,
}

impl Continuation {
    pub(super) fn withhold_operation_resume(&self, parent: *mut Continuation) -> bool {
        *self.inner.region.lock().expect("continuation region") = crate::runtime::region::current();
        if function_calls::function_handoff_allowed() {
            return false;
        }
        let state = self.inner.state.lock().expect("continuation state mutex");
        let mut handoff = self
            .inner
            .operation_handoff
            .lock()
            .expect("operation handoff mutex");
        if !matches!(
            *state,
            ContinuationState::Ready | ContinuationState::Resuming
        ) || *handoff != OperationHandoff::Inactive
            || self.inner.cancellation_requested.load(Ordering::Acquire) != 0
            || (self.inner.machine_entry.load(Ordering::Acquire).is_null()
                && self.inner.resume_callback.load(Ordering::Acquire).is_null())
        {
            return false;
        }
        *handoff = OperationHandoff::Withheld;
        self.inner
            .native_operation_active
            .store(true, Ordering::Release);
        self.inner
            .function_parent
            .store(parent as usize, Ordering::Release);
        true
    }

    /// Called by completion before queue admission. A provider may complete
    /// inside its start hook; it still cannot resume an active native chain.
    pub(super) fn operation_resume_allowed(&self) -> bool {
        let mut handoff = self
            .inner
            .operation_handoff
            .lock()
            .expect("operation handoff mutex");
        match *handoff {
            OperationHandoff::Withheld => {
                *handoff = OperationHandoff::Completed;
                false
            }
            OperationHandoff::Completed => false,
            OperationHandoff::Published | OperationHandoff::Inactive => {
                *handoff = OperationHandoff::Inactive;
                true
            }
        }
    }

    pub(super) fn publish_pending_operation(&self, handle: *mut Continuation) -> bool {
        let state = self.inner.state.lock().expect("continuation state mutex");
        if !matches!(
            *state,
            ContinuationState::Sleeping | ContinuationState::Ready
        ) || (self.inner.cancellation_requested.load(Ordering::Acquire) != 0
            && !self.internal_wait_active())
        {
            return false;
        }
        let mut handoff = self
            .inner
            .operation_handoff
            .lock()
            .expect("operation handoff mutex");
        let dispatch = match *handoff {
            OperationHandoff::Withheld => {
                *handoff = OperationHandoff::Published;
                false
            }
            OperationHandoff::Completed => {
                *handoff = OperationHandoff::Inactive;
                true
            }
            _ => return false,
        };
        drop(handoff);
        drop(state);
        if self.inner.cown_acquire_active.load(Ordering::Acquire)
            && self.inner.cancellation_requested.load(Ordering::Acquire) != 0
        {
            // Unlike a task-group wait, a Cown wait has no child work to drain.
            // Once the native frame hands off, cancellation can remove it now.
            self.cancel();
            return true;
        }
        if dispatch {
            self.enqueue_resume(handle);
        }
        true
    }

    pub(super) fn finish_operation_start(
        &self,
        handle: *mut Continuation,
        accepted: bool,
    ) -> FunctionCallStatus {
        let state = self.inner.state.lock().expect("continuation state mutex");
        self.inner
            .native_operation_active
            .store(false, Ordering::Release);
        let cancelled = self.inner.cancellation_requested.load(Ordering::Acquire) != 0;
        drop(state);
        if cancelled {
            self.cancel();
            return FunctionCallStatus::Cancelled;
        }
        if accepted {
            function_calls::defer_pending_operation(handle);
            FunctionCallStatus::Pending
        } else {
            *self
                .inner
                .operation_handoff
                .lock()
                .expect("operation handoff mutex") = OperationHandoff::Inactive;
            if self.inner.cancellation_requested.load(Ordering::Acquire) != 0 {
                FunctionCallStatus::Cancelled
            } else {
                FunctionCallStatus::Failed
            }
        }
    }

    pub(super) fn start_pending_timer(
        &self,
        handle: *mut Continuation,
        operation: u64,
        milliseconds: u64,
        parent: *mut Continuation,
    ) -> FunctionCallStatus {
        if !self.attach_function_parent(handle, parent) {
            return FunctionCallStatus::Cancelled;
        }
        if !self.withhold_operation_resume(parent) {
            return FunctionCallStatus::Failed;
        }
        self.inner.task_wait_active.store(false, Ordering::Release);
        let accepted = if self.state() == ContinuationState::Resuming {
            self.inner
                .active_operation
                .store(operation.wrapping_add(1), Ordering::Release);
            self.resuspend_sleep(handle, milliseconds)
        } else {
            self.start_suspend(handle, operation, milliseconds, true)
        };
        self.finish_operation_start(handle, accepted)
    }

    pub(super) fn start_pending_provider(
        &self,
        handle: *mut Continuation,
        operation: u64,
        arguments: *const u8,
        size: usize,
        parent: *mut Continuation,
    ) -> FunctionCallStatus {
        if !self.attach_function_parent(handle, parent) {
            return FunctionCallStatus::Cancelled;
        }
        if (size != 0 && arguments.is_null()) || !self.withhold_operation_resume(parent) {
            return FunctionCallStatus::Failed;
        }
        if size != 0 && self.allocate_suspend_arguments(arguments, size).is_null() {
            return self.finish_operation_start(handle, false);
        }
        self.inner.task_wait_active.store(false, Ordering::Release);
        let accepted = if self.state() == ContinuationState::Resuming {
            self.resuspend_provider(handle, operation)
        } else {
            self.start_suspend_provider(handle, operation)
        };
        if !accepted {
            self.cleanup_storage(CONTINUATION_SUSPEND_ARGUMENT_STORAGE);
            self.release_suspend_arguments();
        }
        self.finish_operation_start(handle, accepted)
    }
}

/// Pending-ABI operation entries never wait or take over task completion.
/// Even inline provider completion is delivered after the native boundary.
#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_pending_timer(
    handle: *mut Continuation,
    operation: u64,
    milliseconds: u64,
    parent: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(handle) }.map_or(FunctionCallStatus::Cancelled as u8, |value| {
        value.start_pending_timer(handle, operation, milliseconds, parent) as u8
    })
}

#[no_mangle]
pub(crate) unsafe extern "C" fn jk_continuation_start_pending_provider(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    parent: *mut Continuation,
) -> u8 {
    unsafe { retain_handle(handle) }.map_or(FunctionCallStatus::Cancelled as u8, |value| {
        value.start_pending_provider(handle, operation, arguments, size, parent) as u8
    })
}
