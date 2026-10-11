//! Nonblocking, owner-thread task driving with thread-safe wake notifications.
use super::suspension::{Continuation, ContinuationStatus};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Instant,
};

/// Monotonic time supplied by the embedding host. Advance it only forward.
/// A virtual clock may stop while the application's simulation is paused.
pub trait Clock {
    fn now(&self) -> Instant;
    /// Blocking waits require time to progress with real monotonic time.
    /// Virtual/simulation clocks remain suitable for nonblocking host driving.
    fn allows_blocking_wait(&self) -> bool {
        false
    }
}
#[derive(Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn allows_blocking_wait(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

#[derive(Default)]
struct Group {
    closed: Cell<bool>,
    running: Cell<usize>,
    executor: RefCell<Option<Weak<Queue>>>,
}
/// An ownership boundary for roots and all their structured descendants.
/// Cancel it through its executor before releasing external resources.
#[derive(Clone, Default)]
pub struct TaskGroup(Rc<Group>);
impl TaskGroup {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.get()
    }
    pub(crate) fn running(&self) -> usize {
        self.0.running.get()
    }
}

#[derive(Default)]
struct Queue(Mutex<Ready>, Mutex<Option<Arc<mio::Waker>>>);
#[derive(Default)]
struct Ready {
    order: VecDeque<TaskId>,
    set: BTreeSet<TaskId>,
}
impl Ready {
    fn insert(&mut self, id: TaskId) -> bool {
        if self.set.insert(id) {
            self.order.push_back(id);
            true
        } else {
            false
        }
    }
    fn remove(&mut self, id: &TaskId) {
        if self.set.remove(id) {
            self.order.retain(|i| i != id);
        }
    }
    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}
struct WakeState {
    id: TaskId,
    active: AtomicBool,
    queue: Weak<Queue>,
}
/// Cloneable notification token; it never accesses VM data or runs script code.
/// Late notifications after cancellation/completion are ignored. Repeated wakes
/// coalesce. A host must publish its result before waking, and still implement stop.
#[derive(Clone)]
pub struct TaskWaker(Arc<WakeState>);
impl TaskWaker {
    pub fn wake(&self) {
        if let Some(queue) = self.0.queue.upgrade() {
            let mut ready = queue.0.lock().unwrap_or_else(|e| e.into_inner());
            if self.0.active.load(Ordering::Acquire) && ready.insert(self.0.id) {
                if let Some(signal) = queue.1.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    let _ = signal.wake();
                }
            }
        }
    }
    fn deactivate(&self) {
        self.0.active.store(false, Ordering::Release);
        if let Some(queue) = self.0.queue.upgrade() {
            queue
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0.id);
        }
    }
}

struct Context {
    clock: Rc<dyn Clock>,
    waker: TaskWaker,
    reactor: Rc<RefCell<Option<super::reactor::Reactor>>>,
}
thread_local! { static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) }; }
struct ContextGuard(Option<Context>);
impl Drop for ContextGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            c.replace(self.0.take());
        });
    }
}
/// Capture this inside a suspending host callback, then wake after its event fires.
pub fn current_waker() -> Option<TaskWaker> {
    CURRENT.with(|c| c.borrow().as_ref().map(|c| c.waker.clone()))
}
/// Get the current root's owner-thread reactor. Use prepared, unpolled roots so
/// the executor context is available when the first host operation starts.
pub fn current_reactor() -> Result<super::reactor::Reactor, Error> {
    let (reactor, queue) = CURRENT
        .with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|c| c.waker.0.queue.upgrade().map(|q| (c.reactor.clone(), q)))
        })
        .ok_or_else(|| invalid("I/O registration requires a task executor"))?;
    ensure_reactor(&reactor, &queue)
}
fn ensure_reactor(
    reactor: &RefCell<Option<super::reactor::Reactor>>,
    queue: &Queue,
) -> Result<super::reactor::Reactor, Error> {
    let mut reactor = reactor.borrow_mut();
    if reactor.is_none() {
        let created = super::reactor::Reactor::for_executor()?;
        *queue.1.lock().unwrap_or_else(|e| e.into_inner()) = Some(created.signal());
        *reactor = Some(created);
    }
    Ok(reactor.as_ref().unwrap().clone())
}
pub(crate) fn now() -> Instant {
    // Do not hold the TLS borrow while calling host code.
    let clock = CURRENT.with(|c| c.borrow().as_ref().map(|c| c.clock.clone()));
    clock.map_or_else(Instant::now, |c| c.now())
}

pub(crate) trait TaskEnvironment {
    fn resume(&self, task: &mut Continuation) -> Result<(), Error>;
    fn dispose(&self, task: Continuation);
}
struct Entry {
    task: Option<Continuation>,
    environment: Option<Rc<dyn TaskEnvironment>>,
    _lease: Lease,
}
impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            match &self.environment {
                Some(environment) => environment.dispose(task),
                None => drop(task),
            }
        }
    }
}
struct Lease(Rc<Cell<usize>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
struct Record {
    entry: Option<Entry>,
    group: TaskGroup,
    waker: TaskWaker,
    deadline: Option<Instant>,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.waker.deactivate();
    }
}

/// A terminal task has already stopped its children and released task storage.
#[derive(Debug)]
pub struct TaskCompletion {
    pub id: TaskId,
    pub result: Result<u64, Error>,
}
#[derive(Debug, Default)]
pub struct PollReport {
    pub polled: usize,
    pub completed: Vec<TaskCompletion>,
    pub ready: bool,
    pub next_deadline: Option<Instant>,
}

/// Single-threaded executor. Each drive polls a snapshot of ready roots at most
/// once, in FIFO readiness order. No sleeping or implicit polling of external waits.
/// The root-poll limit is cooperative, not a time limit or native preemption.
pub struct HostExecutor {
    tasks: RefCell<BTreeMap<TaskId, Record>>,
    queue: Arc<Queue>,
    clock: Rc<dyn Clock>,
    polling: Cell<bool>,
    max_tasks: usize,
    live: Rc<Cell<usize>>,
    reactor: Rc<RefCell<Option<super::reactor::Reactor>>>,
}
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
fn next_id() -> Result<TaskId, Error> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(TaskId)
        .map_err(|_| invalid("Task identities exhausted"))
}
impl Default for HostExecutor {
    fn default() -> Self {
        Self::new(Rc::new(SystemClock), 4096)
    }
}
impl HostExecutor {
    pub fn new(clock: Rc<dyn Clock>, max_tasks: usize) -> Self {
        Self {
            tasks: RefCell::new(BTreeMap::new()),
            queue: Arc::default(),
            clock,
            polling: Cell::new(false),
            max_tasks,
            live: Rc::new(Cell::new(0)),
            reactor: Default::default(),
        }
    }
    /// Register an unpolled continuation (or adopt an already started one).
    /// Use an unpolled task when injecting a clock or capturing wake tokens at startup.
    pub fn spawn(&self, group: &TaskGroup, task: Continuation) -> Result<TaskId, Error> {
        self.spawn_in(group, task, None)
    }
    pub(crate) fn spawn_in(
        &self,
        group: &TaskGroup,
        task: Continuation,
        environment: Option<Rc<dyn TaskEnvironment>>,
    ) -> Result<TaskId, Error> {
        if group.is_closed() {
            return Err(invalid("Task group is closed"));
        }
        if self.live.get() >= self.max_tasks {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Executor task limit exceeded",
            ));
        }
        let mut owner = group.0.executor.borrow_mut();
        if owner
            .as_ref()
            .is_some_and(|q| !Weak::ptr_eq(q, &Arc::downgrade(&self.queue)))
        {
            return Err(invalid("Task group belongs to another executor"));
        }
        *owner = Some(Arc::downgrade(&self.queue));
        drop(owner);
        let waker = self.new_waker()?;
        let id = waker.0.id;
        let wake = task.wait_plan().wake();
        let deadline = match wake {
            Some(super::driver::Wake::Deadline(d)) => Some(d),
            _ => None,
        };
        if !task.has_started()
            || task.is_finished()
            || matches!(wake, Some(super::driver::Wake::Immediate))
        {
            waker.wake();
        }
        self.live.set(self.live.get() + 1);
        self.tasks.borrow_mut().insert(
            id,
            Record {
                entry: Some(Entry {
                    task: Some(task),
                    environment,
                    _lease: Lease(self.live.clone()),
                }),
                group: group.clone(),
                waker,
                deadline,
            },
        );
        Ok(id)
    }
    pub(crate) fn contains(&self, id: TaskId) -> bool {
        self.tasks.borrow().contains_key(&id)
    }
    pub fn task_count(&self) -> usize {
        self.tasks.borrow().len()
    }
    /// Cancel synchronously unless called reentrantly from the task's own poll.
    /// In that case the active poll unwinds to the executor before cleanup.
    pub fn cancel(&self, id: TaskId) -> bool {
        let record = self.tasks.borrow_mut().remove(&id);
        let found = record.is_some();
        // A pending root can be cancelled outside poll. Keep its owner busy
        // throughout stop too: stop may synchronously request owner destruction.
        let stopping = record
            .as_ref()
            .filter(|r| r.entry.is_some())
            .map(|r| r.group.clone());
        if let Some(group) = &stopping {
            group.0.running.set(group.running() + 1);
        }
        drop(record); // stop callbacks must run outside the registry borrow
        if let Some(group) = &stopping {
            group.0.running.set(group.running() - 1);
        }
        found
    }
    /// Permanently forbid new roots and stop existing roots and descendants.
    /// Returns false if a reentrant cancellation awaits an active poll/stop, or
    /// if the group belongs to another executor.
    pub fn cancel_group(&self, group: &TaskGroup) -> bool {
        if group
            .0
            .executor
            .borrow()
            .as_ref()
            .is_some_and(|q| !Weak::ptr_eq(q, &Arc::downgrade(&self.queue)))
        {
            return false;
        }
        group.0.closed.set(true);
        let ids: Vec<_> = self
            .tasks
            .borrow()
            .iter()
            .filter(|(_, r)| Rc::ptr_eq(&r.group.0, &group.0))
            .map(|(&id, _)| id)
            .collect();
        // Invalidate all tokens before any stop callback can send notifications.
        for id in &ids {
            if let Some(r) = self.tasks.borrow().get(id) {
                r.waker.deactivate();
            }
        }
        for id in ids {
            self.cancel(id);
        }
        group.running() == 0
    }
    pub fn poll(&self, max_polls: usize) -> Result<PollReport, Error> {
        if self.polling.replace(true) {
            return Err(invalid("Executor is already polling"));
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.polling);
        if let Some(reactor) = self.reactor.borrow().as_ref() {
            reactor.poll(Some(std::time::Duration::ZERO))?;
        }
        let now = self.clock.now();
        for r in self.tasks.borrow().values() {
            if r.deadline.is_some_and(|d| d <= now) {
                r.waker.wake();
            }
        }
        let batch: Vec<_> = {
            let mut ready = self.queue.0.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<_> = ready.order.iter().take(max_polls).copied().collect();
            for id in &ids {
                ready.remove(id);
            }
            ids
        };
        let mut report = PollReport::default();
        for id in batch {
            let Some((mut entry, group, waker)) =
                self.tasks.borrow_mut().get_mut(&id).and_then(|r| {
                    r.entry
                        .take()
                        .map(|e| (e, r.group.clone(), r.waker.clone()))
                })
            else {
                continue;
            };
            group.0.running.set(group.running() + 1);
            let _context = self.enter(waker.clone());
            let task = entry.task.as_mut().expect("live task");
            let result = if task.is_finished() {
                Ok(())
            } else {
                report.polled += 1;
                match &entry.environment {
                    Some(env) => env.resume(task),
                    None => task.resume().map_err(Error::from),
                }
            };
            let cancelled = group.is_closed() || !waker.0.active.load(Ordering::Acquire);
            if cancelled || task.is_finished() || result.is_err() {
                if !cancelled {
                    let result = result.and_then(|()| match task.status_ref() {
                        ContinuationStatus::Failed(e) => Err(e.clone().into()),
                        _ => task
                            .cps_result()
                            .ok_or_else(|| invalid("Executor requires a scalar task result")),
                    });
                    if result.is_err() {
                        self.cancel_group(&group);
                    }
                    report.completed.push(TaskCompletion { id, result });
                }
                self.cancel(id);
                drop(entry); // stop before decrementing the active-poll count
            } else {
                let wake = task.wait_plan().wake();
                let mut tasks = self.tasks.borrow_mut();
                let r = tasks.get_mut(&id).expect("active task record");
                r.deadline = match wake {
                    Some(super::driver::Wake::Deadline(d)) => Some(d),
                    _ => None,
                };
                r.entry = Some(entry);
                if matches!(wake, Some(super::driver::Wake::Immediate)) {
                    waker.wake();
                }
            }
            group.0.running.set(group.running() - 1);
        }
        report.ready = !self
            .queue
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty();
        report.next_deadline = self
            .tasks
            .borrow()
            .values()
            .filter_map(|r| r.deadline)
            .min();
        Ok(report)
    }
    fn new_waker(&self) -> Result<TaskWaker, Error> {
        Ok(TaskWaker(Arc::new(WakeState {
            id: next_id()?,
            active: AtomicBool::new(true),
            queue: Arc::downgrade(&self.queue),
        })))
    }
    fn enter(&self, waker: TaskWaker) -> ContextGuard {
        let old = CURRENT.with(|c| {
            c.replace(Some(Context {
                clock: self.clock.clone(),
                waker,
                reactor: self.reactor.clone(),
            }))
        });
        ContextGuard(old)
    }
    /// Wait for readiness, the nearest timer, or an external TaskWaker. Does not
    /// run scripts. Call poll again afterwards; notifications may be spurious.
    /// Unknown waits fail, and virtual clocks require host-managed driving.
    pub fn wait(&self) -> Result<(), Error> {
        if self.polling.get() {
            return Err(invalid("Cannot wait while the executor is polling"));
        }
        if self.task_count() == 0 {
            return Ok(());
        }
        let mut plan = super::driver::WaitPlan::default();
        let observed = {
            let tasks = self.tasks.borrow();
            for record in tasks.values() {
                plan.add(record.deadline.map(super::driver::Wake::Deadline));
            }
            tasks
                .values()
                .any(|record| Arc::strong_count(&record.waker.0) > 1)
        };
        self.wait_for(plan, observed)
    }
    fn ready(&self) -> bool {
        !self
            .queue
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
    fn wait_for(&self, plan: super::driver::WaitPlan, observed: bool) -> Result<(), Error> {
        use super::driver::Wake;
        if self.ready() || matches!(plan.wake(), Some(Wake::Immediate)) {
            return Ok(());
        }
        let deadline = match plan.wake() {
            Some(Wake::Deadline(deadline)) => Some(deadline),
            _ => None,
        };
        if deadline.is_some_and(|d| d <= self.clock.now()) {
            return Ok(());
        }
        if deadline.is_none() && !observed {
            return Err(invalid(
                "Task is pending without a supported wake source; use the embedding API to resume external host operations",
            ));
        }
        if !self.clock.allows_blocking_wait() {
            return Err(invalid("Blocking waits require a realtime executor clock"));
        }
        // Attach the kernel signal before checking the queue again. A notification
        // before attachment stays queued; one after it interrupts poll. There is
        // no interval between the final check and sleeping that can lose a wake.
        let reactor = ensure_reactor(&self.reactor, &self.queue)?;
        loop {
            if self.ready() {
                return Ok(());
            }
            let now = self.clock.now();
            if deadline.is_some_and(|d| d <= now) {
                return Ok(());
            }
            reactor.poll(deadline.map(|d| d.saturating_duration_since(now)))?;
        }
    }
}
impl Drop for HostExecutor {
    fn drop(&mut self) {
        for r in self.tasks.get_mut().values() {
            r.group.0.closed.set(true);
            r.waker.deactivate();
        }
        self.tasks.get_mut().clear();
    }
}

/// A fresh native C root shares executor wake/clock/reactor context while its
/// frame remains borrowed from the embedding caller.
pub(crate) struct BlockingContext {
    executor: HostExecutor,
    waker: TaskWaker,
}
impl BlockingContext {
    pub(crate) fn new() -> Result<Self, Error> {
        super::dns::initialize()?;
        let executor = HostExecutor::default();
        let waker = executor.new_waker()?;
        Ok(Self { executor, waker })
    }
    pub(crate) fn poll<T>(&self, operation: impl FnOnce() -> T) -> T {
        self.executor
            .queue
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.waker.0.id);
        let _context = self.executor.enter(self.waker.clone());
        operation()
    }
    pub(crate) fn wait(&self, plan: super::driver::WaitPlan) -> Result<(), Error> {
        self.executor
            .wait_for(plan, Arc::strong_count(&self.waker.0) > 1)
    }
}
impl Drop for BlockingContext {
    fn drop(&mut self) {
        self.waker.deactivate();
    }
}
