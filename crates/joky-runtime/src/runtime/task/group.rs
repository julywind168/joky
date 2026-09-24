use super::*;
use crate::runtime::scope::RuntimeScope;
use std::sync::{Arc, Condvar, Mutex};

pub(crate) struct TaskGroupInner {
    pub(super) _resource: crate::runtime::resources::Lease,
    pub(super) scope: Arc<RuntimeScope>,
    pub(super) tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    pub(super) first_failure: Mutex<super::TaskFailureSlot>,
    pub(super) waiters: Mutex<Vec<crate::runtime::continuation::Continuation>>,
    pub(super) race_waiting: AtomicBool,
    pub(super) race_selected: AtomicBool,
    pub(super) completion_epoch: Mutex<u64>,
    pub(super) completion: Condvar,
    pub(super) close: Mutex<()>,
    pub(super) scope_work: Mutex<Option<super::scope::ScopeWork>>,
}

pub(super) struct TaskRecord {
    pub(super) control: Arc<TaskControl>,
    pub(super) context: *mut TaskContext,
}

// The generated-code contract keeps task context storage alive until the
// owning TaskGroup has joined or closed every task.
unsafe impl Send for TaskRecord {}
unsafe impl Sync for TaskRecord {}

impl TaskGroupInner {
    /// Cancel every task owned by this group. This is used by the runtime
    /// scope shutdown path before waiting for task completion.
    pub(crate) fn cancel_all(&self) {
        let tasks = self
            .tasks
            .lock()
            .expect("task map mutex")
            .values()
            .map(|task| {
                task.control.wake.cancel(&task.control.cancellation);
                crate::runtime::continuation::task_continuation(task.context)
            })
            .collect::<Vec<_>>();
        for continuation in tasks.into_iter().flatten() {
            // A suspended task no longer has a worker invocation. Route
            // cancellation through its continuation so scope shutdown does
            // not wait for an outstanding timer or provider request.
            continuation.cancel();
        }
    }

    /// Wait for every task and release unclaimed results. The close mutex
    /// makes normal lexical close and scope-triggered close idempotent.
    pub(super) fn close_finished_tasks(&self) {
        let _close = self.close.lock().expect("task group close mutex");
        // Keep the records visible while waiting: a child that fails during
        // close still needs to find and cancel its siblings. Clear the map
        // only after every task has reached a terminal state, making a later
        // `Drop` close a no-op without exposing stale contexts.
        let tasks = self
            .tasks
            .lock()
            .expect("task map mutex")
            .values()
            .map(|task| (task.context, Arc::clone(&task.control)))
            .collect::<Vec<_>>();
        for (_, task) in &tasks {
            if current_worker_thread() {
                task_scheduler().help_until(task);
            } else {
                task.wait();
            }
        }
        // Cancellation snapshots are acquired under this mutex. Remove the
        // records before freeing contexts, so an old address cannot resolve to
        // an unrelated task if the allocator reuses it during close.
        self.tasks.lock().expect("task map mutex").clear();
        for (context, _) in tasks {
            // SAFETY: close waits for the task before disposing an unclaimed
            // result. The context remains valid until the surrounding frame
            // returns from ScopeExit.
            unsafe { drop_task_result(context) };
            heap_task_storage()
                .lock()
                .expect("heap task storage mutex")
                .remove(&(context as usize));
        }
        self.clean_failure();
        self.scope_work
            .lock()
            .expect("task scope work mutex")
            .take();
    }

    pub(super) fn mark_completed(&self) {
        *self.completion_epoch.lock().expect("completion mutex") += 1;
        self.completion.notify_all();
        self.notify_waiters();
    }

    pub(crate) fn register_waiter(
        &self,
        waiter: crate::runtime::continuation::Continuation,
        race: bool,
    ) {
        if race {
            self.race_waiting.store(true, Ordering::Release);
        }
        self.waiters
            .lock()
            .expect("task waiters mutex")
            .push(waiter);
        // Registration and terminal publication both recheck after releasing
        // their locks, so a completion before registration cannot lose a wake.
        self.notify_waiters();
    }

    fn notify_waiters(&self) {
        if self.race_waiting.load(Ordering::Acquire) && !self.race_selected.load(Ordering::Acquire)
        {
            let cancellations = {
                let tasks = self.tasks.lock().expect("task map mutex");
                let winner = tasks.iter().find_map(|(id, task)| {
                    (*task.control.state.lock().expect("task state mutex") == TaskState::Completed)
                        .then_some(*id)
                });
                if let Some(winner) =
                    winner.filter(|_| !self.race_selected.swap(true, Ordering::AcqRel))
                {
                    tasks
                        .iter()
                        .filter(|(id, _)| **id != winner)
                        .filter_map(|(_, task)| {
                            task.control.wake.cancel(&task.control.cancellation);
                            crate::runtime::continuation::task_continuation(task.context)
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            };
            for continuation in cancellations {
                continuation.cancel();
            }
        }
        let ready = self
            .tasks
            .lock()
            .expect("task map mutex")
            .values()
            .all(|task| {
                matches!(
                    *task.control.state.lock().expect("task state mutex"),
                    TaskState::Completed | TaskState::Cancelled | TaskState::Aborted
                )
            });
        if ready {
            let waiters = std::mem::take(&mut *self.waiters.lock().expect("task waiters mutex"));
            for waiter in waiters {
                waiter.complete_task_wait();
            }
        }
    }

    fn clean_failure(&self) {
        let mut slot = self.first_failure.lock().expect("task failure mutex");
        let previous = std::mem::replace(&mut *slot, super::TaskFailureSlot::Cleaned);
        drop(previous);
    }

    pub(super) fn finish_external(&self, task: TaskId) {
        let record = self
            .tasks
            .lock()
            .expect("task map mutex")
            .get(&task)
            .map(|record| (Arc::clone(&record.control), record.context));
        let Some((control, context)) = record else {
            return;
        };
        // A continuation can finish outside the worker that originally ran
        // the task. Preserve the invocation path's terminal-state precedence
        // and notify joiners only after lease cleanup is complete.
        let state = if unsafe { (*context).abort_operation } != super::NO_TASK_ABORT {
            TaskState::Aborted
        } else if control.cancellation.load(Ordering::Acquire) {
            TaskState::Cancelled
        } else {
            TaskState::Completed
        };
        unsafe { crate::runtime::task::cleanup_task_cown_leases(context) };
        control.finish(state);
        self.mark_completed();
    }

    /// Publish the first abort and cooperatively stop the remaining siblings.
    pub(super) fn record_abort(
        &self,
        source: TaskId,
        failure: TaskFailure,
    ) -> Result<(), TaskFailure> {
        let mut failure = Some(failure);
        let should_cancel = {
            let mut first_failure = self.first_failure.lock().expect("task failure mutex");
            if matches!(*first_failure, super::TaskFailureSlot::Empty) {
                *first_failure =
                    super::TaskFailureSlot::Pending(failure.take().expect("failure is present"));
                true
            } else {
                false
            }
        };
        if !should_cancel {
            return Err(failure.expect("rejected task failure"));
        }
        let siblings = self
            .tasks
            .lock()
            .expect("task map mutex")
            .iter()
            .filter(|(id, _)| **id != source)
            .map(|(_, task)| {
                task.control.wake.cancel(&task.control.cancellation);
                crate::runtime::continuation::task_continuation(task.context)
            })
            .collect::<Vec<_>>();
        for continuation in siblings.into_iter().flatten() {
            // A sibling may be suspended without a worker invocation. Route
            // cancellation through its continuation so it reaches a terminal
            // state and releases its frame/leases promptly.
            continuation.cancel();
        }
        Ok(())
    }
}

pub(super) unsafe fn drop_task_result(context: *mut TaskContext) {
    if unsafe { (*context).cancelled_result } {
        return;
    }
    if unsafe { !(*context).result_initialized } {
        return;
    }
    let drop_result = unsafe { (*context).drop_result.take() };
    if let Some(drop_result) = drop_result {
        // SAFETY: the generated thunk matches the task's result ABI.
        unsafe { drop_result((*context).result) };
    }
}

pub(super) unsafe fn claim_task_result(context: *mut TaskContext) {
    unsafe { (*context).drop_result = None };
}
