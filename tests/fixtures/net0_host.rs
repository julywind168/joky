// Shared by JIT/interpreter tests and an independently linked AOT executable.
// Loopback setup and readiness delivery are test drivers, not a production reactor.
use joky::runtime::{
    borrowed_host::BorrowingHostV1,
    cps::CpsFrame,
    executor::{current_waker, Clock, HostExecutor, TaskGroup, TaskWaker},
    suspension::Continuation,
};
use std::{
    cell::{Cell, RefCell},
    io::{ErrorKind, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    rc::Rc,
    time::{Duration, Instant},
};

#[repr(C)]
struct NetRequest {
    marker: i32,
    mode: i32,
    limit: i32,
    received: i32,
    bytes: [u8; 4],
}
struct Operation {
    request: *mut NetRequest,
    socket: TcpStream,
    bytes: [u8; 4],
    received: usize,
}
thread_local! {
    static SOCKET: RefCell<Option<TcpStream>> = const { RefCell::new(None) };
    static PEER: RefCell<Option<TcpStream>> = const { RefCell::new(None) };
    static ACTIVE: Cell<*mut Operation> = const { Cell::new(std::ptr::null_mut()) };
    static TOKEN: RefCell<Option<TaskWaker>> = const { RefCell::new(None) };
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    static POLLS: Cell<usize> = const { Cell::new(0) };
}
#[no_mangle]
pub extern "C" fn net0_event(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
pub fn events() -> Vec<i32> {
    EVENTS.with(|events| events.borrow().clone())
}
pub fn active() -> bool {
    ACTIVE.with(|p| !p.get().is_null())
}
pub fn token() -> TaskWaker {
    TOKEN.with(|t| t.borrow().clone().unwrap())
}
pub fn teardown() {
    assert!(!active());
    SOCKET.with(|s| s.borrow_mut().take());
    PEER.with(|s| s.borrow_mut().take());
    TOKEN.with(|t| t.borrow_mut().take());
}
pub fn setup() {
    teardown();
    EVENTS.with(|e| e.borrow_mut().clear());
    POLLS.with(|n| n.set(0));
    // Blocking setup is outside all language polls. Only the accepted stream is
    // transferred to the operation; the test driver owns the peer and listener.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_nodelay(true).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (socket, _) = listener.accept().unwrap();
    socket.set_nonblocking(true).unwrap();
    SOCKET.with(|s| *s.borrow_mut() = Some(socket));
    PEER.with(|s| *s.borrow_mut() = Some(peer));
}
pub fn send(bytes: &[u8]) {
    PEER.with(|p| p.borrow_mut().as_mut().unwrap().write_all(bytes).unwrap());
}
pub fn eof() {
    PEER.with(|p| {
        p.borrow()
            .as_ref()
            .unwrap()
            .shutdown(Shutdown::Write)
            .unwrap()
    });
}
// No raw language/frame pointer is accessed by this driver. ACTIVE is removed
// synchronously by stop before the host-owned operation can be destroyed.
pub fn deliver_readiness() {
    ACTIVE.with(|slot| {
        let pointer = slot.get();
        if pointer.is_null() {
            return;
        }
        // SAFETY: Single task thread, live registered Operation, no poll/stop reentry.
        let op = unsafe { &*pointer };
        match op.socket.peek(&mut [0; 1]) {
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            _ => token().wake(),
        }
    });
}
pub fn received() -> usize {
    ACTIVE.with(|p| {
        let p = p.get();
        if p.is_null() {
            0
        } else {
            // SAFETY: Same serialized task thread; stop clears ACTIVE before free.
            unsafe { (*p).received }
        }
    })
}
// SAFETY: Registered as one exclusive arena-allocated NetRequest reference.
// The versioned wrapper arms stop before entry. No frame/slot address is retained.
unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
    POLLS.with(|n| n.set(n.get() + 1));
    if (*frame).host_context.is_null() {
        let request = (&(*frame).slots)[0] as *mut NetRequest;
        assert_eq!((*request).marker, 42);
        assert_eq!((*request).limit, 4);
        assert!(!active());
        let op = Box::new(Operation {
            request,
            socket: SOCKET.with(|s| s.borrow_mut().take().unwrap()),
            bytes: [0; 4],
            received: 0,
        });
        let op = Box::into_raw(op);
        (*frame).host_context = op.cast();
        ACTIVE.with(|p| p.set(op));
        TOKEN.with(|t| *t.borrow_mut() = current_waker());
        net0_event(1);
    }
    let op = &mut *(*frame).host_context.cast::<Operation>();
    if (*op.request).mode == 1 {
        return 7;
    }
    if op.received >= 2 && (*op.request).mode == 2 {
        return 7;
    }
    if op.received >= 2 && (*op.request).mode == 3 {
        (*frame).result = (-2_i32) as u32 as u64;
        return 0;
    }
    while op.received < 4 {
        match op.socket.read(&mut op.bytes[op.received..]) {
            Ok(0) => {
                (*frame).result = (-1_i32) as u32 as u64;
                return 0;
            }
            Ok(n) => {
                for &byte in &op.bytes[op.received..op.received + n] {
                    net0_event(1000 + i32::from(byte));
                }
                op.received += n;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return 1,
            Err(_) => {
                (*frame).result = (-2_i32) as u32 as u64;
                return 0;
            }
        }
    }
    (*op.request).bytes = op.bytes;
    (*frame).result = 4;
    0
}
// SAFETY: stop runs on the task thread before the request/arena/code is released.
// It first unregisters readiness, then destroys I/O state; repeated calls are inert.
unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    let pointer = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if pointer.is_null() {
        return;
    }
    let op = Box::from_raw(pointer.cast::<Operation>());
    assert_eq!(
        ACTIVE.with(|p| p.replace(std::ptr::null_mut())),
        &*op as *const _ as *mut _
    );
    assert_eq!((*op.request).marker, 42);
    (*op.request).marker = 43;
    (*op.request).received = op.received as i32;
    net0_event(2 + reason as i32);
    drop(op);
    net0_event(5);
}
#[export_name = "__joky_cps_borrow_v1_net0_read"]
pub static READ_HOST: BorrowingHostV1 = BorrowingHostV1 {
    abi_version: 1,
    poll: Some(poll),
    stop: Some(stop),
};
pub struct ManualClock(Cell<Instant>);
impl ManualClock {
    pub fn new() -> Rc<Self> {
        Rc::new(Self(Cell::new(Instant::now())))
    }
    pub fn advance(&self) {
        self.0.set(self.0.get() + Duration::from_millis(10));
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
pub fn await_partial(mut drive: impl FnMut()) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while received() != 2 {
        assert!(
            active() && Instant::now() < deadline,
            "partial read stalled: {:?}",
            events()
        );
        deliver_readiness();
        drive();
        std::thread::yield_now();
    }
}
pub fn assert_cleanup(stop_event: i32, packet: bool, parent: Option<i32>) {
    assert!(!active());
    let events = events();
    assert_eq!(events.iter().filter(|&&n| (2..=4).contains(&n)).count(), 1);
    for event in [1, stop_event, 5] {
        assert_eq!(
            events.iter().filter(|&&n| n == event).count(),
            1,
            "{events:?}"
        );
    }
    let stop = events.iter().position(|&n| n == stop_event).unwrap();
    let close = events.iter().position(|&n| n == 5).unwrap();
    assert!(stop < close, "{events:?}");
    assert_eq!(
        events.iter().filter(|&&n| n == 21).count(),
        usize::from(stop_event == 2)
    );
    if stop_event == 2 {
        assert!(close < events.iter().position(|&n| n == 21).unwrap());
    }
    assert_eq!(
        events.iter().filter(|&&n| n == 20).count(),
        usize::from(packet)
    );
    if let Some(parent) = parent {
        assert!(close < events.iter().position(|&n| n == parent).unwrap());
    }
    assert!(!events.contains(&99));
}
pub const ENTRIES: [&str; 8] = [
    "data",
    "eof",
    "domain_error",
    "startup_failure",
    "pending_failure",
    "timeout",
    "sibling_failure",
    "unstarted",
];

pub fn check_suite(mut prepare: impl FnMut(&str) -> Continuation) {
    for entry in ENTRIES {
        setup();
        let clock = ManualClock::new();
        let executor = HostExecutor::new(clock.clone(), 8);
        let group = TaskGroup::new();
        executor.spawn(&group, prepare(entry)).unwrap();
        let first = executor.poll(1).unwrap();
        if entry == "unstarted" {
            assert_eq!(first.completed.len(), 1);
            assert_eq!(first.completed[0].result, Ok(42));
            assert!(events().is_empty());
            assert!(!active());
            assert_eq!(executor.task_count(), 0);
            continue;
        }
        if entry == "startup_failure" {
            assert!(first.completed[0].result.is_err());
            assert_cleanup(3, false, None);
            continue;
        }
        assert!(active());
        let stale = token();
        let polls = POLLS.with(Cell::get);
        for _ in 0..4 {
            assert_eq!(executor.poll(1).unwrap().polled, 0);
        }
        assert_eq!(POLLS.with(Cell::get), polls);
        send(&[0, 128]);
        await_partial(|| {
            assert!(executor.poll(1).unwrap().completed.is_empty());
        });
        // A spurious wake must not replay startup or bytes already consumed.
        stale.wake();
        if entry != "pending_failure" && entry != "domain_error" {
            assert!(executor.poll(1).unwrap().completed.is_empty());
            assert_eq!(received(), 2);
        }
        match entry {
            "data" => send(&[255, 42]),
            "eof" => eof(),
            "timeout" | "sibling_failure" => clock.advance(),
            _ => {}
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            deliver_readiness();
            let report = executor.poll(1).unwrap();
            if let Some(done) = report.completed.into_iter().next() {
                break done.result;
            }
            assert!(Instant::now() < deadline, "{entry} did not finish");
            std::thread::yield_now();
        };
        if entry == "pending_failure" || entry == "sibling_failure" {
            assert!(result.is_err());
        } else {
            assert_eq!(result, Ok(42), "{entry}");
        }
        let stopped = if entry == "pending_failure" {
            3
        } else if entry == "timeout" || entry == "sibling_failure" {
            4
        } else {
            2
        };
        let parent = match entry {
            "data" | "eof" | "domain_error" => Some(10),
            "timeout" => Some(11),
            _ => None,
        };
        assert_cleanup(stopped, entry == "data", parent);
        for (event, expected_entry) in [(12, "eof"), (13, "domain_error")] {
            assert_eq!(
                events().iter().filter(|&&n| n == event).count(),
                usize::from(entry == expected_entry)
            );
        }
        let expected = if entry == "data" {
            vec![1000, 1128, 1255, 1042]
        } else {
            vec![1000, 1128]
        };
        assert_eq!(
            events()
                .into_iter()
                .filter(|n| *n >= 1000)
                .collect::<Vec<_>>(),
            expected
        );
        stale.wake();
        deliver_readiness();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        assert_eq!(executor.task_count(), 0);
    }
    setup();
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor.spawn(&group, prepare("data")).unwrap();
    executor.poll(1).unwrap();
    let stale = token();
    send(&[0, 128]);
    await_partial(|| {
        executor.poll(1).unwrap();
    });
    assert!(executor.cancel_group(&group));
    assert_cleanup(4, false, None);
    stale.wake();
    deliver_readiness();
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    teardown();
}
