use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

use super::scheduler::{current_worker_thread, task_scheduler};
use super::*;

/// Small batches need no allocation; the overflow capacity is reused across whens.
#[derive(Default)]
pub(super) struct CownLeases {
    inline: [usize; 4],
    len: usize,
    overflow: Vec<usize>,
}

impl CownLeases {
    fn push(&mut self, value: usize) {
        if self.len < self.inline.len() {
            self.inline[self.len] = value;
        } else {
            self.overflow.push(value);
        }
        self.len += 1;
    }
    fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(if self.len < self.inline.len() {
            self.inline[self.len]
        } else {
            self.overflow.pop().unwrap()
        })
    }
    fn remove(&mut self, value: usize) -> bool {
        let index = self.inline[..self.len.min(4)]
            .iter()
            .chain(self.overflow.iter())
            .position(|&entry| entry == value);
        let Some(index) = index else {
            return false;
        };
        let last = self.pop().unwrap();
        if index < self.len {
            if index < 4 {
                self.inline[index] = last;
            } else {
                self.overflow[index - 4] = last;
            }
        }
        true
    }
}

pub(crate) fn has_current_task_context() -> bool {
    CURRENT_TASK_CONTEXT.with(|current| !current.get().is_null())
}

pub(crate) fn enqueue_scheduler_callback(callback: Box<dyn FnOnce() + Send + 'static>) {
    task_scheduler().enqueue_callback(callback);
}

/// Let the current worker execute one unrelated queued task while a runtime
/// primitive waits for progress. This keeps a Cown contention point from
/// monopolizing a worker and is intentionally a no-op on non-worker threads.
pub(crate) fn help_current_worker() -> bool {
    task_scheduler().help_one()
}

/// Whether this thread is one of the shared scheduler workers.
pub(crate) fn on_worker_thread() -> bool {
    current_worker_thread()
}

pub(crate) fn current_task_context() -> Option<*mut TaskContext> {
    CURRENT_TASK_CONTEXT.with(|current| {
        let context = current.get();
        (!context.is_null()).then_some(context)
    })
}

fn task_wake(context: &TaskContext) -> Option<&super::scheduler::TaskWake> {
    // TaskGroup owns this state through task execution and terminal cleanup.
    unsafe { context.wake.as_ref() }
}

pub(crate) fn register_cown_leases(context: *mut TaskContext, cowns: &[*mut u8]) {
    let Some(wake) = unsafe { context.as_ref() }.and_then(task_wake) else {
        return;
    };
    let mut leases = wake.cown_leases.lock().expect("task Cown leases");
    for &cown in cowns {
        leases.push(cown as usize);
    }
}

pub(crate) fn unregister_cown_lease(context: *mut TaskContext, cown: *mut u8) -> bool {
    unsafe { context.as_ref() }
        .and_then(task_wake)
        .is_some_and(|wake| {
            wake.cown_leases
                .lock()
                .expect("task Cown leases")
                .remove(cown as usize)
        })
}

/// Terminal cleanup runs only after native/continuation execution has drained.
pub(crate) unsafe fn cleanup_task_cown_leases(context: *mut TaskContext) {
    let Some(wake) = unsafe { context.as_ref() }.and_then(task_wake) else {
        return;
    };
    let mut entries = std::mem::take(&mut *wake.cown_leases.lock().expect("task Cown leases"));
    while let Some(cown) = entries.pop() {
        crate::runtime::managed::jk_cown_release_cleanup(cown as *mut u8);
    }
}

pub(crate) struct CownWaitRegistration(Option<*mut TaskContext>);

impl Drop for CownWaitRegistration {
    fn drop(&mut self) {
        if let Some(wake) = self
            .0
            .and_then(|context| unsafe { context.as_ref() })
            .and_then(task_wake)
        {
            *wake.cown_waiter.lock().expect("task Cown waiter") = Weak::new();
        }
    }
}

pub(crate) fn register_cown_waiter(
    waiter: &Arc<crate::runtime::cown::Waiter>,
) -> CownWaitRegistration {
    let context = current_task_context();
    if let Some(wake) = context
        .and_then(|context| unsafe { context.as_ref() })
        .and_then(task_wake)
    {
        *wake.cown_waiter.lock().expect("task Cown waiter") = Arc::downgrade(waiter);
        // Cancellation before publication is observed here; cancellation after
        // publication finds the waiter under the same per-task mutex.
        if context.is_some_and(task_context_is_cancelled) {
            waiter.notify();
        }
    }
    CownWaitRegistration(context)
}

pub(crate) use super::scheduler::register_cown_helper;

/// Install a suspended task's dynamic context while its continuation machine
/// entry runs on a scheduler thread. Initial task thunks use
/// [`TaskInvocation::invoke`]; this is the matching stackless-resume path.
pub(crate) unsafe fn with_task_context<R>(context: *mut TaskContext, f: impl FnOnce() -> R) -> R {
    let previous_cancellation = CURRENT_CANCELLATION.with(|current| {
        let previous = current.get();
        current.set(if context.is_null() {
            std::ptr::null()
        } else {
            // SAFETY: the continuation retains this task context until it
            // completes or is cancelled.
            unsafe { (*context).cancellation }
        });
        previous
    });
    let previous_context = CURRENT_TASK_CONTEXT.with(|current| {
        let previous = current.get();
        current.set(context);
        previous
    });
    let previous_control = CURRENT_TASK_CONTROL.with(|current| {
        if context.is_null() {
            current.replace(std::ptr::null())
        } else {
            current.get()
        }
    });
    let previous_handler = unsafe {
        crate::runtime::handler::install(if context.is_null() {
            std::ptr::null()
        } else {
            (*context).handler_frame
        })
    };
    let result = f();
    unsafe { crate::runtime::handler::install(previous_handler) };
    CURRENT_TASK_CONTEXT.with(|current| current.set(previous_context));
    CURRENT_TASK_CONTROL.with(|current| current.set(previous_control));
    CURRENT_CANCELLATION.with(|current| current.set(previous_cancellation));
    result
}

pub(crate) fn mark_current_task_continuation_pending() {
    CURRENT_TASK_CONTEXT.with(|current| {
        let context = current.get();
        if !context.is_null() {
            // Cancellation can win between publishing a continuation and the
            // generated thunk returning from the runtime call. Do not revive
            // a task that the continuation has already completed terminally.
            if !task_context_is_cancelled(context) {
                // SAFETY: the task thunk owns this context for the duration
                // of the suspended invocation.
                unsafe {
                    (*context)
                        .continuation_pending
                        .store(true, Ordering::Release)
                };
            }
        }
    });
    CURRENT_TASK_CONTROL.with(|current| {
        let control = current.get();
        if !control.is_null()
            && !CURRENT_TASK_CONTEXT.with(|context| task_context_is_cancelled(context.get()))
        {
            // Keep joins blocked while the continuation owns completion.
            unsafe { (&*control).set_sleeping() };
        }
    });
}

pub(crate) unsafe fn cancel_task_context(context: *mut TaskContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: the task control and context remain owned by the TaskGroup.
    unsafe {
        if !(*context).cancellation.is_null() {
            (*(*context).cancellation).store(true, Ordering::Release);
        }
        (*context).cancelled_result = true;
    }
}

pub(crate) fn task_context_is_cancelled(context: *mut TaskContext) -> bool {
    if context.is_null() {
        return false;
    }
    // SAFETY: the task group keeps the context and its cancellation pointer
    // alive until the task is joined or closed.
    unsafe {
        let cancellation = (*context).cancellation;
        !cancellation.is_null() && (*cancellation).load(Ordering::Acquire)
    }
}

pub(crate) fn complete_suspended_task(context: *mut TaskContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: continuation frame storage keeps the task context alive until
    // its enclosing scope joins or closes the task.
    let context_ref = unsafe { &*context };
    if context_ref.group.is_null() {
        return;
    }
    // Keep the group alive through terminal publication and waiter delivery.
    // Publishing the terminal TaskState lets another worker resume the parent
    // and drop its TaskGroup immediately, before finish_external returns.
    let task = context_ref.task;
    let group = context_ref.group;
    // SAFETY: the task is not terminal yet, so its parent still owns this Arc.
    let group = unsafe {
        std::sync::Arc::increment_strong_count(group);
        std::sync::Arc::from_raw(group)
    };
    group.finish_external(task);
}

pub(super) fn abort_current_task(failure: TaskFailure) {
    CURRENT_TASK_CONTEXT.with(|current| {
        let context = current.get();
        if context.is_null() {
            let scope = crate::runtime::scope::current_or_default();
            let mut root = scope.root_failure.lock().expect("root failure mutex");
            if root.is_none() {
                *root = Some(failure);
            }
            return;
        }
        // SAFETY: the task invocation owns this context, and its TaskGroup
        // keeps the inner control structure alive until all task threads exit.
        unsafe {
            // The payload is still transported through the task group so MIR
            // can bind it to typed handler locals. This lookup establishes
            // nearest-frame semantics for ordinary calls in the task.
            let _handler = crate::runtime::handler::nearest_operation(failure.operation);
            (*context).abort_operation = failure.operation;
            (*context).cancelled_result = true;
            if !(*context).cancellation.is_null() {
                (*(*context).cancellation).store(true, Ordering::Release);
            }
            let group = (*context).group;
            if !group.is_null() {
                let _ = (*group).record_abort((*context).task, failure);
            }
        }
    });
}

pub(crate) fn reset_root_failure() {
    crate::runtime::scope::current_or_default()
        .root_failure
        .lock()
        .expect("root failure mutex")
        .take();
}

pub(crate) fn take_root_failure_operation() -> Option<u64> {
    crate::runtime::scope::current_or_default()
        .root_failure
        .lock()
        .expect("root failure mutex")
        .take()
        .map(|failure| failure.operation)
}

/// Sleep the current task until a monotonic deadline. This is deliberately a
/// runtime parking primitive for the first `time.sleep` slice; a future
/// continuation scheduler can replace the wait without changing the MIR ABI.
pub(crate) fn sleep_current_task(milliseconds: u64) {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(milliseconds))
        .unwrap_or_else(Instant::now);
    CURRENT_TASK_CONTEXT.with(|current| {
        let context = current.get();
        if context.is_null() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining);
            return;
        }
        // SAFETY: the task context and wake state remain live for the entire
        // task invocation, and TaskGroup owns the wake state through control.
        let context = unsafe { &*context };
        let control = CURRENT_TASK_CONTROL.with(|current| current.get());
        if !control.is_null() {
            // SAFETY: TaskInvocation keeps the control Arc alive for the
            // duration of this invocation.
            unsafe { (&*control).set_sleeping() };
        }
        if context.cancellation.is_null() || context.wake.is_null() {
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
            if !control.is_null() {
                // SAFETY: see the matching set_sleeping call above.
                unsafe { (&*control).set_running() };
            }
            return;
        }
        let wake = unsafe { &*context.wake };
        let mut guard = wake.state.lock().expect("task wake mutex");
        while !unsafe { (*context.cancellation).load(Ordering::Acquire) } {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next_guard, timeout) = wake
                .signal
                .wait_timeout(guard, remaining)
                .expect("task wake mutex");
            guard = next_guard;
            if timeout.timed_out() {
                break;
            }
        }
        if !control.is_null() {
            // SAFETY: TaskInvocation keeps the control Arc alive for the
            // duration of this invocation.
            unsafe { (&*control).set_running() };
        }
    });
}
