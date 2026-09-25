//! Task runtime tests.

use super::context::TaskResultDropThunk;
use super::*;
use crate::runtime::scope::RuntimeScope;
use joky_runtime_abi::{
    HANDLER_CALL_CAPTURES_OFFSET, HANDLER_CALL_REQUEST_OFFSET, HANDLER_CALL_RESULT_OFFSET,
    HANDLER_CALL_RESULT_SIZE_OFFSET, HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET,
    HANDLER_ENV_SLOT_OFFSET_OFFSET, HANDLER_ENV_SLOT_OWNERSHIP_OFFSET, HANDLER_ENV_SLOT_SIZE,
    TASK_CONTEXT_CANCELLED_RESULT_OFFSET, TASK_CONTEXT_DROP_RESULT_OFFSET,
    TASK_CONTEXT_RESULT_INITIALIZED_OFFSET, TASK_CONTEXT_RESULT_SIZE_OFFSET,
};

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TASK_START_TIMEOUT: Duration = Duration::from_secs(10);

struct TestContinuation(*mut crate::runtime::continuation::Continuation);

impl Drop for TestContinuation {
    fn drop(&mut self) {
        // TestTask must drain task execution before this handle is released.
        unsafe { crate::runtime::continuation::jk_continuation_free(self.0) };
    }
}

/// Declare after context/capture storage so unwinding cancels and drains
/// native execution before any borrowed storage is dropped.
struct TestTask<'a> {
    group: &'a TaskGroup,
    id: TaskId,
    control: Arc<TaskControl>,
    context: *mut TaskContext,
    external_completion: bool,
}

impl<'a> TestTask<'a> {
    fn new(group: &'a TaskGroup, id: TaskId, external_completion: bool) -> Self {
        let tasks = group.inner.tasks.lock().expect("task map mutex");
        let record = tasks.get(&id).expect("spawned test task");
        Self {
            group,
            id,
            control: Arc::clone(&record.control),
            context: record.context,
            external_completion,
        }
    }

    fn wait_for_invocation(&self) {
        let (finished, signal) = &self.control.invocation_finished;
        let (finished, _) = signal
            .wait_timeout_while(
                finished.lock().expect("test invocation mutex"),
                TASK_START_TIMEOUT,
                |finished| !*finished,
            )
            .expect("test invocation mutex");
        // Release the lock before asserting: failure must not poison the
        // synchronization needed by this guard's unwind cleanup.
        let did_finish = *finished;
        drop(finished);
        assert!(
            did_finish,
            "task invocation did not return before the deadline"
        );
    }

    fn wait_for_sleeping(&self) {
        assert!(
            self.control
                .wait_for_state(TaskState::Sleeping, TASK_START_TIMEOUT),
            "task did not sleep before the deadline: {:?}",
            self.group.state(self.id),
        );
    }
}

impl Drop for TestTask<'_> {
    fn drop(&mut self) {
        // Request cancellation without concurrently cleaning the legacy
        // continuation while its initial native invocation still uses it.
        self.control.wake.cancel(&self.control.cancellation);
        let (finished, signal) = &self.control.invocation_finished;
        drop(
            signal
                .wait_while(
                    finished.lock().expect("test invocation mutex"),
                    |finished| !*finished,
                )
                .expect("test invocation mutex"),
        );
        if !self.group.cancel(self.id) {
            return;
        }
        if self.external_completion && self.group.state(self.id) == Some(TaskState::Sleeping) {
            complete_suspended_task(self.context);
        }
        self.group.join(self.id);
        self.group.close();
    }
}

#[test]
fn abi_descriptors_match_runtime_layouts() {
    assert_eq!(joky_runtime_abi::FunctionCallStatus::Ready as u8, 0);
    assert_eq!(joky_runtime_abi::FunctionCallStatus::Pending as u8, 1);
    assert_eq!(joky_runtime_abi::FunctionCallStatus::Failed as u8, 2);
    assert_eq!(joky_runtime_abi::FunctionCallStatus::Cancelled as u8, 3);
    let task = task_abi_descriptor();
    assert_eq!(task.version, 1);
    assert_eq!(task.context_size, TASK_CONTEXT_SIZE);
    assert_eq!(task.continuation_offset, TASK_CONTEXT_CONTINUATION_OFFSET);
    assert_eq!(std::mem::size_of::<TaskContext>() as u32, TASK_CONTEXT_SIZE);
    assert_eq!(
        std::mem::offset_of!(TaskContext, captures) as i32,
        TASK_CONTEXT_CAPTURES_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, result) as i32,
        TASK_CONTEXT_RESULT_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, result_size) as i32,
        TASK_CONTEXT_RESULT_SIZE_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, drop_result) as i32,
        TASK_CONTEXT_DROP_RESULT_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, cancelled_result) as i32,
        TASK_CONTEXT_CANCELLED_RESULT_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, result_initialized) as i32,
        TASK_CONTEXT_RESULT_INITIALIZED_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, cancellation) as i32,
        TASK_CONTEXT_CANCELLATION_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(TaskContext, continuation) as i32,
        TASK_CONTEXT_CONTINUATION_OFFSET
    );
    let continuation = crate::runtime::continuation::continuation_abi_descriptor();
    assert_eq!(continuation.version, 1);
    assert_eq!(
        continuation.state_ready,
        crate::runtime::continuation::ContinuationState::Ready as u8
    );
    assert_eq!(
        continuation.state_completed,
        crate::runtime::continuation::ContinuationState::Completed as u8
    );
    let handler = crate::runtime::handler::handler_abi_descriptor();
    assert_eq!(handler.version, 1);
    assert_eq!(
        handler.call_size,
        std::mem::size_of::<crate::runtime::handler::HandlerCall>() as u32
    );
    assert_eq!(handler.env_slot_size, HANDLER_ENV_SLOT_SIZE as u32);
    assert_eq!(handler.request_offset, HANDLER_CALL_REQUEST_OFFSET);
    assert_eq!(handler.captures_offset, HANDLER_CALL_CAPTURES_OFFSET);
    assert_eq!(handler.result_offset, HANDLER_CALL_RESULT_OFFSET);
    assert_eq!(
        std::mem::offset_of!(crate::runtime::handler::HandlerCall, result_size) as i32,
        HANDLER_CALL_RESULT_SIZE_OFFSET
    );
    assert_eq!(
        std::mem::size_of::<crate::runtime::handler::HandlerEnvSlot>() as i32,
        HANDLER_ENV_SLOT_SIZE
    );
    assert_eq!(
        std::mem::offset_of!(crate::runtime::handler::HandlerEnvSlot, offset) as i32,
        HANDLER_ENV_SLOT_OFFSET_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(crate::runtime::handler::HandlerEnvSlot, ownership) as i32,
        HANDLER_ENV_SLOT_OWNERSHIP_OFFSET
    );
    assert_eq!(
        std::mem::offset_of!(crate::runtime::handler::HandlerEnvSlot, drop_callback) as i32,
        HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET
    );
    let managed = crate::runtime::managed::managed_abi_descriptor();
    assert_eq!(
        managed.version,
        crate::runtime::managed::RUNTIME_ABI_VERSION
    );
    assert_eq!(
        managed.header_size,
        std::mem::size_of::<crate::runtime::managed::ObjectHeader>() as u32
    );
    assert_eq!(
        managed.cown_state_size,
        std::mem::size_of::<crate::runtime::managed::CownState>() as u32
    );
    assert_eq!(
        managed.cown_id_offset,
        std::mem::offset_of!(crate::runtime::managed::CownState, id) as i32
    );
    assert_eq!(
        managed.cown_payload_offset,
        std::mem::offset_of!(crate::runtime::managed::CownState, payload) as i32
    );
    let effect = crate::runtime::effect_abi_descriptor();
    assert_eq!(effect.operation_word_size, 8);
    assert_eq!(effect.payload_alignment, std::mem::align_of::<u64>() as u32);
    let failure = task_failure_abi_descriptor();
    assert_eq!(failure.version, FAILURE_ABI_VERSION);
    assert_eq!(failure.operation_word_size, 8);
    assert_eq!(failure.state_pending, 1);
    assert_eq!(
        failure.drop_callback_size,
        std::mem::size_of::<TaskFailureDropThunk>() as u32
    );
}

static CLOSE_RESULT_DROPS: AtomicU64 = AtomicU64::new(0);
static CLOSE_RESULT_TEST_LOCK: Mutex<()> = Mutex::new(());
static RACE_RESULT_DROPS: AtomicU64 = AtomicU64::new(0);
static CANCELLED_CONTINUATION_DISPATCHES: AtomicU64 = AtomicU64::new(0);
static INHERITED_HANDLER: AtomicBool = AtomicBool::new(false);

unsafe extern "C-unwind" fn wait_for_peer(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid Barrier in captures until join.
    let barrier = unsafe { &*((*context).captures as *const Barrier) };
    barrier.wait();
}

unsafe extern "C-unwind" fn wait_for_cancel(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid Barrier in captures until join.
    let barrier = unsafe { &*((*context).captures as *const Barrier) };
    barrier.wait();
    while !unsafe { (*context).is_cancelled() } {
        thread::yield_now();
    }
}

unsafe extern "C-unwind" fn wait_for_runtime_poll(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid Barrier in captures until join.
    let barrier = unsafe { &*((*context).captures as *const Barrier) };
    barrier.wait();
    while jk_task_is_cancelled() == 0 {
        thread::yield_now();
    }
}

unsafe extern "C-unwind" fn sleep_for_test(_: *mut TaskContext) {
    jk_time_sleep(60_000);
}

unsafe extern "C-unwind" fn abort_after_sync(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid Barrier in captures until join.
    let barrier = unsafe { &*((*context).captures as *const Barrier) };
    barrier.wait();
    jk_task_abort(73);
}

unsafe extern "C-unwind" fn abort_payload_after_sync(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid Barrier in captures until join.
    let barrier = unsafe { &*((*context).captures as *const Barrier) };
    barrier.wait();
    let payload = [17_u64, 29_u64];
    // SAFETY: the runtime copies the payload before this function returns.
    unsafe {
        jk_task_abort_payload(
            74,
            payload.as_ptr().cast(),
            std::mem::size_of_val(&payload),
            std::ptr::null(),
        );
    }
}

unsafe extern "C-unwind" fn observes_inherited_handler(_: *mut TaskContext) {
    INHERITED_HANDLER.store(
        crate::runtime::handler::nearest_operation(91).is_some(),
        Ordering::Release,
    );
}

unsafe extern "C-unwind" fn complete(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid counter in captures until join.
    let counter = unsafe { &*((*context).captures as *const Mutex<u32>) };
    *counter.lock().expect("counter mutex") += 1;
}

unsafe extern "C-unwind" fn increment_atomic(context: *mut TaskContext) {
    // SAFETY: test setup stores a valid atomic counter in captures until
    // every queued invocation has joined.
    let counter = unsafe { &*((*context).captures as *const AtomicUsize) };
    counter.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "C-unwind" fn hold_cown_until_cancel(context: *mut TaskContext) {
    // SAFETY: the test keeps the capture array and Cown capability alive
    // until the task has joined.
    let captures = unsafe { &*((*context).captures as *const [usize; 2]) };
    let cown = captures[0] as *mut u8;
    let entered = unsafe { &*(captures[1] as *const AtomicBool) };
    let payload = crate::runtime::managed::jk_cown_acquire(cown);
    assert!(!payload.is_null());
    entered.store(true, Ordering::Release);
    while !task_context_is_cancelled(context) {
        thread::yield_now();
    }
}

unsafe extern "C-unwind" fn acquire_cown_then_panic(context: *mut TaskContext) {
    // SAFETY: the test keeps the capture array and Cown capability alive
    // until the task has joined.
    let captures = unsafe { &*((*context).captures as *const [usize; 3]) };
    let cown = captures[0] as *mut u8;
    let barrier = unsafe { &*(captures[1] as *const Barrier) };
    assert!(!crate::runtime::managed::jk_cown_acquire(cown).is_null());
    barrier.wait();
    panic!("task thunk panic");
}

unsafe extern "C-unwind" fn panic_without_result(_: *mut TaskContext) {
    panic!("task thunk panic before result initialization");
}

unsafe extern "C-unwind" fn write_result(context: *mut TaskContext) {
    // SAFETY: test setup provides a u64 result slot for this task.
    unsafe { *((*context).result as *mut u64) = 7 };
}

unsafe extern "C-unwind" fn write_result_and_mark(context: *mut TaskContext) {
    // SAFETY: test setup provides a u64 result slot and an AtomicUsize
    // capture that remains live until both tasks have entered race().
    unsafe {
        *((*context).result as *mut u64) = 7;
        let ready = &*((*context).captures as *const AtomicUsize);
        ready.fetch_add(1, Ordering::AcqRel);
    }
}

unsafe extern "C" fn count_close_result_drop(_: *mut c_void) {
    CLOSE_RESULT_DROPS.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "C" fn count_race_result_drop(_: *mut c_void) {
    RACE_RESULT_DROPS.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "C-unwind" fn suspend_without_completion(_: *mut TaskContext) {
    mark_current_task_continuation_pending();
}

unsafe extern "C-unwind" fn continuation_sleep_task(context: *mut TaskContext) {
    // SAFETY: the test keeps the raw continuation handle alive until the
    // task has joined and the timer worker has exited.
    let continuation =
        unsafe { *((*context).captures as *const *mut crate::runtime::continuation::Continuation) };
    let _ = unsafe {
        crate::runtime::continuation::jk_continuation_set_machine_entry(
            continuation,
            count_cancelled_continuation as *mut c_void,
        )
    };
    let _ =
        unsafe { crate::runtime::continuation::jk_continuation_begin_sleep(continuation, 60_000) };
}

unsafe extern "C" fn count_cancelled_continuation(
    _continuation: *mut crate::runtime::continuation::Continuation,
) {
    CANCELLED_CONTINUATION_DISPATCHES.fetch_add(1, Ordering::SeqCst);
}

fn context(captures: *mut c_void) -> TaskContext {
    TaskContext {
        captures,
        result: std::ptr::null_mut(),
        result_size: 0,
        drop_result: None,
        cancelled_result: false,
        result_initialized: false,
        cancellation: std::ptr::null(),
        abort_operation: NO_TASK_ABORT,
        group: std::ptr::null(),
        task: TaskId(0),
        wake: std::ptr::null(),
        continuation_pending: AtomicBool::new(false),
        handler_frame: std::ptr::null(),
        continuation: AtomicUsize::new(0),
    }
}

fn result_context(result: *mut u64, drop_result: TaskResultDropThunk) -> TaskContext {
    TaskContext {
        captures: std::ptr::null_mut(),
        result: result.cast(),
        result_size: std::mem::size_of::<u64>(),
        drop_result: Some(drop_result),
        cancelled_result: false,
        result_initialized: false,
        cancellation: std::ptr::null(),
        abort_operation: NO_TASK_ABORT,
        group: std::ptr::null(),
        task: TaskId(0),
        wake: std::ptr::null(),
        continuation_pending: AtomicBool::new(false),
        handler_frame: std::ptr::null(),
        continuation: AtomicUsize::new(0),
    }
}

#[test]
fn function_pending_does_not_replace_the_enclosing_task_continuation() {
    use crate::runtime::continuation::*;
    let mut outer = context(std::ptr::null_mut());
    outer.continuation_pending.store(true, Ordering::Release);
    outer.continuation.store(123, Ordering::Release);
    let handle = jk_continuation_new(0);
    unsafe {
        with_task_context(&mut outer, || {
            assert_eq!(jk_continuation_begin_function_pending(handle), 1);
            assert_eq!(
                jk_continuation_complete_function_pending(handle, std::ptr::null(), 0),
                1
            );
            assert_eq!(jk_continuation_poll_function_pending(handle), 0);
        });
        jk_continuation_free(handle);
    }
    assert_eq!(outer.continuation.load(Ordering::Acquire), 123);
    assert!(outer.continuation_pending.load(Ordering::Acquire));
    assert!(!outer.cancelled_result);
}

#[test]
fn pending_task_handoff_does_not_rebind_cancelled_work() {
    use crate::runtime::continuation::*;

    for cancel_task in [false, true] {
        let scope = RuntimeScope::new();
        let _guard = scope.enter();
        let token = AtomicBool::new(false);
        let mut task = context(std::ptr::null_mut());
        task.cancellation = &token;
        let handle = jk_continuation_new(0);
        let handed_off = unsafe {
            with_task_function_pending_boundary(&mut task, || {
                assert_eq!(jk_continuation_begin_function_pending(handle), 1);
                assert_eq!(jk_continuation_poll_function_pending(handle), 1);
                if cancel_task {
                    token.store(true, Ordering::Release);
                } else {
                    assert_eq!(jk_continuation_cancel(handle), 1);
                }
                jk_task_function_status(1);
            })
        };
        assert!(!handed_off);
        assert!(!task.continuation_pending.load(Ordering::Acquire));
        assert_eq!(task.continuation.load(Ordering::Acquire), 0);
        assert_eq!(
            unsafe { jk_continuation_state(handle) },
            ContinuationState::Cancelled as u8
        );
        unsafe { jk_continuation_free(handle) };
        scope.close_and_wait();
    }
}

#[test]
fn spawned_task_inherits_the_dynamic_handler_chain() {
    INHERITED_HANDLER.store(false, Ordering::Release);
    let frame = unsafe { crate::runtime::handler::jk_handler_frame_new([91_u64].as_ptr(), 1) };
    assert_eq!(
        unsafe { crate::runtime::handler::jk_handler_frame_enter(frame) },
        1
    );
    let group = TaskGroup::new();
    let mut context = context(std::ptr::null_mut());
    let task = unsafe { group.spawn(observes_inherited_handler, &mut context) };
    assert_eq!(group.join(task), Some(TaskState::Completed));
    assert!(INHERITED_HANDLER.load(Ordering::Acquire));
    group.close();
    assert_eq!(
        unsafe { crate::runtime::handler::jk_handler_frame_exit(frame) },
        1
    );
    unsafe { crate::runtime::handler::jk_handler_frame_free(frame) };
}

#[test]
fn suspended_task_remains_joinable_until_continuation_completion() {
    let group = TaskGroup::new();
    let mut context = context(std::ptr::null_mut());
    let task = unsafe { group.spawn(suspend_without_completion, &mut context) };
    let pending = TestTask::new(&group, task, true);
    pending.wait_for_invocation();
    assert_eq!(group.state(task), Some(TaskState::Sleeping));
    complete_suspended_task(&mut context);
    assert_eq!(group.join(task), Some(TaskState::Completed));
    group.close();
}

#[test]
fn test_task_guard_drains_a_delayed_invocation_on_unwind() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut context = context((&barrier as *const Barrier).cast_mut().cast());
    let task = unsafe { group.spawn(wait_for_runtime_poll, &mut context) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pending = TestTask::new(&group, task, false);
        barrier.wait();
        // The invocation is deliberately held before returning. Its guard
        // must cancel and drain it even when an early assertion fails.
        assert!(!*pending.control.invocation_finished.0.lock().unwrap());
        panic!("simulated assertion before invocation handoff");
    }));
    assert_eq!(
        result
            .expect_err("simulated assertion must unwind")
            .downcast_ref::<&str>(),
        Some(&"simulated assertion before invocation handoff"),
    );
    assert_eq!(group.state(task), None);
}

#[test]
fn test_task_guard_completes_a_suspended_task_on_unwind() {
    let group = TaskGroup::new();
    let mut context = context(std::ptr::null_mut());
    let task = unsafe { group.spawn(suspend_without_completion, &mut context) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pending = TestTask::new(&group, task, true);
        pending.wait_for_invocation();
        assert_eq!(group.state(task), Some(TaskState::Sleeping));
        panic!("simulated assertion after invocation handoff");
    }));
    assert_eq!(
        result
            .expect_err("simulated assertion must unwind")
            .downcast_ref::<&str>(),
        Some(&"simulated assertion after invocation handoff"),
    );
    assert_eq!(group.state(task), None);
}

#[test]
fn group_runs_branches_concurrently() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut first = context((&barrier as *const Barrier).cast_mut().cast());
    let mut second = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: both contexts and the barrier outlive group.close().
    let first = unsafe { group.spawn(wait_for_peer, &mut first) };
    let second = unsafe { group.spawn(wait_for_peer, &mut second) };
    assert_eq!(group.join(first), Some(TaskState::Completed));
    assert_eq!(group.join(second), Some(TaskState::Completed));
    group.close();
}

#[test]
fn fixed_worker_pool_drains_a_burst_of_tasks() {
    let group = TaskGroup::new();
    let counter = AtomicUsize::new(0);
    let capture = (&counter as *const AtomicUsize).cast_mut().cast();
    let mut contexts = (0..64).map(|_| context(capture)).collect::<Vec<_>>();
    let tasks = contexts
        .iter_mut()
        .map(|context| unsafe { group.spawn(increment_atomic, context) })
        .collect::<Vec<_>>();
    for task in tasks {
        assert_eq!(group.join(task), Some(TaskState::Completed));
    }
    assert_eq!(counter.load(Ordering::Acquire), 64);
    group.close();
}

#[test]
fn cancellation_waits_for_task_cleanup() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut context = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: context and barrier outlive join.
    let task = unsafe { group.spawn(wait_for_cancel, &mut context) };
    barrier.wait();
    assert!(group.cancel(task));
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    group.close();
}

#[test]
fn runtime_poll_reads_the_current_task_cancellation_token() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut context = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: context and barrier outlive join.
    let task = unsafe { group.spawn(wait_for_runtime_poll, &mut context) };
    barrier.wait();
    assert!(group.cancel(task));
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    group.close();
}

#[test]
fn cancellation_wakes_a_sleeping_task() {
    let group = TaskGroup::new();
    let mut context = context(std::ptr::null_mut());
    // SAFETY: context outlives join and the test cleanup guard.
    let task = unsafe { group.spawn(sleep_for_test, &mut context) };
    let sleeping = TestTask::new(&group, task, false);
    sleeping.wait_for_sleeping();
    let started = Instant::now();
    assert!(group.cancel(task));
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(500));
    group.close();
}

#[test]
fn cancellation_cleans_up_a_pending_continuation_without_resuming_it() {
    CANCELLED_CONTINUATION_DISPATCHES.store(0, Ordering::SeqCst);
    let group = TaskGroup::new();
    let continuation = crate::runtime::continuation::jk_continuation_new(1);
    let _continuation_cleanup = TestContinuation(continuation);
    assert!(!continuation.is_null());
    assert!(!unsafe {
        crate::runtime::continuation::jk_continuation_alloc_frame(continuation, 16)
    }
    .is_null());
    assert!(!unsafe {
        crate::runtime::continuation::jk_continuation_alloc_spill(continuation, 16)
    }
    .is_null());
    assert!(!unsafe {
        crate::runtime::continuation::jk_continuation_alloc_result(continuation, 8)
    }
    .is_null());
    let mut context = context(
        (&continuation as *const *mut crate::runtime::continuation::Continuation)
            .cast_mut()
            .cast(),
    );
    let task = unsafe { group.spawn(continuation_sleep_task, &mut context) };
    let pending = TestTask::new(&group, task, false);
    pending.wait_for_invocation();
    assert_eq!(group.state(task), Some(TaskState::Sleeping));
    assert!(group.cancel(task));
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    assert!(context.cancelled_result);
    assert_eq!(
        unsafe { crate::runtime::continuation::jk_continuation_state(continuation) },
        crate::runtime::continuation::ContinuationState::Cancelled as u8
    );
    assert!(
        unsafe { crate::runtime::continuation::jk_continuation_frame_pointer(continuation) }
            .is_null()
    );
    assert!(
        unsafe { crate::runtime::continuation::jk_continuation_result_pointer(continuation) }
            .is_null()
    );
    assert!(
        unsafe { crate::runtime::continuation::jk_continuation_spill_pointer(continuation) }
            .is_null()
    );
    assert_eq!(CANCELLED_CONTINUATION_DISPATCHES.load(Ordering::SeqCst), 0);
    group.close();
}

#[test]
fn abort_cancels_siblings_and_records_the_first_failure() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut failing = context((&barrier as *const Barrier).cast_mut().cast());
    let mut sibling = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: the contexts and barrier live until both tasks have joined.
    let failing = unsafe { group.spawn(abort_after_sync, &mut failing) };
    let sibling = unsafe { group.spawn(wait_for_cancel, &mut sibling) };

    assert_eq!(group.join(failing), Some(TaskState::Aborted));
    assert_eq!(group.join(sibling), Some(TaskState::Cancelled));
    assert_eq!(group.failure_snapshot(), Some((73, Vec::new())));
    group.close();
}

#[test]
fn abort_copies_payload_before_the_child_frame_returns() {
    let group = TaskGroup::new();
    let barrier = Barrier::new(2);
    let mut failing = context((&barrier as *const Barrier).cast_mut().cast());
    let mut sibling = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: the contexts and barrier live until both tasks have joined.
    let failing = unsafe { group.spawn(abort_payload_after_sync, &mut failing) };
    let sibling = unsafe { group.spawn(wait_for_cancel, &mut sibling) };

    assert_eq!(group.join(failing), Some(TaskState::Aborted));
    assert_eq!(group.join(sibling), Some(TaskState::Cancelled));
    let (operation, payload) = group
        .failure_snapshot()
        .expect("task failure should be recorded");
    assert_eq!(operation, 74);
    assert_eq!(payload.len(), 16);
    assert_eq!(payload[0..8], 17_u64.to_ne_bytes());
    assert_eq!(payload[8..16], 29_u64.to_ne_bytes());
    // SAFETY: the group stays live while these borrowed ABI views are read.
    unsafe {
        assert_eq!(jk_task_group_failure_operation(&group), 74);
        assert_eq!(jk_task_group_failure_payload_size(&group), 16);
        let payload = jk_task_group_failure_payload(&group);
        assert!(!payload.is_null());
        assert_eq!(*(payload as *const u64), 17);
        assert_eq!(*((payload as *const u64).add(1)), 29);
    }
    group.close();
}

#[test]
fn failure_payload_transfers_only_once() {
    let group = TaskGroup::new();
    let failure = TaskFailure {
        operation: 81,
        payload: vec![1, 2, 3],
        drop_payload: None,
    };
    assert!(group.inner.record_abort(TaskId(0), failure).is_ok());
    assert_eq!(group.failure_operation(), Some(81));
    group.claim_failure();
    assert_eq!(group.failure_operation(), None);
    assert!(group.take_failure().is_none());
    group.claim_failure();
    group.close();
}

#[test]
fn race_cancels_losers_after_a_winner_completes() {
    let group = TaskGroup::new();
    let counter = Mutex::<u32>::new(0);
    let barrier = Barrier::new(2);
    let mut winner = context((&counter as *const Mutex<u32>).cast_mut().cast());
    let mut loser = context((&barrier as *const Barrier).cast_mut().cast());
    // SAFETY: all test storage outlives race(), which joins both tasks.
    let winner = unsafe { group.spawn(complete, &mut winner) };
    let loser = unsafe { group.spawn(wait_for_cancel, &mut loser) };
    barrier.wait();
    assert_eq!(group.race(&[winner, loser]), Some(winner));
    assert_eq!(group.state(loser), Some(TaskState::Cancelled));
    assert_eq!(*counter.lock().expect("counter mutex"), 1);
    group.close();
}

#[test]
fn close_drops_an_unclaimed_task_result_once() {
    let _guard = CLOSE_RESULT_TEST_LOCK
        .lock()
        .expect("task result test mutex");
    CLOSE_RESULT_DROPS.store(0, Ordering::Release);
    let group = TaskGroup::new();
    let mut result = 0_u64;
    let mut context = result_context(&mut result, count_close_result_drop);
    // SAFETY: result storage remains live through close().
    unsafe { group.spawn(write_result, &mut context) };
    group.close();
    assert_eq!(CLOSE_RESULT_DROPS.load(Ordering::Acquire), 1);
}

unsafe extern "C-unwind" fn write_result_then_wait_for_cancel(context: *mut TaskContext) {
    // SAFETY: test setup provides a u64 result slot and an AtomicBool capture
    // that remains live until group.close().
    unsafe {
        *((*context).result as *mut u64) = 7;
        (*context).result_initialized = true;
        let entered = &*((*context).captures as *const AtomicBool);
        entered.store(true, Ordering::Release);
    }
    while !unsafe { (*context).is_cancelled() } {
        thread::yield_now();
    }
}

#[test]
fn cancellation_racing_completion_cleans_a_written_result_exactly_once() {
    let _guard = CLOSE_RESULT_TEST_LOCK
        .lock()
        .expect("task result test mutex");
    // Cancel before the task runs, while it runs, and after it signalled the
    // result write: in every interleaving the written value is cleaned by
    // exactly one party (never zero cleanups while written, never two).
    for cancel_after_enter in [false, true] {
        CLOSE_RESULT_DROPS.store(0, Ordering::Release);
        let group = TaskGroup::new();
        let mut result = 0_u64;
        let entered = AtomicBool::new(false);
        let mut context = result_context(&mut result, count_close_result_drop);
        context.captures = (&entered as *const AtomicBool).cast_mut().cast();
        // SAFETY: the context, result slot, and capture outlive group.close().
        let task = unsafe { group.spawn(write_result_then_wait_for_cancel, &mut context) };
        if cancel_after_enter {
            while !entered.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
        group.inner.cancel_all();
        assert_eq!(group.join(task), Some(TaskState::Cancelled));
        group.close();
        assert_eq!(
            u64::from(entered.load(Ordering::Acquire)),
            CLOSE_RESULT_DROPS.load(Ordering::Acquire),
            "a written task result must be cleaned exactly once across the cancel race"
        );
    }
}

#[test]
fn external_completion_racing_cancellation_reaches_one_terminal_state() {
    let group = TaskGroup::new();
    let mut context = context(std::ptr::null_mut());
    let task = unsafe { group.spawn(suspend_without_completion, &mut context) };
    let pending = TestTask::new(&group, task, true);
    pending.wait_for_invocation();
    assert_eq!(group.state(task), Some(TaskState::Sleeping));
    struct ContextPointer(*mut TaskContext);
    impl ContextPointer {
        // A method call makes the closure capture the whole wrapper; a
        // direct `.0` field use would capture the raw pointer itself
        // (edition-2021 disjoint capture), which is not Send.
        fn get(self) -> *mut TaskContext {
            self.0
        }
    }
    // SAFETY: the group keeps the pointed-to context alive until close(),
    // which this test calls only after the thread has finished.
    unsafe impl Send for ContextPointer {}
    let context_pointer = ContextPointer(&mut context as *mut TaskContext);
    let completer = thread::spawn(move || {
        // The provider-completion path may run on any thread while the group
        // keeps the context alive until close().
        complete_suspended_task(context_pointer.get());
    });
    // Scope cancellation races the external completion. Whichever wins, the
    // task must end in exactly one terminal state and close must not hang.
    group.inner.cancel_all();
    completer.join().expect("external completer thread");
    let state = group.join(task);
    assert!(matches!(
        state,
        Some(TaskState::Completed) | Some(TaskState::Cancelled)
    ));
    group.close();
}

#[test]
fn heap_task_result_pointer_borrows_until_claim() {
    let _guard = CLOSE_RESULT_TEST_LOCK
        .lock()
        .expect("task result test mutex");
    for claim in [false, true] {
        CLOSE_RESULT_DROPS.store(0, Ordering::Release);
        let group = jk_task_group_new(0);
        // No native result or context storage survives this ABI call.
        let task = unsafe {
            jk_task_spawn_heap(
                group,
                write_result as *mut c_void,
                std::ptr::null(),
                0,
                8,
                count_close_result_drop as *mut c_void,
            )
        };
        unsafe { jk_task_join(group, task) };
        let result = unsafe { jk_task_result_pointer(group, task) };
        assert_eq!(unsafe { result.cast::<u64>().read_unaligned() }, 7);
        assert_eq!(CLOSE_RESULT_DROPS.load(Ordering::Acquire), 0);
        if claim {
            unsafe { jk_task_claim_result(group, task) };
        }
        unsafe { jk_task_group_free(group) };
        assert_eq!(
            CLOSE_RESULT_DROPS.load(Ordering::Acquire),
            u64::from(!claim)
        );
    }
}

#[test]
fn race_transfers_winner_and_drops_loser_result_once() {
    RACE_RESULT_DROPS.store(0, Ordering::Release);
    let group = TaskGroup::new();
    let mut first_result = 0_u64;
    let mut second_result = 0_u64;
    let mut output = 0_u64;
    let ready = AtomicUsize::new(0);
    let mut first = result_context(&mut first_result, count_race_result_drop);
    let mut second = result_context(&mut second_result, count_race_result_drop);
    first.captures = (&ready as *const AtomicUsize).cast_mut().cast();
    second.captures = (&ready as *const AtomicUsize).cast_mut().cast();
    // SAFETY: all contexts and result storage remain live through race().
    let first = unsafe { group.spawn(write_result_and_mark, &mut first) };
    let second = unsafe { group.spawn(write_result_and_mark, &mut second) };
    while ready.load(Ordering::Acquire) != 2 {
        thread::yield_now();
    }
    let winner = unsafe {
        group.race_into(
            &[first, second],
            (&mut output as *mut u64).cast(),
            std::mem::size_of::<u64>(),
        )
    };
    assert!(matches!(winner, Some(task) if task == first || task == second));
    assert_eq!(output, 7);
    assert_eq!(RACE_RESULT_DROPS.load(Ordering::Acquire), 1);
    group.close();
    assert_eq!(RACE_RESULT_DROPS.load(Ordering::Acquire), 1);
}

#[test]
fn cancellation_drains_cown_leases_after_task_returns() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let cown = crate::runtime::managed::jk_cown_new(
        crate::runtime::managed::jk_alloc_object(
            crate::runtime::managed::RuntimeValueKind::Class as u8,
            8,
            8,
        ),
        None,
    );
    let entered = AtomicBool::new(false);
    let captures = [
        crate::runtime::managed::jk_dup(cown) as usize,
        (&entered as *const AtomicBool) as usize,
    ];
    let mut context = context((&captures as *const [usize; 2]).cast_mut().cast());
    let group = TaskGroup::new();
    // SAFETY: captures and Cown remain live until the task has joined.
    let task = unsafe { group.spawn(hold_cown_until_cancel, &mut context) };
    for _ in 0..100 {
        if entered.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(entered.load(Ordering::Acquire));
    assert!(group.cancel(task));
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    let probe = crate::runtime::managed::jk_dup(cown);
    let release_probe = crate::runtime::managed::jk_dup(cown);
    assert!(!crate::runtime::managed::jk_cown_acquire(probe).is_null());
    crate::runtime::managed::jk_cown_release(release_probe);
    group.close();
    crate::runtime::managed::jk_drop(cown);
    region_scope.close_and_wait();
}

#[test]
fn panic_aborts_task_cancels_sibling_and_drains_cown_lease() {
    let region_scope = crate::runtime::scope::RuntimeScope::new();
    let _region_scope_guard = region_scope.enter();
    let cown = crate::runtime::managed::jk_cown_new(
        crate::runtime::managed::jk_alloc_object(
            crate::runtime::managed::RuntimeValueKind::Class as u8,
            8,
            8,
        ),
        None,
    );
    let barrier = Barrier::new(3);
    let captures = [
        crate::runtime::managed::jk_dup(cown) as usize,
        (&barrier as *const Barrier) as usize,
        0,
    ];
    let mut failing = context((&captures as *const [usize; 3]).cast_mut().cast());
    let mut sibling = context((&barrier as *const Barrier).cast_mut().cast());
    let group = TaskGroup::new();
    // SAFETY: captures, barrier and Cown remain live through both joins.
    let failing = unsafe { group.spawn(acquire_cown_then_panic, &mut failing) };
    let sibling = unsafe { group.spawn(wait_for_cancel, &mut sibling) };
    barrier.wait();
    assert_eq!(group.join(failing), Some(TaskState::Aborted));
    assert_eq!(group.join(sibling), Some(TaskState::Cancelled));
    assert_eq!(group.failure_operation(), Some(TASK_PANIC_ABORT));

    // The panic path must release the lease even though no generated
    // release statement ran after the unwinding thunk.
    let probe = crate::runtime::managed::jk_dup(cown);
    let release_probe = crate::runtime::managed::jk_dup(cown);
    assert!(!crate::runtime::managed::jk_cown_acquire(probe).is_null());
    crate::runtime::managed::jk_cown_release(release_probe);
    group.close();
    crate::runtime::managed::jk_drop(cown);
    region_scope.close_and_wait();
}

#[test]
fn panic_skips_uninitialized_result_drop() {
    let _guard = CLOSE_RESULT_TEST_LOCK
        .lock()
        .expect("task result test mutex");
    CLOSE_RESULT_DROPS.store(0, Ordering::Release);
    let group = TaskGroup::new();
    let mut result = 0_u64;
    let mut context = result_context(&mut result, count_close_result_drop);
    // SAFETY: result storage remains live through close().
    let task = unsafe { group.spawn(panic_without_result, &mut context) };
    assert_eq!(group.join(task), Some(TaskState::Aborted));
    group.close();
    assert_eq!(CLOSE_RESULT_DROPS.load(Ordering::Acquire), 0);
}

fn cown_test_wait_until(ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "Cown task did not make progress");
        thread::sleep(Duration::from_millis(1));
    }
}

unsafe extern "C-unwind" fn wait_for_contended_cown(context: *mut TaskContext) {
    let cown = *(*context).captures.cast::<*mut u8>();
    if !crate::runtime::managed::jk_cown_acquire(cown).is_null() {
        crate::runtime::managed::jk_cown_release(cown);
    }
}

#[test]
fn cancellation_wakes_a_cown_waiter_without_releasing_the_holder() {
    use crate::runtime::managed::*;
    let scope = RuntimeScope::new();
    let _scope = scope.enter();
    let cown = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    assert!(!jk_cown_acquire(cown).is_null());
    let mut capture = cown;
    let mut context = context((&mut capture as *mut *mut u8).cast());
    let group = TaskGroup::new();
    let task = unsafe { group.spawn(wait_for_contended_cown, &mut context) };
    cown_test_wait_until(|| cown_waiter_count(cown) == 1);
    assert!(group.cancel(task));
    cown_test_wait_until(|| cown_waiter_count(cown) == 0);
    assert_eq!(group.join(task), Some(TaskState::Cancelled));
    // Cancellation must finish even while the unrelated owner retains its lease.
    jk_cown_release(cown);
    group.close();
    scope.close_and_wait();
}

#[test]
fn parked_cown_worker_helps_newly_enqueued_work() {
    // Also run alone with JOKY_WORKER_COUNT=1: spare workers cannot hide a
    // missed scheduler notification to the worker parked on the Cown.
    use crate::runtime::managed::*;
    unsafe extern "C-unwind" fn mark(context: *mut TaskContext) {
        (*(*context).captures.cast::<AtomicBool>()).store(true, Ordering::Release);
    }
    let scope = RuntimeScope::new();
    let _scope = scope.enter();
    let cown = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    assert!(!jk_cown_acquire(cown).is_null());
    let mut capture = cown;
    let mut waiter = context((&mut capture as *mut *mut u8).cast());
    let group = TaskGroup::new();
    let waiting = unsafe { group.spawn(wait_for_contended_cown, &mut waiter) };
    cown_test_wait_until(|| cown_waiter_count(cown) == 1);
    let ran = AtomicBool::new(false);
    let mut work = context((&ran as *const AtomicBool).cast_mut().cast());
    let helping = unsafe { group.spawn(mark, &mut work) };
    cown_test_wait_until(|| ran.load(Ordering::Acquire));
    assert_eq!(group.join(helping), Some(TaskState::Completed));
    jk_cown_release(cown);
    assert_eq!(group.join(waiting), Some(TaskState::Completed));
    group.close();
    scope.close_and_wait();
}

#[test]
fn selected_outer_cown_waiter_does_not_strand_its_helped_task() {
    // Run with one worker to force the second task onto the first task's stack.
    use crate::runtime::managed::*;
    struct Capture {
        cown: *mut u8,
        entered: AtomicUsize,
        completed: AtomicUsize,
    }
    unsafe extern "C-unwind" fn acquire(context: *mut TaskContext) {
        let capture = &*(*context).captures.cast::<Capture>();
        capture.entered.fetch_add(1, Ordering::Release);
        assert!(!jk_cown_acquire(capture.cown).is_null());
        jk_cown_release(capture.cown);
        capture.completed.fetch_add(1, Ordering::Release);
    }
    let scope = RuntimeScope::new();
    let _scope = scope.enter();
    let cown = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    assert!(!jk_cown_acquire(cown).is_null());
    let mut capture = Capture {
        cown,
        entered: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
    };
    let mut first = context((&mut capture as *mut Capture).cast());
    let mut second = context((&mut capture as *mut Capture).cast());
    let group = TaskGroup::new();
    let outer = unsafe { group.spawn(acquire, &mut first) };
    cown_test_wait_until(|| cown_waiter_count(cown) == 1);
    let inner = unsafe { group.spawn(acquire, &mut second) };
    cown_test_wait_until(|| capture.entered.load(Ordering::Acquire) == 2);
    jk_cown_release(cown);
    cown_test_wait_until(|| capture.completed.load(Ordering::Acquire) == 2);
    assert_eq!(group.join(inner), Some(TaskState::Completed));
    assert_eq!(group.join(outer), Some(TaskState::Completed));
    group.close();
    scope.close_and_wait();
}

#[test]
fn task_cleanup_releases_large_cown_batches_and_reuses_local_storage() {
    use crate::runtime::managed::*;
    unsafe extern "C-unwind" fn acquire(context: *mut TaskContext) {
        let handles = std::slice::from_raw_parts((*context).captures.cast::<*mut u8>(), 9);
        let mut payloads = [std::ptr::null_mut(); 9];
        for _ in 0..8 {
            assert_eq!(
                jk_cown_acquire_many(handles.as_ptr(), 9, payloads.as_mut_ptr()),
                1
            );
            for &handle in handles.iter().rev() {
                jk_cown_release(handle);
            }
        }
        // Simulate an exit that bypasses generated lease release code.
        assert_eq!(
            jk_cown_acquire_many(handles.as_ptr(), 9, payloads.as_mut_ptr()),
            1
        );
    }
    let scope = RuntimeScope::new();
    let _scope = scope.enter();
    let mut handles = [std::ptr::null_mut(); 9];
    for handle in &mut handles {
        *handle = jk_cown_new(jk_alloc_object(RuntimeValueKind::Class as u8, 8, 8), None);
    }
    let mut task_context = context(handles.as_mut_ptr().cast());
    let group = TaskGroup::new();
    let task = unsafe { group.spawn(acquire, &mut task_context) };
    assert_eq!(group.join(task), Some(TaskState::Completed));
    for handle in handles {
        assert!(!jk_cown_acquire(handle).is_null());
        jk_cown_release(handle);
    }
    group.close();
    scope.close_and_wait();
}
