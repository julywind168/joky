use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use super::super::scope::RuntimeScope;
use super::{cleanup_task_cown_leases, TaskContext, TaskGroupInner, TaskState};

thread_local! {
    static CURRENT_WORKER_INDEX: std::cell::Cell<usize> =
        const { std::cell::Cell::new(usize::MAX) };
    static CURRENT_WORKER_QUEUE: std::cell::Cell<*const Worker<TaskJob>> =
        const { std::cell::Cell::new(std::ptr::null()) };
    static CURRENT_HELP_DEPTH: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Wake state shared by a task and the scheduler-facing cancellation path.
pub(super) struct TaskWake {
    pub(super) state: Mutex<()>,
    pub(super) signal: Condvar,
    pub(super) cown_leases: Mutex<super::current::CownLeases>,
    pub(super) cown_waiter: Mutex<Weak<crate::runtime::cown::Waiter>>,
}

impl TaskWake {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(()),
            signal: Condvar::new(),
            cown_leases: Mutex::default(),
            cown_waiter: Mutex::default(),
        }
    }

    pub(super) fn cancel(&self, token: &AtomicBool) {
        // Serialize the cancellation store with the sleeper's predicate
        // check. This closes the small window where a notification could be
        // delivered just before the waiter commits to `wait_timeout`.
        let _guard = self.state.lock().expect("task wake mutex");
        token.store(true, Ordering::Release);
        self.signal.notify_all();
        if let Some(waiter) = self.cown_waiter.lock().expect("task Cown waiter").upgrade() {
            waiter.notify();
        }
    }
}

pub(super) struct TaskControl {
    pub(super) cancellation: AtomicBool,
    pub(super) wake: Arc<TaskWake>,
    pub(super) state: Mutex<TaskState>,
    completed: Condvar,
}

impl TaskControl {
    pub(super) fn new() -> Self {
        Self {
            cancellation: AtomicBool::new(false),
            wake: Arc::new(TaskWake::new()),
            state: Mutex::new(TaskState::Pending),
            completed: Condvar::new(),
        }
    }

    pub(super) fn finish(&self, state: TaskState) {
        *self.state.lock().expect("task state mutex") = state;
        self.completed.notify_all();
    }

    pub(super) fn set_sleeping(&self) {
        *self.state.lock().expect("task state mutex") = TaskState::Sleeping;
    }

    pub(super) fn set_running(&self) {
        let mut state = self.state.lock().expect("task state mutex");
        if *state == TaskState::Sleeping {
            *state = TaskState::Running;
        }
    }

    pub(super) fn wait(&self) -> TaskState {
        let mut state = self.state.lock().expect("task state mutex");
        while matches!(
            *state,
            TaskState::Pending | TaskState::Running | TaskState::Sleeping
        ) {
            state = self.completed.wait(state).expect("task state mutex");
        }
        *state
    }
}

pub(super) struct TaskInvocation {
    pub(super) thunk: super::TaskThunk,
    pub(super) context: *mut TaskContext,
    pub(super) control: *const TaskControl,
    pub(super) scope: Arc<RuntimeScope>,
    pub(super) region: Arc<crate::runtime::region::Region>,
}

unsafe impl Send for TaskInvocation {}

impl TaskInvocation {
    pub(super) unsafe fn invoke(self) {
        let _scope_guard = self.scope.enter();
        let _region_guard = crate::runtime::region::install(self.region);
        let previous = super::CURRENT_CANCELLATION.with(|current| {
            let previous = current.get();
            // SAFETY: TaskGroup::spawn installed the task control pointer and
            // keeps it alive for this thread's entire invocation.
            current.set(unsafe { (*self.context).cancellation });
            previous
        });
        let previous_context = super::CURRENT_TASK_CONTEXT.with(|current| {
            let previous = current.get();
            current.set(self.context);
            previous
        });
        let previous_control = super::CURRENT_TASK_CONTROL.with(|current| {
            let previous = current.get();
            current.set(self.control);
            previous
        });
        let previous_handler =
            unsafe { crate::runtime::handler::install((*self.context).handler_frame) };
        struct InvocationGuard {
            cancellation: *const AtomicBool,
            context: *mut TaskContext,
            control: *const TaskControl,
            handler: *const crate::runtime::handler::HandlerFrame,
        }
        impl Drop for InvocationGuard {
            fn drop(&mut self) {
                super::CURRENT_CANCELLATION.with(|current| current.set(self.cancellation));
                super::CURRENT_TASK_CONTEXT.with(|current| current.set(self.context));
                super::CURRENT_TASK_CONTROL.with(|current| current.set(self.control));
                // SAFETY: the handler frame was installed by this invocation
                // and remains valid until the enclosing task scope exits.
                unsafe { crate::runtime::handler::install(self.handler) };
            }
        }
        let _guard = InvocationGuard {
            cancellation: previous,
            context: previous_context,
            control: previous_control,
            handler: previous_handler,
        };
        // SAFETY: upheld by TaskGroup::spawn.
        unsafe { (self.thunk)(self.context) };
    }
}

pub(super) struct TaskScheduler {
    injector: Injector<TaskJob>,
    stealers: Vec<Stealer<TaskJob>>,
    pending_jobs: AtomicUsize,
    wake: Condvar,
    wake_state: Mutex<()>,
    cown_helpers: AtomicU64,
    worker_threads: Vec<OnceLock<thread::Thread>>,
}

enum TaskJob {
    Invocation {
        invocation: TaskInvocation,
        control: Arc<TaskControl>,
        group: Arc<TaskGroupInner>,
        context: usize,
    },
    Callback(Box<dyn FnOnce() + Send + 'static>),
}

static TASK_SCHEDULER: OnceLock<Arc<TaskScheduler>> = OnceLock::new();

/// The worker pool is sized at 2x available parallelism (clamped 8..32) so a
/// worker blocked on a mutex or the blocking pool does not stall task
/// throughput. `JOKY_WORKER_COUNT` overrides it: fairness and starvation
/// observations need to run the same program under different worker counts,
/// and a process-wide environment knob keeps that repeatable without adding a
/// configuration surface to the compiler.
fn configured_worker_count() -> usize {
    if let Ok(value) = std::env::var("JOKY_WORKER_COUNT") {
        match value.trim().parse::<usize>() {
            Ok(count) if (1..=64).contains(&count) => return count,
            _ => eprintln!("ignoring invalid JOKY_WORKER_COUNT '{value}'"),
        }
    }
    thread::available_parallelism()
        .map(|count| count.get().saturating_mul(2))
        .unwrap_or(16)
        .clamp(8, 32)
}

pub(super) fn task_scheduler() -> &'static Arc<TaskScheduler> {
    TASK_SCHEDULER.get_or_init(|| {
        let worker_count = configured_worker_count();
        let mut workers = Vec::with_capacity(worker_count);
        let mut stealers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let worker = Worker::new_fifo();
            stealers.push(worker.stealer());
            workers.push(worker);
        }
        let scheduler = Arc::new(TaskScheduler {
            injector: Injector::new(),
            stealers,
            pending_jobs: AtomicUsize::new(0),
            wake: Condvar::new(),
            wake_state: Mutex::new(()),
            cown_helpers: AtomicU64::new(0),
            worker_threads: (0..worker_count).map(|_| OnceLock::new()).collect(),
        });
        for (index, worker) in workers.into_iter().enumerate() {
            let scheduler = Arc::clone(&scheduler);
            thread::Builder::new()
                .name("joky-worker".to_string())
                .spawn(move || task_worker_loop(scheduler, index, worker))
                .expect("failed to start Joky worker");
        }
        scheduler
    })
}

pub(super) fn enqueue_invocation(
    invocation: TaskInvocation,
    control: Arc<TaskControl>,
    group: Arc<TaskGroupInner>,
    context: *mut TaskContext,
) {
    task_scheduler().enqueue(TaskJob::Invocation {
        invocation,
        control,
        group,
        context: context as usize,
    });
}

impl TaskScheduler {
    fn enqueue(&self, job: TaskJob) {
        self.pending_jobs.fetch_add(1, Ordering::AcqRel);
        self.injector.push(job);
        self.wake.notify_one();
        // A worker parked on a Cown can still service unrelated queued work.
        // Registration precedes the worker's last queue check; unpark's token
        // survives a notification delivered just before park.
        // Pair an RMW with helper registration: either we observe its bit,
        // or its acquire observes our release after publishing the job.
        let helpers = self.cown_helpers.fetch_or(0, Ordering::SeqCst);
        if helpers != 0 {
            if let Some(worker) = self.worker_threads[helpers.trailing_zeros() as usize].get() {
                worker.unpark();
            }
        }
    }

    pub(super) fn enqueue_callback(&self, callback: Box<dyn FnOnce() + Send + 'static>) {
        self.enqueue(TaskJob::Callback(callback));
    }

    fn steal_one(&self, local: &Worker<TaskJob>, index: usize) -> Option<TaskJob> {
        loop {
            let mut retry = false;
            match self.injector.steal_batch_and_pop(local) {
                Steal::Success(job) => return Some(job),
                Steal::Retry => retry = true,
                Steal::Empty => {}
            }
            for (other_index, stealer) in self.stealers.iter().enumerate() {
                if other_index == index {
                    continue;
                }
                match stealer.steal_batch_and_pop(local) {
                    Steal::Success(job) => return Some(job),
                    Steal::Retry => retry = true,
                    Steal::Empty => {}
                }
            }
            // A parked Cown helper has no timeout to recover from mistaking
            // a concurrent steal for an empty queue. Resolve Retry first.
            if !retry {
                return None;
            }
            std::hint::spin_loop();
        }
    }

    pub(super) fn help_one(&self) -> bool {
        let index = CURRENT_WORKER_INDEX.with(std::cell::Cell::get);
        if index == usize::MAX || CURRENT_HELP_DEPTH.with(std::cell::Cell::get) != 0 {
            return false;
        }
        let local = CURRENT_WORKER_QUEUE.with(|queue| queue.get());
        if local.is_null() {
            return false;
        }
        // SAFETY: the pointer is installed by the worker thread and points to
        // its stack-owned deque, which remains live for the worker lifetime.
        let Some(job) = (unsafe { &*local }).pop().or_else(|| {
            // SAFETY: see above; only the owning worker calls help_one.
            self.steal_one(unsafe { &*local }, index)
        }) else {
            return false;
        };
        self.cown_helpers
            .fetch_and(!(1_u64 << index), Ordering::SeqCst);
        let previous = CURRENT_HELP_DEPTH.with(|depth| depth.replace(1));
        execute_task_job(job);
        CURRENT_HELP_DEPTH.with(|depth| depth.set(previous));
        true
    }

    pub(super) fn help_until(&self, control: &TaskControl) {
        while matches!(
            *control.state.lock().expect("task state mutex"),
            TaskState::Pending | TaskState::Running | TaskState::Sleeping
        ) {
            if !self.help_one() {
                thread::yield_now();
            }
        }
    }
}

fn task_worker_loop(scheduler: Arc<TaskScheduler>, index: usize, local: Worker<TaskJob>) {
    let _ = scheduler.worker_threads[index].set(thread::current());
    CURRENT_WORKER_INDEX.with(|worker| worker.set(index));
    CURRENT_WORKER_QUEUE.with(|queue| queue.set(&local));
    let mut empty_spins: u32 = 0;
    loop {
        if let Some(job) = local.pop().or_else(|| scheduler.steal_one(&local, index)) {
            empty_spins = 0;
            execute_task_job(job);
            continue;
        }
        if scheduler.pending_jobs.load(Ordering::Acquire) > 0 {
            // Work exists but is not reachable by pop/steal: it is in flight
            // on another worker or mid-handoff. Sleeping here would sleep
            // through the enqueue notify and delay the job by the full
            // timeout, so back off briefly and retry instead.
            empty_spins += 1;
            if empty_spins < 16 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros(100));
            }
            continue;
        }
        empty_spins = 0;
        let guard = scheduler
            .wake_state
            .lock()
            .expect("task scheduler wake mutex");
        // Re-check under the wake mutex so an enqueue that raced the empty
        // poll cannot be slept through.
        if scheduler.pending_jobs.load(Ordering::Acquire) > 0 {
            drop(guard);
            continue;
        }
        let _ = scheduler
            .wake
            .wait_timeout(guard, Duration::from_millis(2))
            .expect("task scheduler wake mutex");
    }
}

fn execute_task_job(job: TaskJob) {
    let scheduler = task_scheduler();
    struct JobGuard<'a>(&'a TaskScheduler);
    impl Drop for JobGuard<'_> {
        fn drop(&mut self) {
            self.0.pending_jobs.fetch_sub(1, Ordering::AcqRel);
            self.0.wake.notify_all();
        }
    }
    let _job_guard = JobGuard(scheduler);
    let TaskJob::Invocation {
        invocation,
        control,
        group,
        context: context_address,
    } = job
    else {
        if let TaskJob::Callback(callback) = job {
            let _ = catch_unwind(AssertUnwindSafe(callback));
        }
        return;
    };
    if control.cancellation.load(Ordering::Acquire) {
        unsafe {
            (*(context_address as *mut TaskContext)).cancelled_result = true;
            (*(context_address as *mut TaskContext)).result_initialized = false;
            cleanup_task_cown_leases(context_address as *mut TaskContext);
        };
        control.finish(TaskState::Cancelled);
        group.mark_completed();
        return;
    }

    *control.state.lock().expect("task state mutex") = TaskState::Running;
    let invocation_result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: TaskGroup::spawn establishes the context lifetime contract.
        unsafe {
            crate::runtime::continuation::with_task_function_pending_boundary(
                context_address as *mut TaskContext,
                || invocation.invoke(),
            )
        }
    }));
    if invocation_result.is_err() {
        handle_task_panic(&control, &group, context_address as *mut TaskContext);
        return;
    }
    if matches!(invocation_result, Ok(true)) {
        return;
    }

    let context = context_address as *mut TaskContext;
    if unsafe {
        !(*context).continuation_pending.load(Ordering::Acquire)
            && (*context).abort_operation == super::NO_TASK_ABORT
    } {
        unsafe { (*context).result_initialized = true };
    }
    let aborted = unsafe { (*context).abort_operation } != super::NO_TASK_ABORT;
    if unsafe { (*context).continuation_pending.load(Ordering::Acquire) } {
        return;
    }
    unsafe { cleanup_task_cown_leases(context) };
    let state = if aborted {
        TaskState::Aborted
    } else if control.cancellation.load(Ordering::Acquire) {
        TaskState::Cancelled
    } else {
        TaskState::Completed
    };
    control.finish(state);
    group.mark_completed();
}

fn handle_task_panic(control: &TaskControl, group: &TaskGroupInner, context: *mut TaskContext) {
    unsafe {
        (*context).abort_operation = super::TASK_PANIC_ABORT;
        (*context).cancelled_result = true;
        (*context).result_initialized = false;
        (*context)
            .continuation_pending
            .store(false, Ordering::Release);
        crate::runtime::continuation::cancel_task_continuation(context);
        cleanup_task_cown_leases(context);
    }
    control.cancellation.store(true, Ordering::Release);
    let _ = group.record_abort(
        unsafe { (*context).task },
        super::TaskFailure {
            operation: super::TASK_PANIC_ABORT,
            payload: Vec::new(),
            drop_payload: None,
        },
    );
    control.finish(TaskState::Aborted);
    group.mark_completed();
}

pub(crate) struct CownHelperGuard(Option<usize>);

impl Drop for CownHelperGuard {
    fn drop(&mut self) {
        if let Some(index) = self.0 {
            task_scheduler()
                .cown_helpers
                .fetch_and(!(1_u64 << index), Ordering::SeqCst);
        }
    }
}

pub(crate) fn register_cown_helper() -> CownHelperGuard {
    let index = CURRENT_WORKER_INDEX.with(std::cell::Cell::get);
    if index == usize::MAX || CURRENT_HELP_DEPTH.with(std::cell::Cell::get) != 0 {
        return CownHelperGuard(None);
    }
    task_scheduler()
        .cown_helpers
        .fetch_or(1_u64 << index, Ordering::SeqCst);
    CownHelperGuard(Some(index))
}

pub(crate) fn current_worker_thread() -> bool {
    CURRENT_WORKER_INDEX.with(|worker| worker.get() != usize::MAX)
}
