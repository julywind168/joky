//! Fixed-worker execution. Only factories and terminal packets cross threads.
use super::{Guard, Lease, Storage};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard, Weak,
    },
    task::Poll,
    thread::{self, JoinHandle},
    time::Instant,
};
mod disposal;
#[cfg(test)]
mod scoped_tests;
pub(crate) use disposal::CleanupReceipt;
pub(crate) type Output = Box<dyn Any + Send>;
pub(crate) type Factory = Box<dyn FnOnce() -> Result<Box<dyn Job>, Error> + Send>;
pub(crate) trait Job {
    fn poll(&mut self) -> Poll<Result<Output, Error>>;
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
pub(crate) fn cancelled_error() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::Cancelled,
        "Parallel task cancelled",
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepared,
    Ready,
    Running,
    Waiting,
    Stopping,
    Cleaned,
}
struct State {
    phase: Phase,
    generation: u64,
    children: Vec<Weak<Task>>,
    children_charge: Lease,
    result: Option<Result<Output, Error>>,
    sequence: u64,
    failure: Option<Error>,
    observer: Option<crate::runtime::executor::TaskWaker>,
    deadline: Option<Instant>,
}
struct Task {
    #[cfg(test)]
    polls: AtomicUsize,
    id: u64,
    worker: usize,
    shared: Arc<Shared>,
    root: Storage,
    code: Arc<dyn Any + Send + Sync>,
    cancelled: AtomicBool,
    cleaned: AtomicBool,
    failed: AtomicBool,
    controls_pending: AtomicUsize,
    cleanups_pending: AtomicUsize,
    cleanups: Mutex<Vec<disposal::Work>>,
    parent: Option<Weak<Task>>,
    group: Option<(Arc<Arbiter>, usize)>,
    state: Mutex<State>,
    factory: Mutex<Option<Factory>>,
    _charge: Lease,
}
struct Queue {
    ready: Vec<VecDeque<Arc<Task>>>,
    tasks: Vec<Weak<Task>>,
    closing: bool,
    next_worker: usize,
}
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    sequence: AtomicU64,
    max_tasks: usize,
    io: Mutex<Option<IoDriver>>,
    controls: Mutex<Vec<super::control::Driver>>,
}
struct IoDriver {
    reactor: crate::runtime::reactor::Reactor,
    thread: JoinHandle<()>,
}
/// A wake never retains the task, its job, or a borrowed frame.
#[derive(Clone)]
pub(crate) struct IoWaker(Weak<Task>);
impl IoWaker {
    pub(crate) fn wake(&self) {
        if let Some(task) = self.0.upgrade() {
            task.wake();
        }
    }
}
pub(crate) fn io_waker() -> Option<IoWaker> {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .map(|task| IoWaker(Arc::downgrade(task)))
    })
}
pub(crate) fn io_reactor() -> Result<crate::runtime::reactor::Reactor, Error> {
    let task = CURRENT
        .with(|current| current.borrow().clone())
        .ok_or_else(|| invalid("I/O requires a parallel task"))?;
    let mut io = lock(&task.shared.io);
    if lock(&task.shared.queue).closing {
        return Err(cancelled_error());
    }
    if io.is_none() {
        let reactor = crate::runtime::reactor::Reactor::for_executor()?;
        let driver = reactor.clone();
        let shared = Arc::downgrade(&task.shared);
        let thread = thread::Builder::new()
            .name("joky-io".into())
            .spawn(move || {
                loop {
                    if let Err(error) = driver.poll_events() {
                        if !driver.is_closed() {
                            // Publish terminal failures without holding the queue or
                            // registration lock. No sleeper depends on a dead driver.
                            if let Some(shared) = shared.upgrade() {
                                let tasks = {
                                    let mut queue = lock(&shared.queue);
                                    queue.closing = true;
                                    queue
                                        .tasks
                                        .iter()
                                        .filter_map(Weak::upgrade)
                                        .collect::<Vec<_>>()
                                };
                                for task in tasks {
                                    task.fail(error.clone());
                                }
                                shared.wake.notify_all();
                            }
                            driver.request_close();
                        }
                        break;
                    }
                }
            })
            .map_err(|error| invalid(&format!("Cannot create I/O driver: {error}")))?;
        *io = Some(IoDriver { reactor, thread });
    }
    Ok(io.as_ref().unwrap().reactor.clone())
}
/// Control remains on its creating thread; Drop waits for cleanup and joins workers.
pub struct ParallelExecutor {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    _local: PhantomData<Rc<()>>,
}
/// Clones submit checked packets; dropping a submitter never waits for workers.
#[derive(Clone)]
pub struct ParallelSubmitter {
    shared: Arc<Shared>,
}
impl ParallelSubmitter {
    pub fn submit(
        &self,
        task: crate::vm::PreparedParallelTask,
    ) -> Result<crate::vm::ParallelTask, Error> {
        task.submit_to(self)
    }
    pub(crate) fn publish(
        &self,
        root: Storage,
        code: Arc<dyn Any + Send + Sync>,
        factory: Factory,
    ) -> Result<Handle, Error> {
        Ok(publish(&self.shared, None, root, code, vec![factory], None)?.remove(0))
    }
}
#[derive(Clone)]
pub(crate) struct Handle(Arc<Task>);
thread_local! {
    static CURRENT: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };
    static WAITING: Cell<bool> = const { Cell::new(false) };
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static CLEANING: Cell<bool> = const { Cell::new(false) };
}
#[cfg(test)]
pub(crate) fn cleanup_count() -> usize {
    CURRENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .unwrap()
            .cleanups_pending
            .load(Ordering::Acquire)
    })
}
#[cfg(test)]
pub(crate) fn poll_count() -> usize {
    CURRENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .unwrap()
            .polls
            .load(Ordering::Acquire)
    })
}
pub(crate) fn identity() -> Option<u64> {
    CURRENT.with(|c| c.borrow().as_ref().map(|task| task.id))
}
pub(crate) fn parent_identity() -> Option<u64> {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()?
            .parent
            .as_ref()?
            .upgrade()
            .map(|task| task.id)
    })
}
pub(crate) fn active() -> bool {
    CURRENT.with(|c| c.borrow().is_some())
}
/// A failed worker activation may retain its local owners until Job::drop.
/// The terminal driver cancels descendants and drops the job only after their
/// cleanup acknowledgements. This query never waits on a worker.
pub(crate) fn children_pending() -> bool {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|task| !task.children_cleaned())
    })
}
/// Uninstalled captures can owe cleanup even if publication created no child.
/// Keep failed local frames (and their group arbitration) until the drain path
/// has run those receipts; otherwise isolate could recover before their Drop.
pub(crate) fn cleanup_pending() -> bool {
    CURRENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|task| task.cleanups_pending.load(Ordering::Acquire) != 0)
    })
}
pub(crate) fn check_cancelled() -> Result<(), Error> {
    if CLEANING.with(Cell::get) {
        return Ok(());
    }
    CURRENT.with(|c| {
        let current = c.borrow();
        let Some(task) = current.as_ref() else {
            return Ok(());
        };
        if task.failed.load(Ordering::Acquire) {
            return Err(lock(&task.state)
                .failure
                .clone()
                .expect("published task failure"));
        }
        if task.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
        }
        Ok(())
    })
}

pub(crate) fn code<T: Any + Send + Sync>() -> Option<Arc<T>> {
    CURRENT.with(|c| c.borrow().as_ref()?.code.clone().downcast().ok())
}
/// A control request retains cleanup responsibility until its Local frame is gone.
pub(crate) struct ControlReceipt(Arc<Task>);
pub(crate) fn control_receipt() -> Option<ControlReceipt> {
    CURRENT.with(|slot| {
        slot.borrow().as_ref().map(|task| {
            task.controls_pending.fetch_add(1, Ordering::Relaxed);
            ControlReceipt(task.clone())
        })
    })
}
impl Drop for ControlReceipt {
    fn drop(&mut self) {
        self.0.controls_pending.fetch_sub(1, Ordering::Release);
        self.0.wake();
    }
}
pub(crate) fn ready() {
    WAITING.with(|w| w.set(false));
}
pub(crate) fn wait_plan(plan: crate::runtime::driver::WaitPlan) {
    match plan.wake() {
        Some(crate::runtime::driver::Wake::Immediate) => ready(),
        Some(crate::runtime::driver::Wake::Deadline(deadline)) => wait_until(deadline),
        None => {}
    }
}
pub(crate) fn wait_for_wake() {
    WAITING.with(|w| w.set(true));
}
/// Arm only the actual polling task. Deadlines never retain a frame pointer.
pub(crate) fn wait_until(deadline: Instant) {
    if active() {
        WAITING.with(|w| w.set(true));
        DEADLINE.with(|d| d.set(Some(d.get().map_or(deadline, |old| old.min(deadline)))));
    }
}
pub(crate) struct CleanupGuard(bool);
impl CleanupGuard {
    pub(crate) fn enter() -> Self {
        Self(CLEANING.with(|c| c.replace(true)))
    }
}
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        CLEANING.with(|c| c.set(self.0));
    }
}
impl Task {
    fn fail(self: &Arc<Self>, error: Error) {
        {
            let mut state = lock(&self.state);
            if state.phase == Phase::Cleaned {
                return;
            }
            state.failure.get_or_insert(error);
            self.failed.store(true, Ordering::Release);
        }
        self.wake();
    }
    fn wake(self: &Arc<Self>) {
        let mut state = lock(&self.state);
        if state.phase == Phase::Cleaned {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        if matches!(
            state.phase,
            Phase::Waiting | Phase::Prepared | Phase::Stopping
        ) {
            state.phase = Phase::Ready;
            lock(&self.shared.queue).ready[self.worker].push_back(self.clone());
            self.shared.wake.notify_all();
        }
    }
    fn cancel(self: &Arc<Self>) {
        let mut pending = vec![self.clone()];
        while let Some(task) = pending.pop() {
            task.cancelled.store(true, Ordering::Release);
            pending.extend(lock(&task.state).children.iter().filter_map(Weak::upgrade));
            task.wake();
        }
    }
    fn children_cleaned(&self) -> bool {
        let children = lock(&self.state).children.clone();
        children
            .into_iter()
            .filter_map(|t| t.upgrade())
            .all(|t| t.cleaned.load(Ordering::Acquire))
    }
}
/// A control driver submits computation as children of its original root.
#[derive(Clone, Default)]
pub(crate) struct DomainSpawner {
    parent: Weak<Task>,
}
impl DomainSpawner {
    pub(crate) fn cancelled(&self) -> bool {
        self.parent
            .upgrade()
            .is_none_or(|parent| parent.cancelled.load(Ordering::Acquire))
    }

    pub(crate) fn submit(&self, factory: Factory) -> Result<Handle, Error> {
        let parent = self.parent.upgrade().ok_or_else(cancelled_error)?;
        if parent.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
        }
        Ok(publish(
            &parent.shared,
            Some(&parent),
            parent.root.clone(),
            Arc::new(()),
            vec![factory],
            None,
        )?
        .remove(0))
    }
}
impl Handle {
    #[cfg(test)]
    pub(crate) fn poll_count(&self) -> usize {
        self.0.polls.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn identity(&self) -> u64 {
        self.0.id
    }
    #[cfg(test)]
    pub(crate) fn domain_spawner(&self) -> DomainSpawner {
        DomainSpawner {
            parent: Arc::downgrade(&self.0),
        }
    }

    pub(crate) fn observe(&self, waker: crate::runtime::executor::TaskWaker) {
        let mut state = lock(&self.0.state);
        if state.phase == Phase::Cleaned {
            drop(state);
            waker.wake();
        } else {
            state.observer = Some(waker);
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> bool {
        lock(&self.0.state).phase == Phase::Waiting
    }

    #[cfg(test)]
    pub(crate) fn stopping(&self) -> bool {
        lock(&self.0.state).phase == Phase::Stopping
    }
    pub(crate) fn cancel(&self) {
        self.0.cancel();
    }
    pub(crate) fn cleaned(&self) -> bool {
        lock(&self.0.state).phase == Phase::Cleaned
    }
    pub(crate) fn sequence(&self) -> Option<u64> {
        let state = lock(&self.0.state);
        (state.phase == Phase::Cleaned).then_some(state.sequence)
    }
    pub(crate) fn failed(&self) -> bool {
        lock(&self.0.state)
            .result
            .as_ref()
            .is_some_and(Result::is_err)
    }
    pub(crate) fn take(&self) -> Option<Result<Output, Error>> {
        lock(&self.0.state).result.take()
    }
    pub(crate) fn wait(&self) -> Result<(), Error> {
        if active() {
            return Err(invalid("Blocking task waits are forbidden on workers"));
        }
        let mut q = lock(&self.0.shared.queue);
        loop {
            // Never acquire a task lock while holding the queue lock: wake uses
            // the opposite order. The timed wait also tolerates a just-missed wake.
            drop(q);
            if self.cleaned() {
                return Ok(());
            }
            q = lock(&self.0.shared.queue);
            q = self
                .0
                .shared
                .wake
                .wait_timeout(q, std::time::Duration::from_millis(10))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
impl ParallelExecutor {
    #[cfg(test)]
    pub(crate) fn control_observations(&self) -> Vec<(std::thread::ThreadId, usize)> {
        lock(&self.shared.controls)
            .iter()
            .map(super::control::Driver::observation)
            .collect()
    }

    pub fn new(workers: usize, max_tasks: usize) -> Result<Self, Error> {
        if active() {
            return Err(invalid(
                "Parallel control must be created outside task workers",
            ));
        }
        if workers == 0 || workers > 256 || max_tasks == 0 {
            return Err(invalid(
                "Parallel workers must be 1..=256 and task capacity must be positive",
            ));
        }
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                ready: (0..workers).map(|_| VecDeque::new()).collect(),
                tasks: Vec::new(),
                closing: false,
                next_worker: 0,
            }),
            wake: Condvar::new(),
            sequence: AtomicU64::new(1),
            max_tasks,
            io: Mutex::new(None),
            controls: Mutex::new(Vec::new()),
        });
        let mut pool = Self {
            shared,
            workers: Vec::new(),
            _local: PhantomData,
        };
        for index in 0..workers {
            let shared = pool.shared.clone();
            let worker = thread::Builder::new()
                .name(format!("joky-task-{index}"))
                .spawn(move || run(shared, index))
                .map_err(|e| invalid(&format!("Cannot create task worker: {e}")))?;
            pool.workers.push(worker);
        }
        Ok(pool)
    }
    #[cfg(test)]
    pub(crate) fn reactor(&self) -> Option<crate::runtime::reactor::Reactor> {
        lock(&self.shared.io).as_ref().map(|io| io.reactor.clone())
    }
    pub fn submitter(&self) -> ParallelSubmitter {
        ParallelSubmitter {
            shared: self.shared.clone(),
        }
    }
    pub(crate) fn submit(
        &self,
        root: Storage,
        code: Arc<dyn Any + Send + Sync>,
        factory: Factory,
    ) -> Result<Handle, Error> {
        self.submitter().publish(root, code, factory)
    }
    pub fn request_close(&self) {
        let tasks = {
            let mut q = lock(&self.shared.queue);
            q.closing = true;
            q.tasks.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        for task in tasks {
            task.cancel();
        }
        self.shared.wake.notify_all();
        let reactor = lock(&self.shared.io).as_ref().map(|io| io.reactor.clone());
        if let Some(reactor) = reactor {
            reactor.request_close();
        }
    }
    pub fn is_closed(&self) -> bool {
        let tasks = {
            let q = lock(&self.shared.queue);
            if !q.closing {
                return false;
            }
            q.tasks.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        tasks.iter().all(|t| lock(&t.state).phase == Phase::Cleaned)
            && lock(&self.shared.controls)
                .iter()
                .all(super::control::Driver::finished)
            && lock(&self.shared.io)
                .as_ref()
                .is_none_or(|io| io.thread.is_finished())
    }
    pub fn wait_closed(&mut self) {
        self.request_close();
        for worker in self.workers.drain(..) {
            worker.join().expect("task worker panicked");
        }
        let controls = std::mem::take(&mut *lock(&self.shared.controls));
        for control in controls {
            control.join();
        }
        let io = lock(&self.shared.io).take();
        if let Some(io) = io {
            io.thread.join().expect("I/O driver panicked");
            debug_assert_eq!(io.reactor.registration_count(), 0);
        }
    }
}
impl Drop for ParallelExecutor {
    fn drop(&mut self) {
        self.wait_closed();
    }
}
fn children(factories: Vec<Factory>, group: Arc<Arbiter>) -> Result<Vec<Handle>, Error> {
    CURRENT.with(|c| {
        let parent = c
            .borrow()
            .clone()
            .ok_or_else(|| invalid("Missing parallel task context"))?;
        publish(
            &parent.shared,
            Some(&parent),
            parent.root.clone(),
            parent.code.clone(),
            factories,
            Some(group),
        )
    })
}
fn publish(
    shared: &Arc<Shared>,
    parent: Option<&Arc<Task>>,
    root: Storage,
    code: Arc<dyn Any + Send + Sync>,
    factories: Vec<Factory>,
    group: Option<Arc<Arbiter>>,
) -> Result<Vec<Handle>, Error> {
    // Reserve every record before publication. No child can execute a partial batch.
    let charges = (0..factories.len())
        .map(|_| root.accounts().reserve(std::mem::size_of::<Task>() + 128))
        .collect::<Result<Vec<_>, _>>()?;
    let identities = (0..factories.len())
        .map(|_| crate::runtime::task_scope::fresh_identity())
        .collect::<Result<Vec<_>, _>>()?;
    let mut retained = Vec::new();
    let mut parent_state = parent.map(|t| lock(&t.state));
    if parent.is_some_and(|t| t.cancelled.load(Ordering::Acquire)) {
        return Err(cancelled_error());
    }
    let mut arbitration = group.as_ref().map(|g| lock(&g.state));
    if arbitration.as_ref().is_some_and(|a| a.selected.is_some()) {
        return Err(arbitration
            .as_ref()
            .and_then(|a| a.failure.clone())
            .unwrap_or_else(|| invalid("Parallel scope is closed to publication")));
    }
    if let Some(state) = &mut arbitration {
        // A dynamic scope retains completed result positions until join, even
        // after live-task admission slots become reusable. Charge that capacity
        // independently so a long scope cannot bypass its root budget.
        let count = state
            .children
            .len()
            .checked_add(factories.len())
            .ok_or_else(|| invalid("Parallel scope metadata overflow"))?;
        let bytes = count
            .checked_mul(64)
            .and_then(|n| {
                n.checked_add(std::mem::size_of::<Group>() + std::mem::size_of::<Arbiter>())
            })
            .ok_or_else(|| invalid("Parallel scope metadata overflow"))?;
        if let Some(charge) = &mut state.charge {
            charge.resize(bytes)?;
        } else {
            state.charge = Some(root.accounts().reserve(bytes)?);
        }
    }
    let mut q = lock(&shared.queue);
    q.tasks.retain(|t| {
        t.upgrade().is_some_and(|t| {
            let live = !t.cleaned.load(Ordering::Acquire);
            retained.push(t);
            live
        })
    });
    if let Some(state) = &mut parent_state {
        state.children.retain(|t| {
            t.upgrade().is_some_and(|t| {
                let live = !t.cleaned.load(Ordering::Acquire);
                retained.push(t);
                live
            })
        });
    }
    if q.closing
        || q.tasks
            .len()
            .checked_add(factories.len())
            .is_none_or(|n| n > shared.max_tasks)
    {
        return Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Parallel task admission capacity exhausted or executor closing",
        ));
    }
    if let Some(state) = &mut parent_state {
        let capacity = state
            .children
            .capacity()
            .max(state.children.len() + factories.len());
        state
            .children_charge
            .resize(capacity * std::mem::size_of::<Weak<Task>>())?;
        state
            .children
            .try_reserve_exact(factories.len())
            .map_err(|_| invalid("Task child metadata capacity exhausted"))?;
    }
    let mut handles = Vec::with_capacity(factories.len());
    for ((factory, charge), id) in factories.into_iter().zip(charges).zip(identities) {
        let worker = q.next_worker;
        q.next_worker = (worker + 1) % q.ready.len();
        let task = Arc::new(Task {
            #[cfg(test)]
            polls: AtomicUsize::new(0),
            id,
            worker,
            shared: shared.clone(),
            root: root.clone(),
            code: code.clone(),
            cancelled: AtomicBool::new(false),
            cleaned: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            controls_pending: AtomicUsize::new(0),
            cleanups_pending: AtomicUsize::new(0),
            cleanups: Mutex::new(Vec::new()),
            parent: parent.map(Arc::downgrade),
            group: group
                .as_ref()
                .map(|g| (g.clone(), arbitration.as_ref().unwrap().children.len())),
            state: Mutex::new(State {
                phase: Phase::Prepared,
                generation: 0,
                children: Vec::new(),
                children_charge: root.accounts().reserve(0).expect("zero reservation"),
                result: None,
                sequence: 0,
                failure: None,
                observer: None,
                deadline: None,
            }),
            factory: Mutex::new(Some(factory)),
            _charge: charge,
        });
        if let Some(state) = &mut parent_state {
            state.children.push(Arc::downgrade(&task));
        }
        if let Some(arbitration) = &mut arbitration {
            arbitration.children.push(Arc::downgrade(&task));
        }
        q.tasks.push(Arc::downgrade(&task));
        handles.push(Handle(task));
    }
    for handle in &handles {
        lock(&handle.0.state).phase = Phase::Ready;
        q.ready[handle.0.worker].push_back(handle.0.clone());
    }
    drop(q);
    shared.wake.notify_all();
    Ok(handles)
}
struct Local {
    _task: Arc<Task>,
    job: Option<Box<dyn Job>>,
    terminal: Option<Result<Output, Error>>,
}
fn run(shared: Arc<Shared>, worker: usize) {
    let mut local = HashMap::<u64, Local>::new();
    loop {
        // Scan only this worker's records outside the queue lock (wake takes
        // state then queue). Recheck the queue before sleeping to avoid lost wakes.
        let now = Instant::now();
        let mut deadline: Option<Instant> = None;
        for item in local.values() {
            let due = lock(&item._task.state).deadline;
            if let Some(due) = due {
                if due <= now {
                    item._task.wake();
                } else {
                    deadline = Some(deadline.map_or(due, |old| old.min(due)));
                }
            }
        }
        let task = {
            let mut q = lock(&shared.queue);
            loop {
                if let Some(task) = q.ready[worker].pop_front() {
                    break Some(task);
                }
                if q.closing && local.is_empty() {
                    return;
                }
                if let Some(deadline) = deadline {
                    let (next, timeout) = shared
                        .wake
                        .wait_timeout(q, deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or_else(|e| e.into_inner());
                    q = next;
                    if timeout.timed_out() {
                        break None;
                    }
                } else {
                    q = shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            }
        };
        let Some(task) = task else {
            continue;
        };
        let generation = {
            let mut state = lock(&task.state);
            if state.phase != Phase::Ready {
                continue;
            }
            state.phase = Phase::Running;
            state.deadline = None;
            state.generation
        };
        CURRENT.with(|c| c.replace(Some(task.clone())));
        WAITING.with(|w| w.set(false));
        DEADLINE.with(|d| d.set(None));
        let _storage = Guard::enter(&task.root);
        let disposal_error = disposal::drain(&task).err();
        let item = local.entry(task.id).or_insert_with(|| Local {
            _task: task.clone(),
            job: None,
            terminal: None,
        });
        if let Some(error) = disposal_error {
            task.fail(error.clone());
            if item.terminal.as_ref().is_some_and(|r| {
                r.as_ref()
                    .err()
                    .is_none_or(|e| e.code() == ErrorCode::Cancelled)
            }) {
                item.terminal = Some(Err(error));
            }
        }
        if item.job.is_none() && item.terminal.is_none() {
            if task.cancelled.load(Ordering::Acquire) {
                let abandoned = { lock(&task.factory).take() };
                drop(abandoned);
                item.terminal = Some(Err(cancelled_error()));
            } else {
                let factory = lock(&task.factory).take().expect("one task constructor");
                match factory() {
                    Ok(job) => item.job = Some(job),
                    Err(e) => item.terminal = Some(Err(e)),
                }
            }
        }
        if item.terminal.is_none() {
            if let Err(error) = check_cancelled() {
                item.terminal = Some(Err(error));
            } else if let Poll::Ready(result) = item.job.as_mut().unwrap().poll() {
                item.terminal = Some(result);
            }
        }
        #[cfg(test)]
        task.polls.fetch_add(1, Ordering::Release);
        if item.terminal.is_some() {
            let children: Vec<_> = lock(&task.state)
                .children
                .iter()
                .filter_map(Weak::upgrade)
                .collect();
            for child in children {
                if lock(&child.state).phase != Phase::Cleaned {
                    child.cancel();
                }
            }
            let children_cleaned = task.children_cleaned();
            if children_cleaned {
                let _cleaning = CleanupGuard::enter();
                let cleanup = crate::runtime::error::checked(|| drop(item.job.take()));
                if let Err(error) = cleanup {
                    if item
                        .terminal
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .err()
                        .is_none_or(|e| e.code() == ErrorCode::Cancelled)
                    {
                        item.terminal = Some(Err(error));
                    }
                }
            }
            if children_cleaned {
                if let Err(error) = disposal::drain(&task) {
                    if item.terminal.as_ref().is_some_and(|r| {
                        r.as_ref()
                            .err()
                            .is_none_or(|e| e.code() == ErrorCode::Cancelled)
                    }) {
                        item.terminal = Some(Err(error));
                    }
                }
            }
            let control_closed = if children_cleaned && task.parent.is_none() {
                let control = lock(&shared.controls)
                    .iter()
                    .find_map(|c| c.matches(&task.root));
                control.is_none_or(|c| c.finish(IoWaker(Arc::downgrade(&task))))
            } else {
                true
            };
            if children_cleaned
                && control_closed
                && task.controls_pending.load(Ordering::Acquire) == 0
                && task.cleanups_pending.load(Ordering::Acquire) == 0
            {
                let _cleaning = CleanupGuard::enter();
                let mut item = local.remove(&task.id).unwrap();
                let mut result = item.terminal.take().unwrap();
                if result
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.code() == ErrorCode::Cancelled)
                {
                    // Scopes can disappear during parent unwinding, before
                    // descendants finish. Their failures are retained on the
                    // parent record independently of scope/result handles.
                    if let Some(error) = lock(&task.state).failure.clone() {
                        result = Err(error);
                    }
                }
                let cleanup = crate::runtime::error::checked(|| drop(item.job.take()));
                if result.is_ok()
                    || result
                        .as_ref()
                        .err()
                        .is_some_and(|e| e.code() == ErrorCode::Cancelled)
                {
                    if let Err(e) = cleanup {
                        result = Err(e);
                    }
                }
                if task.cancelled.load(Ordering::Acquire) && result.is_ok() {
                    result = Err(cancelled_error());
                }
                if let Some((group, index)) = &task.group {
                    group.complete(*index, result.as_ref().err().cloned());
                }
                let mut state = lock(&task.state);
                state.phase = Phase::Cleaned;
                state.sequence = shared.sequence.fetch_add(1, Ordering::Relaxed);
                state.result = Some(result);
                task.cleaned.store(true, Ordering::Release);
                let observer = state.observer.take();
                drop(state);
                if let Some(observer) = observer {
                    observer.wake();
                }
                if let Some(parent) = task.parent.as_ref().and_then(Weak::upgrade) {
                    parent.wake();
                }
                shared.wake.notify_all();
            } else {
                let mut state = lock(&task.state);
                state.phase = Phase::Stopping;
                // A completion racing the child scan increments generation.
                if state.generation != generation {
                    drop(state);
                    task.wake();
                }
            }
        } else {
            let mut state = lock(&task.state);
            state.phase = Phase::Waiting;
            state.deadline = DEADLINE.with(Cell::get);
            let ready = !WAITING.with(Cell::get)
                || state.generation != generation
                || task.cancelled.load(Ordering::Acquire);
            drop(state);
            if ready {
                task.wake();
            }
        }
        CURRENT.with(|c| c.borrow_mut().take());
    }
}

/// Per-scope arbitration. The owner reads only synchronized terminal packets.
pub(crate) struct Group {
    arbiter: Arc<Arbiter>,
    kind: u32,
    handles: Vec<Handle>,
    selected: Option<usize>,
    failure: Option<Error>,
    taken: Vec<bool>,
}
impl Group {
    fn retain_failure(&mut self, error: Error) {
        if self.arbiter.propagate_failure {
            self.failure.get_or_insert(error);
        } else {
            crate::runtime::isolate::retain_failure(&mut self.failure, error);
        }
    }
    pub(crate) fn new(kind: u32) -> Self {
        Self {
            arbiter: Arc::new(Arbiter {
                parent: CURRENT.with(|c| c.borrow().as_ref().map(Arc::downgrade)),
                kind,
                propagate_failure: !crate::runtime::isolate::active(),
                state: Mutex::new(Arbitration {
                    selected: None,
                    failure: None,
                    children: Vec::new(),
                    charge: None,
                }),
            }),
            kind,
            handles: Vec::new(),
            selected: None,
            failure: None,
            taken: Vec::new(),
        }
    }
    pub(crate) fn capture_guard(&self) -> disposal::CaptureGuard {
        disposal::CaptureGuard::enter(
            (!self.arbiter.propagate_failure).then(|| self.arbiter.clone()),
        )
    }
    pub(crate) fn publish(&mut self, factories: Vec<Factory>) -> Result<(), Error> {
        if self.selected.is_some() || self.failure.is_some() {
            return Err(invalid("Parallel scope is closed"));
        }
        let handles = children(factories, self.arbiter.clone())?;
        self.taken.extend((0..handles.len()).map(|_| false));
        self.handles.extend(handles);
        Ok(())
    }
    pub(crate) fn poll(&mut self) -> Result<bool, Error> {
        let (selected, failure) = {
            let arbitration = lock(&self.arbiter.state);
            (arbitration.selected, arbitration.failure.clone())
        };
        self.selected = selected;
        if let Some(error) = failure {
            self.retain_failure(error);
        }
        if !self.handles.iter().all(Handle::cleaned) {
            wait_for_wake();
            return Ok(false);
        }
        if self.kind == 1 {
            for (index, handle) in self.handles.iter().enumerate() {
                if self.selected != Some(index) && !handle.failed() {
                    drop(handle.take());
                }
            }
        }
        if let Some(task) = CURRENT.with(|slot| slot.borrow().clone()) {
            if let Err(error) = disposal::drain(&task) {
                self.retain_failure(error);
            }
        }
        let failure = lock(&self.arbiter.state).failure.clone();
        if let Some(error) = failure {
            self.retain_failure(error);
        }
        // Cancellation itself is expected for losers; a real cleanup failure is not.
        let mut ordered: Vec<_> = self.handles.iter().enumerate().collect();
        ordered.sort_by_key(|(_, h)| h.sequence());
        let mut failures = Vec::new();
        for (i, h) in ordered {
            if self.selected != Some(i) && h.failed() {
                if let Some(Err(error)) = h.take() {
                    if error.code() != ErrorCode::Cancelled {
                        failures.push(error);
                    }
                }
            }
        }
        for error in failures {
            self.retain_failure(error);
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        // Cancellation may arrive after the driver's entry check and drain all
        // children during this poll. Their cancelled packets are not results.
        check_cancelled()?;
        Ok(true)
    }
    /// Local cancellation must also dispose unclaimed results before recovery.
    /// Their portable cleanup receipts belong to the consuming parent task.
    pub(crate) fn cancel_and_drain(&mut self) -> Result<bool, Error> {
        self.cancel();
        let outcome = self.poll();
        if matches!(outcome, Ok(false)) {
            return Ok(false);
        }
        for handle in &self.handles {
            drop(handle.take());
        }
        let cleanup = CURRENT
            .with(|slot| slot.borrow().clone())
            .map_or(Ok(()), |task| disposal::drain(&task));
        let failure = lock(&self.arbiter.state).failure.clone();
        if let Some(error) = failure {
            self.retain_failure(error);
        }
        if let Err(error) = outcome {
            self.retain_failure(error);
        }
        if let Err(error) = cleanup {
            self.retain_failure(error);
        }
        self.failure.clone().map_or(Ok(true), Err)
    }
    pub(crate) fn next_result(&self) -> Option<usize> {
        if self.kind == 1 {
            self.selected.filter(|&i| !self.taken[i])
        } else {
            self.taken.iter().position(|taken| !taken)
        }
    }
    pub(crate) fn take(&mut self, index: usize) -> Result<Output, Error> {
        if !self.poll()? {
            return Err(invalid("Parallel result is not ready"));
        }
        if self.taken.get(index).copied() != Some(false)
            || (self.kind == 1 && self.selected != Some(index))
        {
            return Err(invalid("Parallel result was consumed or is not the winner"));
        }
        self.taken[index] = true;
        self.handles[index]
            .take()
            .ok_or_else(|| invalid("Missing parallel result"))?
    }
    pub(crate) fn winner(&self) -> Option<usize> {
        self.selected
    }
    pub(crate) fn cancel(&self) {
        for handle in &self.handles {
            handle.cancel();
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct Arbitration {
    selected: Option<usize>,
    failure: Option<Error>,
    children: Vec<Weak<Task>>,
    charge: Option<Lease>,
}
struct Arbiter {
    parent: Option<Weak<Task>>,
    kind: u32,
    propagate_failure: bool,
    state: Mutex<Arbitration>,
}
impl Arbiter {
    fn complete(&self, index: usize, error: Option<Error>) {
        let mut retained = Vec::new();
        let failure;
        {
            let mut state = lock(&self.state);
            if error
                .as_ref()
                .is_some_and(|e| e.code() == ErrorCode::Cancelled)
            {
                return;
            }
            if state.selected.is_some() {
                // A chosen successful value still depends on losing cleanup.
                let Some(error) = error else {
                    return;
                };
                if self.propagate_failure {
                    state.failure.get_or_insert(error);
                } else {
                    crate::runtime::isolate::retain_failure(&mut state.failure, error);
                }
            } else {
                if self.kind != 1 && error.is_none() {
                    return;
                }
                state.selected = Some(index);
                state.failure = error;
            }
            failure = state.failure.clone();
            retained.extend(
                state
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .filter_map(|(_, c)| c.upgrade()),
            );
        }
        for task in retained {
            task.cancel();
        }
        if let Some(error) = failure {
            if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
                if self.propagate_failure {
                    parent.fail(error);
                } else {
                    parent.wake();
                }
            }
        }
    }

    fn cleanup_failed(&self, error: Error) {
        let children = {
            let mut state = lock(&self.state);
            if self.propagate_failure {
                state.failure.get_or_insert(error);
            } else {
                crate::runtime::isolate::retain_failure(&mut state.failure, error);
            }
            state
                .children
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for child in children {
            child.cancel();
        }
        if let Some(parent) = self.parent.as_ref().and_then(Weak::upgrade) {
            parent.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_pool_running_notification_survives_pending_and_late_wake_is_inert() {
        struct Probe {
            first: bool,
            sent: std::sync::mpsc::Sender<Arc<Task>>,
            barrier: Arc<std::sync::Barrier>,
        }
        impl Job for Probe {
            fn poll(&mut self) -> Poll<Result<Output, Error>> {
                if self.first {
                    self.first = false;
                    wait_for_wake();
                    self.sent
                        .send(CURRENT.with(|c| c.borrow().as_ref().unwrap().clone()))
                        .unwrap();
                    self.barrier.wait();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(Box::new(42usize)))
                }
            }
        }
        let pool = ParallelExecutor::new(1, 4).unwrap();
        let root = Storage::new(vec![super::super::TaskBudget::new(1 << 20)]).unwrap();
        let (sent, recv) = std::sync::mpsc::channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let release = barrier.clone();
        let task = pool
            .submit(
                root,
                Arc::new(()),
                Box::new(move || {
                    Ok(Box::new(Probe {
                        first: true,
                        sent,
                        barrier,
                    }))
                }),
            )
            .unwrap();
        let record = recv
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        record.wake();
        record.wake();
        release.wait();
        task.wait().unwrap();
        assert_eq!(
            *task.take().unwrap().unwrap().downcast::<usize>().unwrap(),
            42
        );
        record.wake();
        assert_eq!(lock(&record.state).phase, Phase::Cleaned);
        assert!(lock(&pool.shared.queue)
            .ready
            .iter()
            .all(VecDeque::is_empty));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    fn task_pool_deadline_wait_is_targeted_and_worker_blocking_is_rejected() {
        struct Waiting(std::sync::mpsc::Sender<Arc<Task>>);
        impl Job for Waiting {
            fn poll(&mut self) -> Poll<Result<Output, Error>> {
                assert!(ParallelExecutor::new(1, 4).is_err());
                let task = CURRENT.with(|c| c.borrow().as_ref().unwrap().clone());
                assert!(Handle(task.clone()).wait().is_err());
                wait_until(Instant::now() + std::time::Duration::from_secs(3600));
                self.0.send(task).unwrap();
                Poll::Pending
            }
        }
        let budget = super::super::TaskBudget::new(1 << 20);
        let root = Storage::new(vec![budget.clone()]).unwrap();
        let mut executor = ParallelExecutor::new(1, 8).unwrap();
        let (sent, receive) = std::sync::mpsc::channel();
        let task = executor
            .submit(
                root,
                Arc::new(()),
                Box::new(move || Ok(Box::new(Waiting(sent)))),
            )
            .unwrap();
        let record = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        while lock(&record.state).phase == Phase::Running {
            std::thread::yield_now();
        }
        assert_eq!(lock(&record.state).phase, Phase::Waiting);
        assert!(
            receive.try_recv().is_err(),
            "a parked timer must not busy-poll"
        );
        task.cancel();
        task.wait().unwrap();
        assert_eq!(
            task.take().unwrap().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        executor.wait_closed();
        drop((record, task, executor));
        assert_eq!(budget.used(), 0);
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    struct Probe {
        shared: Arc<Shared>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }
    fn assert_unlocked<T>(mutex: &Mutex<T>) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while mutex.try_lock().is_err() {
            assert!(Instant::now() < deadline, "Drop retained a scheduler lock");
            std::thread::yield_now();
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            assert_unlocked(&self.shared.queue);
            CURRENT.with(|current| {
                if let Some(task) = current.borrow().as_ref() {
                    assert_unlocked(&task.factory);
                    assert_unlocked(&task.state);
                }
            });
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Job for Probe {
        fn poll(&mut self) -> Poll<Result<Output, Error>> {
            Poll::Ready(Ok(Box::new(())))
        }
    }
    fn close_publication(rounds: usize) {
        for _ in 0..rounds {
            let mut pool = ParallelExecutor::new(1, 4).unwrap();
            let shared = pool.shared.clone();
            let budget = super::super::TaskBudget::new(1 << 20);
            let root = Storage::new(vec![budget.clone()]).unwrap();
            let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let probe = Probe {
                shared: shared.clone(),
                drops: drops.clone(),
            };
            let gate = Arc::new(std::sync::Barrier::new(2));
            let submit_gate = gate.clone();
            let submit = std::thread::spawn(move || {
                submit_gate.wait();
                publish(
                    &shared,
                    None,
                    root,
                    Arc::new(()),
                    vec![Box::new(move || Ok(Box::new(probe)))],
                    None,
                )
            });
            gate.wait();
            pool.request_close();
            let result = submit.join().unwrap();
            pool.wait_closed();
            if let Ok(handles) = &result {
                for handle in handles {
                    assert!(handle.cleaned());
                }
            }
            drop(result);
            drop(pool);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            assert_eq!(budget.used(), 0);
        }
    }
    #[test]
    fn task_pool_publication_races_close_without_duplicate_cleanup() {
        close_publication(16);
    }
    #[test]
    #[ignore = "exhaustive: extended concurrent publication and shutdown schedules"]
    fn exhaustive_task_pool_publication_races_close() {
        close_publication(512);
    }
}

pub(crate) fn control() -> Result<Arc<super::control::Core>, Error> {
    let task = CURRENT
        .with(|c| c.borrow().clone())
        .ok_or_else(|| invalid("Control command requires a worker task"))?;
    let mut controls = lock(&task.shared.controls);
    let mut index = 0;
    while index < controls.len() {
        if controls[index].finished() {
            controls.swap_remove(index).join();
        } else {
            index += 1;
        }
    }
    if let Some(core) = controls.iter().find_map(|c| c.matches(&task.root)) {
        return Ok(core);
    }
    if lock(&task.shared.queue).closing {
        return Err(cancelled_error());
    }
    if controls.len() >= task.shared.max_tasks {
        return Err(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::ResourceLimit,
            "Local control driver limit exceeded",
        ));
    }
    let files = if let Some(code) = task.code.downcast_ref::<crate::vm::parallel::Code>() {
        match code {
            crate::vm::parallel::Code::Native(e) => e.files.clone(),
            crate::vm::parallel::Code::Interpreted(e) => e.files.clone(),
        }
    } else {
        task.code
            .downcast_ref::<super::control::Files>()
            .and_then(|files| files.0.clone())
    };
    let mut parent = task.clone();
    while let Some(next) = parent.parent.as_ref().and_then(Weak::upgrade) {
        parent = next;
    }
    let spawner = DomainSpawner {
        parent: Arc::downgrade(&parent),
    };
    let (driver, core) = super::control::Driver::start(task.root.clone(), files, spawner)?;
    controls.push(driver);
    Ok(core)
}
