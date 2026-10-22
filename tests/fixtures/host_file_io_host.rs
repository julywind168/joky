// FS0 model only: mock files and copied bytes, with one controlled worker.
// No production filesystem, File ABI, or legacy byte/borrowing ABI is changed.
use joky::runtime::{
    cps::CpsFrame,
    executor::{current_waker, Clock, HostExecutor, TaskGroup, TaskWaker},
    resource::ResourceFactoryV1,
    suspension::Continuation,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    ffi::c_void,
    rc::Rc,
    sync::{mpsc, Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

const FILES: usize = 3;
const QUEUED: usize = 1;
const REQUESTS: usize = 2;
const PAYLOAD: [u8; 4] = [0, 255, 42, 128];
const WATCHDOG: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Running,
    Ready,
}
struct Request {
    file: usize,
    phase: Phase,
    cancelled: bool,
    delivered: bool,
    bytes: [u8; 4],
    waker: Option<TaskWaker>,
}
#[derive(Default)]
struct FileState {
    opened: bool,
    wrapper: bool,
    closing: bool,
    closed: bool,
    close_queued: bool,
    active: Option<u64>,
    closes: usize,
}
struct State {
    files: [FileState; FILES],
    requests: BTreeMap<u64, Request>,
    queue: VecDeque<u64>,
    cleanup: VecDeque<usize>,
    next: u64,
    shutdown: bool,
    bytes: usize,
    started: Vec<u64>,
    effects: Vec<[u8; 4]>,
    close_threads: Vec<ThreadId>,
    request_releases: usize,
    delivered: usize,
    busy: usize,
    exited: bool,
}
struct Service {
    state: Mutex<State>,
    changed: Condvar,
}
fn lock(service: &Service) -> MutexGuard<'_, State> {
    service.state.lock().unwrap()
}

impl Service {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                files: std::array::from_fn(|_| FileState::default()),
                requests: BTreeMap::new(),
                queue: VecDeque::with_capacity(QUEUED),
                cleanup: VecDeque::with_capacity(FILES),
                next: 1,
                shutdown: false,
                bytes: 0,
                started: Vec::new(),
                effects: Vec::new(),
                close_threads: Vec::new(),
                request_releases: 0,
                delivered: 0,
                busy: 0,
                exited: false,
            }),
            changed: Condvar::new(),
        })
    }
    fn open(&self, file: usize) {
        let mut state = lock(self);
        assert!(!state.shutdown && !state.files[file].opened);
        state.files[file].opened = true;
        state.files[file].wrapper = true;
        // Every file has one preallocated cleanup slot, independent of QUEUED.
    }
    fn submit(&self, file: usize, bytes: &[u8; 4], waker: TaskWaker) -> Option<u64> {
        let mut state = lock(self);
        if state.shutdown
            || state.files[file].closing
            || state.files[file].active.is_some()
            || state.queue.len() == QUEUED
            || state.requests.len() == REQUESTS
        {
            state.busy += 1;
            return None;
        }
        let id = state.next;
        state.next = state.next.checked_add(1).unwrap();
        state.files[file].active = Some(id);
        state.requests.insert(
            id,
            Request {
                file,
                phase: Phase::Queued,
                cancelled: false,
                delivered: false,
                bytes: *bytes,
                waker: Some(waker),
            },
        );
        state.bytes += bytes.len();
        state.queue.push_back(id);
        self.changed.notify_all();
        Some(id)
    }
    fn close_locked(state: &mut State, file: usize) {
        let owner = &mut state.files[file];
        owner.closing = true;
        if owner.active.is_none() && !owner.closed && !owner.close_queued {
            owner.close_queued = true;
            assert!(state.cleanup.len() < FILES);
            state.cleanup.push_back(file);
        }
    }
    fn retire(state: &mut State, id: u64) {
        let request = state.requests.remove(&id).unwrap();
        assert_eq!(state.files[request.file].active.take(), Some(id));
        state.bytes -= request.bytes.len();
        if state.files[request.file].closing {
            Self::close_locked(state, request.file);
        }
    }
    fn stop(&self, id: u64) {
        let mut state = lock(self);
        let request = state.requests.get_mut(&id).unwrap();
        let (file, phase, delivered) = (request.file, request.phase, request.delivered);
        request.waker = None;
        request.cancelled = !delivered;
        state.request_releases += 1;
        if !delivered && phase != Phase::Queued {
            Self::close_locked(&mut state, file);
        }
        if phase != Phase::Running {
            state.queue.retain(|queued| *queued != id);
            Self::retire(&mut state, id);
        }
        self.changed.notify_all();
    }
    fn release_file(&self, file: usize) {
        let mut state = lock(self);
        assert!(state.files[file].wrapper);
        state.files[file].wrapper = false;
        Self::close_locked(&mut state, file);
        self.changed.notify_all();
    }
    fn wait(&self, predicate: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + WATCHDOG;
        let mut state = lock(self);
        while !predicate(&state) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "FS0 worker did not reach the expected state"
            );
            state = self.changed.wait_timeout(state, left).unwrap().0;
        }
    }
}

// This thread owns no language pointer or ResourceLease. The gate deliberately
// holds an operation after dequeue, without holding the queue/completion lock.
fn worker(service: Arc<Service>, gate: mpsc::Receiver<()>) {
    loop {
        let mut state = lock(&service);
        if let Some(file) = state.cleanup.pop_front() {
            assert!(state.files[file].active.is_none());
            state.files[file].close_queued = false;
            state.files[file].closed = true;
            state.files[file].closes += 1;
            state.close_threads.push(thread::current().id());
            service.changed.notify_all();
            continue;
        }
        if let Some(id) = state.queue.pop_front() {
            let request = state.requests.get_mut(&id).unwrap();
            request.phase = Phase::Running;
            let bytes = request.bytes;
            state.started.push(id);
            service.changed.notify_all();
            drop(state);
            gate.recv_timeout(WATCHDOG).expect("FS0 gate watchdog");
            let mut state = lock(&service);
            // A cancelled write may still have effects, but cannot publish to Joky.
            state.effects.push(bytes);
            let request = state.requests.get_mut(&id).unwrap();
            request.phase = Phase::Ready;
            let waker = request.waker.clone();
            if request.cancelled {
                Service::retire(&mut state, id);
            }
            service.changed.notify_all();
            drop(state);
            if let Some(waker) = waker {
                waker.wake();
            }
            continue;
        }
        if state.shutdown && state.files.iter().all(|file| !file.opened || file.closed) {
            state.exited = true;
            service.changed.notify_all();
            return;
        }
        drop(service.changed.wait(state).unwrap());
    }
}

struct FileToken {
    service: Arc<Service>,
    file: usize,
}
impl Drop for FileToken {
    fn drop(&mut self) {
        self.service.release_file(self.file);
    }
}
struct RequestToken {
    service: Arc<Service>,
    id: u64,
}
impl Drop for RequestToken {
    fn drop(&mut self) {
        self.service.stop(self.id);
    }
}
thread_local! {
    static CURRENT: RefCell<Option<Arc<Service>>> = const { RefCell::new(None) };
}
fn service() -> Arc<Service> {
    CURRENT.with(|slot| slot.borrow().as_ref().unwrap().clone())
}
// SAFETY: Test factories transfer exactly one boxed token; release runs on the
// task thread and only changes bounded service state. No worker sees the token.
unsafe extern "C" fn create_file(id: u64, output: *mut *mut c_void) -> i32 {
    let service = service();
    service.open(id as usize);
    *output = Box::into_raw(Box::new(FileToken {
        service,
        file: id as usize,
    }))
    .cast();
    0
}
unsafe extern "C" fn release_file(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<FileToken>()));
}
unsafe extern "C" fn create_request(file: u64, output: *mut *mut c_void) -> i32 {
    let service = service();
    let mut source = PAYLOAD;
    let Some(id) = service.submit(file as usize, &source, current_waker().unwrap()) else {
        return 7;
    };
    // Mutating the submitter's bytes cannot affect the worker-owned snapshot.
    source.fill(17);
    assert_eq!(source, [17; 4]);
    *output = Box::into_raw(Box::new(RequestToken { service, id })).cast();
    0
}
unsafe extern "C" fn release_request(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<RequestToken>()));
}
#[export_name = "__joky_resource_factory_v1_host_file_io_file"]
pub static FILE_FACTORY: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create_file),
    release: Some(release_file),
};
#[export_name = "__joky_resource_factory_v1_host_file_io_request"]
pub static REQUEST_FACTORY: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create_request),
    release: Some(release_request),
};
// SAFETY: Scalar-only test adapter, no retained frame/context/borrow. Its owning
// ProbeRequest handles revocation on structured cancellation. This is not a new
// ResourceIoV1 contract: production files need the separate descriptor in the plan.
#[no_mangle]
pub unsafe extern "C" fn host_file_io_wait(frame: *mut CpsFrame) -> i32 {
    let file = (&(*frame).slots)[0] as usize;
    let service = service();
    let mut state = lock(&service);
    let id = state.files[file].active.unwrap();
    let request = state.requests.get_mut(&id).unwrap();
    if request.phase != Phase::Ready {
        return 1;
    }
    assert!(!request.cancelled && !request.delivered);
    assert_eq!(request.bytes, PAYLOAD);
    request.delivered = true;
    state.delivered += 1;
    (*frame).result = 42;
    0
}
#[no_mangle]
pub extern "C" fn host_file_io_closed(file: i32) -> i32 {
    i32::from(lock(&service()).files[file as usize].closing)
}

struct ManualClock(Cell<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
impl ManualClock {
    fn advance(&self) {
        self.0.set(self.0.get() + Duration::from_millis(10));
    }
}
struct Fixture {
    service: Arc<Service>,
    gate: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
    clock: Rc<ManualClock>,
    executor: HostExecutor,
}
impl Fixture {
    fn new() -> Self {
        let service = Service::new();
        let (gate, receive) = mpsc::channel();
        let background = service.clone();
        let worker = thread::spawn(move || worker(background, receive));
        CURRENT.with(|slot| assert!(slot.borrow_mut().replace(service.clone()).is_none()));
        let clock = Rc::new(ManualClock(Cell::new(Instant::now())));
        let executor = HostExecutor::new(clock.clone(), 16);
        Self {
            service,
            gate,
            worker: Some(worker),
            clock,
            executor,
        }
    }
    fn start(&self, task: Continuation) -> TaskGroup {
        let group = TaskGroup::new();
        self.executor.spawn(&group, task).unwrap();
        let report = self.executor.poll(1).unwrap();
        assert!(report.completed.is_empty());
        group
    }
    fn complete(&self) {
        let deadline = Instant::now() + WATCHDOG;
        loop {
            let report = self.executor.poll(16).unwrap();
            if let Some(done) = report.completed.into_iter().next() {
                assert_eq!(done.result, Ok(42));
                return;
            }
            assert!(Instant::now() < deadline, "FS0 result was not delivered");
            thread::yield_now();
        }
    }
    fn cancel(&self, group: &TaskGroup) {
        self.executor.cancel_group(group);
    }
    fn finish(mut self) {
        assert_eq!(self.executor.task_count(), 0);
        self.service
            .wait(|s| s.requests.is_empty() && s.files.iter().all(|f| !f.opened || f.closed));
        {
            let mut state = lock(&self.service);
            assert_eq!(state.bytes, 0);
            assert!(state.queue.is_empty() && state.cleanup.is_empty());
            for file in &state.files {
                assert!(!file.wrapper);
                assert_eq!(file.closes, usize::from(file.opened));
            }
            assert!(state
                .close_threads
                .iter()
                .all(|id| *id != thread::current().id()));
            assert!(state.effects.iter().all(|bytes| *bytes == PAYLOAD));
            assert_eq!(state.effects.len(), state.started.len());
            state.shutdown = true;
        }
        self.service.changed.notify_all();
        self.service.wait(|s| s.exited);
        self.worker.take().unwrap().join().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Test-driver teardown only, never called from a language callback/poll.
        if self.worker.is_some() {
            drop(std::mem::take(&mut self.executor));
            {
                let mut state = lock(&self.service);
                state.shutdown = true;
                for file in 0..FILES {
                    if state.files[file].opened {
                        Service::close_locked(&mut state, file);
                    }
                }
            }
            for _ in 0..FILES {
                let _ = self.gate.send(());
            }
            self.service.changed.notify_all();
            let _ = self.worker.take().unwrap().join();
        }
        CURRENT.with(|slot| slot.borrow_mut().take());
    }
}

#[cfg(unix)]
pub const ENTRIES: [&str; 8] = [
    "first",
    "second",
    "third",
    "timeout_running",
    "timeout_queued",
    "timer",
    "immediate",
    "queued_reuse",
];

pub fn check_suite(mut prepare: impl FnMut(&str) -> Continuation) {
    // A deliberately blocked worker cannot prevent an independent root or timer.
    {
        let fixture = Fixture::new();
        let first = fixture.start(prepare("first"));
        fixture.service.wait(|s| s.started.len() == 1);
        for _ in 0..3 {
            assert_eq!(fixture.executor.poll(16).unwrap().polled, 0);
        }
        let timer = fixture.start(prepare("timer"));
        let ready = TaskGroup::new();
        fixture
            .executor
            .spawn(&ready, prepare("immediate"))
            .unwrap();
        fixture.complete();
        fixture.clock.advance();
        fixture.complete();
        assert!(lock(&fixture.service).effects.is_empty());
        fixture.gate.send(()).unwrap();
        fixture.complete();
        assert_eq!(lock(&fixture.service).delivered, 1);
        drop((first, timer, ready));
        fixture.finish();
    }
    // Full ordinary queue and in-flight cancellation retain the service budget;
    // destructors still enqueue reserved close work without waiting for the gate.
    {
        let fixture = Fixture::new();
        let first = fixture.start(prepare("first"));
        fixture.service.wait(|s| s.started.len() == 1);
        let second = fixture.start(prepare("second"));
        let third = TaskGroup::new();
        fixture.executor.spawn(&third, prepare("third")).unwrap();
        let rejected = fixture.executor.poll(1).unwrap();
        assert_eq!(rejected.completed.len(), 1);
        assert!(rejected.completed[0].result.is_err());
        {
            let state = lock(&fixture.service);
            assert_eq!(state.queue.len(), QUEUED);
            assert_eq!(state.cleanup.len(), 1);
            assert_eq!(state.busy, 1);
        }
        fixture.cancel(&second);
        fixture.cancel(&first);
        {
            let state = lock(&fixture.service);
            assert_eq!(state.requests.len(), 1);
            assert_eq!(state.bytes, 4);
            assert_eq!(state.cleanup.len(), 2);
            assert_eq!(state.request_releases, 2);
            assert_eq!(state.delivered, 0);
            assert!(state
                .requests
                .values()
                .all(|r| r.cancelled && r.waker.is_none()));
        }
        assert_eq!(fixture.executor.poll(16).unwrap().polled, 0);
        // A shutdown request must return while the detached operation is gated.
        lock(&fixture.service).shutdown = true;
        fixture.service.changed.notify_all();
        assert!(!lock(&fixture.service).exited);
        fixture.gate.send(()).unwrap();
        fixture.service.wait(|s| s.requests.is_empty());
        assert_eq!(fixture.executor.poll(16).unwrap().polled, 0);
        fixture.finish();
    }
    // Running cancellation closes the File before its outer owner is destroyed.
    {
        let fixture = Fixture::new();
        let group = fixture.start(prepare("timeout_running"));
        fixture.service.wait(|s| s.started.len() == 1);
        fixture.clock.advance();
        fixture.complete();
        assert_eq!(lock(&fixture.service).bytes, 4);
        fixture.gate.send(()).unwrap();
        drop(group);
        fixture.finish();
    }
    // Queue cancellation never starts the operation and preserves the live File.
    {
        let fixture = Fixture::new();
        let first = fixture.start(prepare("first"));
        fixture.service.wait(|s| s.started.len() == 1);
        let second = fixture.start(prepare("timeout_queued"));
        fixture.clock.advance();
        fixture.complete();
        assert_eq!(lock(&fixture.service).started.len(), 1);
        fixture.cancel(&first);
        fixture.gate.send(()).unwrap();
        drop(second);
        fixture.finish();
    }
    // A queued cancellation permits a new request with a fresh identity.
    {
        let fixture = Fixture::new();
        let first = fixture.start(prepare("first"));
        fixture.service.wait(|s| s.started.len() == 1);
        let second = fixture.start(prepare("queued_reuse"));
        fixture.clock.advance();
        assert!(fixture.executor.poll(16).unwrap().completed.is_empty());
        {
            let state = lock(&fixture.service);
            assert_eq!(state.files[1].active, Some(3));
            assert!(!state.files[1].closing);
        }
        fixture.cancel(&first);
        fixture.gate.send(()).unwrap();
        fixture.service.wait(|s| s.started.len() == 2);
        fixture.gate.send(()).unwrap();
        fixture.complete();
        drop(second);
        fixture.finish();
    }
    // Completion-before-cancel must not hand a stale result to the task.
    {
        let fixture = Fixture::new();
        let first = fixture.start(prepare("first"));
        fixture.service.wait(|s| s.started.len() == 1);
        let stale = lock(&fixture.service)
            .requests
            .values()
            .next()
            .unwrap()
            .waker
            .clone()
            .unwrap();
        fixture.gate.send(()).unwrap();
        fixture
            .service
            .wait(|s| s.requests.values().any(|r| r.phase == Phase::Ready));
        fixture.cancel(&first);
        assert_eq!(lock(&fixture.service).delivered, 0);
        assert_eq!(fixture.executor.poll(16).unwrap().polled, 0);
        let second = fixture.start(prepare("second"));
        fixture.service.wait(|s| s.started.len() == 2);
        stale.wake();
        assert_eq!(fixture.executor.poll(16).unwrap().polled, 0);
        fixture.gate.send(()).unwrap();
        fixture.complete();
        drop(second);
        fixture.finish();
    }
}
