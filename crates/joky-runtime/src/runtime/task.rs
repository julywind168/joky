//! Structured task runtime primitives.
//!
//! MIR task instructions will eventually lower to this ABI. The runtime owns
//! scheduling, cancellation and completion; generated code owns the typed
//! capture and result storage referenced by [`TaskContext`].

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use super::scope;

mod abi;
mod context;
mod current;
mod group;
mod scheduler;

pub(crate) use abi::*;
use context::{HeapTaskStorage, NO_TASK_ABORT, TASK_PANIC_ABORT};
pub(crate) use context::{
    TaskContext, TaskFailure, TaskFailureDropThunk, TaskId, TaskState, TaskThunk,
};
use current::abort_current_task;
pub(crate) use current::*;
pub(crate) use group::TaskGroupInner;
use group::{claim_task_result, drop_task_result, TaskRecord};
#[cfg(test)]
use joky_runtime_abi::{
    TASK_CONTEXT_CANCELLATION_OFFSET, TASK_CONTEXT_CAPTURES_OFFSET,
    TASK_CONTEXT_CONTINUATION_OFFSET, TASK_CONTEXT_RESULT_OFFSET, TASK_CONTEXT_SIZE,
};
use scheduler::{current_worker_thread, task_scheduler, TaskControl, TaskInvocation};

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(any(test, feature = "test-support"))]
static TEST_HEAP_TASK_PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(any(test, feature = "test-support"))]
static TEST_HEAP_TASK_SPAWNS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Process-isolated batch tests compare actual task storage against the limit.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn reset_heap_task_metrics() {
    assert!(heap_task_storage().lock().unwrap().is_empty());
    TEST_HEAP_TASK_PEAK.store(0, Ordering::SeqCst);
    TEST_HEAP_TASK_SPAWNS.store(0, Ordering::SeqCst);
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn heap_task_metrics() -> (usize, usize, usize) {
    (
        TEST_HEAP_TASK_SPAWNS.load(Ordering::SeqCst),
        TEST_HEAP_TASK_PEAK.load(Ordering::SeqCst),
        heap_task_storage().lock().unwrap().len(),
    )
}

static HEAP_TASK_STORAGE: OnceLock<Mutex<HashMap<usize, Box<HeapTaskStorage>>>> = OnceLock::new();

fn heap_task_storage() -> &'static Mutex<HashMap<usize, Box<HeapTaskStorage>>> {
    HEAP_TASK_STORAGE.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
    static CURRENT_CANCELLATION: Cell<*const AtomicBool> = const { Cell::new(std::ptr::null()) };
    static CURRENT_TASK_CONTEXT: Cell<*mut TaskContext> = const { Cell::new(std::ptr::null_mut()) };
    static CURRENT_TASK_CONTROL: Cell<*const TaskControl> = const { Cell::new(std::ptr::null()) };
}

/// Ownership state for a task group's first failure payload.
enum TaskFailureSlot {
    Empty,
    Pending(TaskFailure),
    Claimed,
    Cleaned,
}

/// Version for the task/failure C ABI consumed by generated code.
#[cfg(test)]
pub(crate) const TASK_ABI_VERSION: u32 = 1;
#[cfg(test)]
pub(crate) const FAILURE_ABI_VERSION: u32 = 1;

#[cfg(test)]
pub(crate) fn task_failure_abi_descriptor() -> crate::runtime::abi::FailureAbiDescriptor {
    crate::runtime::abi::FailureAbiDescriptor {
        version: FAILURE_ABI_VERSION,
        operation_word_size: std::mem::size_of::<u64>() as u32,
        payload_alignment: std::mem::align_of::<u64>() as u32,
        drop_callback_size: std::mem::size_of::<TaskFailureDropThunk>() as u32,
        state_empty: 0,
        state_pending: 1,
        state_claimed: 2,
        state_cleaned: 3,
    }
}

#[cfg(test)]
pub(crate) fn task_abi_descriptor() -> crate::runtime::abi::TaskAbiDescriptor {
    crate::runtime::abi::TaskAbiDescriptor {
        version: TASK_ABI_VERSION,
        context_size: TASK_CONTEXT_SIZE,
        captures_offset: TASK_CONTEXT_CAPTURES_OFFSET,
        result_offset: TASK_CONTEXT_RESULT_OFFSET,
        cancellation_offset: TASK_CONTEXT_CANCELLATION_OFFSET,
        continuation_offset: TASK_CONTEXT_CONTINUATION_OFFSET,
    }
}

/// A lexical group of child tasks. Invocations are scheduled on the process
/// worker pool; the group still owns every context until join/close.
pub(crate) struct TaskGroup {
    region: Option<Arc<super::region::Region>>,
    pub(crate) inner: Arc<TaskGroupInner>,
}

impl TaskGroup {
    pub(crate) fn new() -> Self {
        let runtime_scope = scope::current_or_default();
        let inner = Arc::new(TaskGroupInner {
            _resource: crate::runtime::resources::Lease::new(
                &runtime_scope,
                crate::runtime::resources::Kind::TaskGroups,
                1,
            ),
            scope: Arc::clone(&runtime_scope),
            tasks: Mutex::new(HashMap::new()),
            first_failure: Mutex::new(TaskFailureSlot::Empty),
            completion_epoch: Mutex::new(0),
            waiters: Mutex::new(Vec::new()),
            race_waiting: AtomicBool::new(false),
            race_selected: AtomicBool::new(false),
            completion: Condvar::new(),
            close: Mutex::new(()),
            scope_work: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        if let Some(work) = runtime_scope.begin_work(move || {
            if let Some(inner) = weak.upgrade() {
                inner.cancel_all();
                inner.close_finished_tasks();
            }
        }) {
            *inner.scope_work.lock().expect("task scope work mutex") = Some(work);
        }
        Self {
            inner,
            region: None,
        }
    }

    /// Start a task. The caller must keep `context` and all storage it points
    /// to alive until this group has joined or closed the task.
    pub(crate) unsafe fn spawn(&self, thunk: TaskThunk, context: *mut TaskContext) -> TaskId {
        let id = TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed));
        let control = Arc::new(TaskControl::new());
        // SAFETY: the caller upholds the context allocation lifetime; the
        // control Arc outlives the spawned thread and owns cancellation.
        unsafe {
            (*context).cancellation = &control.cancellation;
            (*context).abort_operation = NO_TASK_ABORT;
            (*context).group = Arc::as_ptr(&self.inner);
            (*context).task = id;
            (*context).wake = Arc::as_ptr(&control.wake);
            (*context)
                .continuation_pending
                .store(false, std::sync::atomic::Ordering::Release);
            (*context).handler_frame = crate::runtime::handler::current();
        }
        // Publish the record while holding the same locks as `record_abort`.
        // A task can fail before its parent has finished creating the
        // remaining siblings; new tasks must inherit that cancellation
        // instead of starting an uncancellable loop during group shutdown.
        let cancelled_before_spawn = {
            let first_failure = self.inner.first_failure.lock().expect("task failure mutex");
            let mut tasks = self.inner.tasks.lock().expect("task map mutex");
            let cancelled = matches!(*first_failure, TaskFailureSlot::Pending(_));
            tasks.insert(
                id,
                TaskRecord {
                    control: Arc::clone(&control),
                    context,
                },
            );
            cancelled
        };
        if cancelled_before_spawn {
            control.cancellation.store(true, Ordering::Release);
        }
        let invocation = TaskInvocation {
            thunk,
            context,
            control: Arc::as_ptr(&control),
            scope: Arc::clone(&self.inner.scope),
            region: super::region::current(),
        };
        scheduler::enqueue_invocation(invocation, control, Arc::clone(&self.inner), context);
        id
    }

    pub(crate) fn state(&self, id: TaskId) -> Option<TaskState> {
        self.inner
            .tasks
            .lock()
            .expect("task map mutex")
            .get(&id)
            .map(|task| *task.control.state.lock().expect("task state mutex"))
    }

    fn has_failure(&self) -> bool {
        let slot = self.inner.first_failure.lock().expect("task failure mutex");
        matches!(*slot, TaskFailureSlot::Pending(_))
    }

    #[cfg(test)]
    fn failure_snapshot(&self) -> Option<(u64, Vec<u8>)> {
        let slot = self.inner.first_failure.lock().expect("task failure mutex");
        match &*slot {
            TaskFailureSlot::Pending(failure) => Some((failure.operation, failure.payload.clone())),
            _ => None,
        }
    }

    fn failure_operation(&self) -> Option<u64> {
        let slot = self.inner.first_failure.lock().expect("task failure mutex");
        match &*slot {
            TaskFailureSlot::Pending(failure) => Some(failure.operation),
            _ => None,
        }
    }

    fn failure_payload(&self) -> Option<(*const c_void, usize)> {
        let slot = self.inner.first_failure.lock().expect("task failure mutex");
        match &*slot {
            TaskFailureSlot::Pending(failure) if !failure.payload.is_empty() => {
                Some((failure.payload.as_ptr().cast(), failure.payload.len()))
            }
            _ => None,
        }
    }

    fn take_failure(&self) -> Option<TaskFailure> {
        let mut slot = self.inner.first_failure.lock().expect("task failure mutex");
        match std::mem::replace(&mut *slot, TaskFailureSlot::Claimed) {
            TaskFailureSlot::Pending(failure) => Some(failure),
            previous => {
                *slot = previous;
                None
            }
        }
    }

    fn claim_failure(&self) {
        if let Some(mut failure) = self.take_failure() {
            failure.drop_payload = None;
        }
    }

    pub(crate) fn cancel(&self, id: TaskId) -> bool {
        let Some((task, context)) = self
            .inner
            .tasks
            .lock()
            .expect("task map mutex")
            .get(&id)
            .map(|task| (Arc::clone(&task.control), task.context))
        else {
            return false;
        };
        task.wake.cancel(&task.cancellation);
        // A suspended task no longer has a worker waiting in its invocation.
        // Cancel its continuation immediately so joins do not wait for the
        // timer and so its frame/result/spill ownership is released now.
        unsafe { crate::runtime::continuation::cancel_task_continuation(context) };
        true
    }

    pub(crate) fn join(&self, id: TaskId) -> Option<TaskState> {
        let control = self
            .inner
            .tasks
            .lock()
            .expect("task map mutex")
            .get(&id)
            .map(|task| Arc::clone(&task.control));
        control.map(|task| {
            if current_worker_thread() {
                task_scheduler().help_until(&task);
                *task.state.lock().expect("task state mutex")
            } else {
                task.wait()
            }
        })
    }

    pub(crate) fn close(&self) {
        self.inner.close_finished_tasks();
    }

    /// Wait for the first successfully completed task, then request
    /// cancellation for every loser and wait for its cleanup to finish.
    pub(crate) fn race(&self, tasks: &[TaskId]) -> Option<TaskId> {
        if tasks.is_empty() {
            return None;
        }
        let mut epoch = *self
            .inner
            .completion_epoch
            .lock()
            .expect("completion mutex");
        loop {
            if self.has_failure() {
                for task in tasks {
                    self.cancel(*task);
                }
                for task in tasks {
                    self.join(*task);
                }
                return None;
            }
            for task in tasks {
                if self.state(*task) == Some(TaskState::Completed) {
                    for loser in tasks.iter().copied().filter(|loser| loser != task) {
                        self.cancel(loser);
                    }
                    for task in tasks {
                        self.join(*task);
                    }
                    return Some(*task);
                }
            }
            if tasks.iter().all(|task| {
                matches!(
                    self.state(*task),
                    Some(TaskState::Completed | TaskState::Cancelled | TaskState::Aborted) | None
                )
            }) {
                // A task may complete between the first winner scan and this
                // terminal-state check. Prefer that completed task instead of
                // incorrectly reporting that the race had no winner.
                if let Some(winner) = tasks
                    .iter()
                    .copied()
                    .find(|task| self.state(*task) == Some(TaskState::Completed))
                {
                    for loser in tasks.iter().copied().filter(|loser| *loser != winner) {
                        self.cancel(loser);
                    }
                    for task in tasks {
                        self.join(*task);
                    }
                    return Some(winner);
                }
                return None;
            }
            if current_worker_thread() && task_scheduler().help_one() {
                epoch = *self
                    .inner
                    .completion_epoch
                    .lock()
                    .expect("completion mutex");
                continue;
            }
            let mut guard = self
                .inner
                .completion_epoch
                .lock()
                .expect("completion mutex");
            while *guard == epoch {
                let (next, _) = self
                    .inner
                    .completion
                    .wait_timeout(guard, Duration::from_millis(2))
                    .expect("completion mutex");
                guard = next;
            }
            epoch = *guard;
        }
    }

    /// Behaves like [`Self::race`] and copies the winner's flattened result
    /// representation into storage owned by the parent frame.
    pub(crate) unsafe fn race_into(
        &self,
        tasks: &[TaskId],
        result: *mut c_void,
        result_size: usize,
    ) -> Option<TaskId> {
        let winner = self.race(tasks)?;
        if result_size != 0 {
            let context = self
                .inner
                .tasks
                .lock()
                .expect("task map mutex")
                .get(&winner)
                .map(|task| task.context)?;
            // SAFETY: race() joined every participating task. Codegen gives
            // every arm equal-sized result storage and keeps both buffers live
            // through the enclosing ScopeExit.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (*context).result.cast::<u8>(),
                    result.cast(),
                    result_size,
                );
            }
        }
        let contexts = self
            .inner
            .tasks
            .lock()
            .expect("task map mutex")
            .iter()
            .filter(|(task, _)| tasks.contains(task))
            .map(|(task, record)| (*task, record.context))
            .collect::<Vec<_>>();
        for (task, context) in contexts {
            if task == winner {
                // The copied winner result is now owned by the parent frame.
                unsafe { claim_task_result(context) };
            } else {
                // A loser may have completed before cancellation won the
                // race. Preserve its real result so its drop thunk still
                // runs; genuinely cancelled tasks keep their placeholder
                // result and are skipped by `drop_task_result`.
                unsafe { (*context).cancelled_result = false };
                unsafe { drop_task_result(context) };
            }
        }
        Some(winner)
    }

    pub(crate) unsafe fn claim_result(&self, id: TaskId) {
        let context = self
            .inner
            .tasks
            .lock()
            .expect("task map mutex")
            .get(&id)
            .map(|task| task.context);
        if let Some(context) = context {
            // SAFETY: caller joined the task before moving its result to the
            // parent frame.
            unsafe { claim_task_result(context) };
        }
    }
}

impl Drop for TaskGroup {
    fn drop(&mut self) {
        self.close();
        if let Some(region) = &self.region {
            region.close();
        }
    }
}

#[cfg(test)]
mod tests;
