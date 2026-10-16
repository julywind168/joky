use super::*;
use crate::runtime::{
    cps::{CpsFrame, DONE},
    executor::{current_waker, HostExecutor, TaskGroup},
    suspension::Continuation,
};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
thread_local! {static WAKE:RefCell<Option<TaskWaker>>=const{RefCell::new(None)};}
unsafe extern "C" fn capture(frame: *mut CpsFrame) -> i32 {
    WAKE.with(|s| s.replace(current_waker()));
    (*frame).state = DONE;
    0
}
fn waker() -> TaskWaker {
    let executor = HostExecutor::default();
    let task = unsafe { Continuation::from_native(capture, CpsFrame::new(0), Rc::new(())) };
    executor.spawn(&TaskGroup::new(), task).unwrap();
    executor.poll(1).unwrap();
    WAKE.with(|s| s.take().unwrap())
}
fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < end, "worker did not make progress");
        thread::yield_now();
    }
}
#[derive(Default)]
struct State {
    released: Mutex<bool>,
    signal: Condvar,
    entered: AtomicUsize,
    closed: AtomicUsize,
}
impl State {
    fn wait(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut open = lock(&self.released);
        while !*open {
            open = self.signal.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *lock(&self.released) = true;
        self.signal.notify_all();
    }
}
struct Backend(Arc<State>);
struct TestFile {
    state: Arc<State>,
    blocked: bool,
}
impl FileSystem for Backend {
    fn open(&self, path: &str, _: u64) -> Result<Box<dyn FileHandle>, FsError> {
        if path == "open" {
            self.0.wait();
        }
        if path == "error" {
            return Err(FsError::new(3, "missing"));
        }
        Ok(Box::new(TestFile {
            state: self.0.clone(),
            blocked: path == "read",
        }))
    }
}
impl FileHandle for TestFile {
    fn read(&mut self, b: &mut [u8]) -> Result<usize, FsError> {
        if self.blocked {
            self.state.wait();
        }
        b[0] = 42;
        Ok(1)
    }
    fn write(&mut self, b: &[u8]) -> Result<usize, FsError> {
        Ok(b.len())
    }
    fn seek(&mut self, _: u32, n: u64) -> Result<u64, FsError> {
        Ok(n)
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        Ok(())
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        self.state.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Fixture {
    service: NativeService,
    state: Arc<State>,
    wake: TaskWaker,
}
impl Fixture {
    fn new() -> Self {
        let state = Arc::new(State::default());
        let service = NativeService::new(
            Arc::new(Backend(state.clone())),
            FileLimits {
                workers: 1,
                queued: 1,
                requests: 4,
                files: 4,
                bytes: 32768,
            },
        )
        .unwrap();
        Self {
            service,
            state,
            wake: waker(),
        }
    }
    fn submit(&self, input: Input<'_>, file: Option<Arc<NativeFile>>) -> Arc<NativeTicket> {
        self.service.submit(input, file, self.wake.clone()).unwrap()
    }
    fn ready(&self, t: &NativeTicket) {
        until(|| lock(&t.state).result.is_some());
    }
    fn open(&self, name: &str) -> Arc<NativeFile> {
        let t = self.submit(Input::Open(name, 1), None);
        self.ready(&t);
        let mut p = t.take().unwrap();
        t.stop();
        let Some(Data::File(f)) = p.data.take() else {
            panic!("open failed")
        };
        let FileCell::Native(f) = &*f else {
            unreachable!()
        };
        f.clone()
    }
    fn drained(&self) {
        self.service.shutdown();
        until(|| self.service.is_quiescent());
        assert_eq!(self.service.usage(), (0, 0, 0));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.release();
        self.service.shutdown();
    }
}
#[test]
fn queued_cancellation_preserves_file_and_close_bypasses_full_queue() {
    let f = Fixture::new();
    let file = f.open("normal");
    let running = f.submit(Input::Open("open", 1), None);
    until(|| f.state.entered.load(Ordering::SeqCst) == 1);
    let queued = f.submit(Input::Read(8), Some(file.clone()));
    queued.stop();
    drop(queued);
    assert!(!file.is_closed());
    let queued = f.submit(Input::Read(8), Some(file.clone()));
    queued.stop();
    drop(queued);
    let queued = f.submit(Input::Open("queued", 1), None);
    assert!(matches!(
        f.service
            .submit(Input::Open("overflow", 1), None, f.wake.clone()),
        Err(FsError { code: 6, .. })
    ));
    file.close(Some(f.wake.clone()));
    assert!(file.is_closed());
    assert!(file.close_result().is_none());
    queued.stop();
    drop(queued);
    running.stop();
    drop(running);
    assert_eq!(
        f.service.usage().1,
        1,
        "cancelled running work retains its permit"
    );
    f.state.release();
    until(|| file.close_result().is_some());
    drop(file);
    f.drained();
    assert_eq!(f.state.closed.load(Ordering::SeqCst), 2);
}
#[test]
fn running_read_cancellation_closes_without_waiting() {
    let f = Fixture::new();
    let file = f.open("read");
    let request = f.submit(Input::Read(1024), Some(file.clone()));
    until(|| f.state.entered.load(Ordering::SeqCst) == 1);
    request.stop();
    drop(request);
    assert!(file.is_closed());
    assert!(file.close_result().is_none());
    assert_eq!(f.service.usage().1, 1);
    assert!(matches!(
        f.service
            .submit(Input::Read(1), Some(file.clone()), f.wake.clone()),
        Err(FsError { code: 7, .. })
    ));
    f.state.release();
    until(|| file.close_result().is_some());
    drop(file);
    f.drained();
    assert_eq!(f.state.closed.load(Ordering::SeqCst), 1);
}
#[test]
fn ready_unclaimed_open_is_closed_and_result_storage_is_charged() {
    let f = Fixture::new();
    let t = f.submit(Input::Open("normal", 1), None);
    f.ready(&t);
    t.stop();
    drop(t);
    f.drained();
    assert_eq!(f.state.closed.load(Ordering::SeqCst), 1);
    let f = Fixture::new();
    let file = f.open("normal");
    let t = f.submit(Input::Read(1024), Some(file.clone()));
    f.ready(&t);
    let packet = t.take().unwrap();
    t.stop();
    drop(t);
    assert_eq!(f.service.usage().1, 1);
    assert!(f.service.usage().2 >= 5120);
    drop(packet);
    assert_eq!(f.service.usage().1, 0);
    file.close(None);
    drop(file);
    f.drained();
}
#[test]
fn shutdown_wakes_queued_and_running_work_drains_after_release() {
    let f = Fixture::new();
    let running = f.submit(Input::Open("open", 1), None);
    until(|| f.state.entered.load(Ordering::SeqCst) == 1);
    let queued = f.submit(Input::Open("normal", 1), None);
    f.service.shutdown();
    assert_eq!(queued.take().unwrap().error.as_ref().unwrap().code, 7);
    queued.stop();
    drop(queued);
    running.stop();
    drop(running);
    assert!(!f.service.is_quiescent());
    f.state.release();
    f.drained();
    assert_eq!(f.state.closed.load(Ordering::SeqCst), 1);
}
#[test]
fn open_failure_releases_reserved_cleanup_and_storage() {
    let f = Fixture::new();
    let t = f.submit(Input::Open("error", 1), None);
    f.ready(&t);
    assert_eq!(t.take().unwrap().error.as_ref().unwrap().code, 3);
    t.stop();
    drop(t);
    f.drained();
}
#[test]
fn ready_read_cancel_closes_and_successful_read_keeps_file_open() {
    for cancelled in [true, false] {
        let f = Fixture::new();
        let file = f.open("normal");
        let t = f.submit(Input::Read(8), Some(file.clone()));
        f.ready(&t);
        if !cancelled {
            drop(t.take().unwrap());
        }
        t.stop();
        drop(t);
        assert_eq!(file.is_closed(), cancelled);
        file.close(None);
        drop(file);
        f.drained();
    }
}
#[test]
fn file_request_and_storage_admission_are_independent() {
    let state = Arc::new(State::default());
    let wake = waker();
    let small = NativeService::new(
        Arc::new(Backend(state.clone())),
        FileLimits {
            workers: 1,
            queued: 1,
            requests: 1,
            files: 1,
            bytes: 4096,
        },
    )
    .unwrap();
    assert!(matches!(
        small.submit(Input::Open("normal", 1), None, wake.clone()),
        Err(FsError { code: 6, .. })
    ));
    assert_eq!(small.usage(), (0, 0, 0));
    small.shutdown();
    until(|| small.is_quiescent());
    let service = NativeService::new(
        Arc::new(Backend(state)),
        FileLimits {
            workers: 1,
            queued: 1,
            requests: 1,
            files: 1,
            bytes: 8192,
        },
    )
    .unwrap();
    let t = service
        .submit(Input::Open("normal", 1), None, wake.clone())
        .unwrap();
    until(|| lock(&t.state).result.is_some());
    let mut packet = t.take().unwrap();
    t.stop();
    drop(t);
    let Some(Data::File(cell)) = packet.data.take() else {
        panic!("expected file")
    };
    assert!(
        matches!(
            service.submit(Input::Open("normal", 1), None, wake.clone()),
            Err(FsError { code: 6, .. })
        ),
        "unclaimed result retains request quota"
    );
    drop(packet);
    assert!(
        matches!(
            service.submit(Input::Open("normal", 1), None, wake),
            Err(FsError { code: 6, .. })
        ),
        "live file retains file quota"
    );
    cell.close(None);
    drop(cell);
    service.shutdown();
    until(|| service.is_quiescent());
    assert_eq!(service.usage(), (0, 0, 0));
}
#[test]
fn failed_result_handoff_closes_the_file() {
    let f = Fixture::new();
    let file = f.open("normal");
    let t = f.submit(Input::Read(8), Some(file.clone()));
    f.ready(&t);
    drop(t.take().unwrap());
    t.fail();
    drop(t);
    assert!(file.is_closed());
    drop(file);
    f.drained();
}
#[test]
fn result_allocation_failure_reclaims_an_unclaimed_open_file() {
    let f = Fixture::new();
    let file = f.open("normal");
    let packet = Packet::ok(Data::File(Arc::new(FileCell::Native(file.clone()))));
    let budget = Rc::new(crate::runtime::arena::Budget {
        bytes: Default::default(),
        limit: std::cell::Cell::new(1),
    });
    let mut arena = crate::runtime::arena::Arena::with_budget(budget.clone(), None);
    assert!(super::super::outcome(&mut arena, packet, None).is_err());
    assert_eq!(budget.bytes.get(), 0);
    assert!(file.is_closed());
    drop(file);
    f.drained();
}
#[test]
fn shutdown_closes_ready_and_running_files_before_results_are_claimed() {
    for running in [true, false] {
        let f = Fixture::new();
        let request = f.submit(
            Input::Open(if running { "open" } else { "normal" }, 1),
            None,
        );
        if running {
            until(|| f.state.entered.load(Ordering::SeqCst) == 1);
        } else {
            f.ready(&request);
        }
        f.service.shutdown();
        f.state.release();
        until(|| f.state.closed.load(Ordering::SeqCst) == 1);
        assert!(
            !f.service.is_quiescent(),
            "unclaimed result still retains its storage quota"
        );
        drop(request.take());
        request.stop();
        drop(request);
        f.drained();
    }
}
