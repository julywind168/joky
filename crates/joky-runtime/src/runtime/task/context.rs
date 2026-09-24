use std::ffi::c_void;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::scheduler::TaskWake;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TaskId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskState {
    Pending,
    Running,
    Sleeping,
    Completed,
    Cancelled,
    Aborted,
}

/// The language-level failure that first escaped a lexical task scope.
///
/// The operation is intentionally opaque to the scheduler. MIR/codegen own
/// its stable encoding and will later pair it with a typed payload when an
/// enclosing effect handler receives the failure.
#[derive(Debug)]
pub(crate) struct TaskFailure {
    pub(crate) operation: u64,
    /// Flattened ABI components copied out of the child task before its stack
    /// frame can disappear. Ownership transfer/drop is performed by the
    /// parent-side typed payload lowering, not by this byte buffer.
    pub(crate) payload: Vec<u8>,
    pub(super) drop_payload: Option<TaskFailureDropThunk>,
}

pub(crate) type TaskFailureDropThunk = unsafe extern "C" fn(*const c_void);

impl Drop for TaskFailure {
    fn drop(&mut self) {
        let Some(drop_payload) = self.drop_payload.take() else {
            return;
        };
        // SAFETY: codegen pairs the flattened payload with drop glue generated
        // from the effect operation's parameter types.
        unsafe { drop_payload(self.payload.as_ptr().cast()) };
    }
}

pub(super) const NO_TASK_ABORT: u64 = u64::MAX;
/// Reserved operation identity used when a generated task thunk unwinds.
/// User effects occupy the stable operation-id space below this sentinel.
pub(super) const TASK_PANIC_ABORT: u64 = u64::MAX - 1;

/// Stable ABI passed to generated task thunks.
///
/// `captures` and `result` are typed allocations owned by codegen or the heap
/// task-spawn path. Their lifetime must extend until the group has observed
/// task completion and run any required cleanup.
#[repr(C)]
pub(crate) struct TaskContext {
    pub(crate) captures: *mut c_void,
    pub(crate) result: *mut c_void,
    pub(crate) result_size: usize,
    pub(super) drop_result: Option<TaskResultDropThunk>,
    pub(super) cancelled_result: bool,
    pub(super) result_initialized: bool,
    pub(super) cancellation: *const AtomicBool,
    pub(super) abort_operation: u64,
    pub(super) group: *const super::TaskGroupInner,
    pub(super) task: TaskId,
    pub(super) wake: *const TaskWake,
    pub(crate) continuation_pending: AtomicBool,
    /// Captured lexical handler chain. The owning scope waits for this task
    /// before the frame can be released.
    pub(crate) handler_frame: *const crate::runtime::handler::HandlerFrame,
    /// Raw continuation handle associated with a suspended invocation.  The
    /// handle is stored atomically so task cancellation can detach it without
    /// racing the timer thread.
    pub(crate) continuation: AtomicUsize,
}

pub(super) struct HeapTaskStorage {
    pub(super) context: TaskContext,
    pub(super) captures: Box<[u8]>,
    pub(super) result: Box<[u8]>,
}

// The storage is protected by the mutex and the contained raw pointers are
// only consumed by the generated task thunk under the same lifetime contract
// as `TaskInvocation`.
unsafe impl Send for HeapTaskStorage {}

impl TaskContext {
    #[cfg(test)]
    pub(crate) unsafe fn is_cancelled(&self) -> bool {
        // SAFETY: TaskGroup installs a pointer into the task's Arc control and
        // keeps that control alive until the thunk has completed.
        unsafe { (*self.cancellation).load(Ordering::Acquire) }
    }
}

// The unwind-capable C ABI lets native provider thunks be contained by
// `catch_unwind` without allowing a panic to cross the scheduler boundary.
// JIT entries are passed as raw pointers at the C ABI boundary.
pub(crate) type TaskThunk = unsafe extern "C-unwind" fn(*mut TaskContext);
pub(crate) type TaskResultDropThunk = unsafe extern "C" fn(*mut c_void);
