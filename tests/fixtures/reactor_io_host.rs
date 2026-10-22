use joky::runtime::{
    byte_request::{ByteRequest, ResourceIoV1},
    cps::CpsFrame,
    executor::{current_reactor, current_waker, Clock, HostExecutor, TaskGroup, TaskWaker},
    reactor::{net, Interest, IoRegistration, IoSource, Reactor},
    resource::{ResourceEvent, ResourceFactoryV1},
    suspension::Continuation,
};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    rc::Rc,
    time::{Duration, Instant},
};
struct Socket {
    source: IoSource<net::TcpStream>,
}
struct Operation {
    registration: IoRegistration<net::TcpStream>,
}
thread_local! {
    static AUTO: Cell<u8> = const {Cell::new(0)};
    static WORKERS: RefCell<Vec<std::thread::JoinHandle<()>>> = const {RefCell::new(Vec::new())};
    static PEERS: RefCell<Vec<(TcpStream,bool)>> = const {RefCell::new(Vec::new())};
    static LIVE: Cell<usize> = const {Cell::new(0)};
    static POLLS: Cell<usize> = const {Cell::new(0)};
    static STOPS: Cell<usize> = const {Cell::new(0)};
    static TOKENS: RefCell<Vec<TaskWaker>> = const {RefCell::new(Vec::new())};
    static EVENTS: RefCell<Vec<ResourceEvent>> = const {RefCell::new(Vec::new())};
    static REACTOR: RefCell<Option<Reactor>> = const {RefCell::new(None)};
}
unsafe extern "C" fn create(_: u64, output: *mut *mut c_void) -> i32 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    stream.set_nonblocking(true).unwrap();
    PEERS.with(|p| p.borrow_mut().push((peer, false)));
    *output = Box::into_raw(Box::new(Socket {
        source: IoSource::new(net::TcpStream::from_std(stream)),
    }))
    .cast();
    LIVE.with(|n| n.set(n.get() + 1));
    0
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Socket>()));
    LIVE.with(|n| n.set(n.get() - 1));
}
#[export_name = "__joky_resource_factory_v1_reactor_io_make"]
pub static FACTORY: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create),
    release: Some(release),
};
unsafe extern "C" fn poll(
    frame: *mut CpsFrame,
    request: *mut ByteRequest,
    count: *mut usize,
) -> i32 {
    POLLS.with(|n| n.set(n.get() + 1));
    let request = &mut *request;
    let socket = &mut *request.resource().cast::<Socket>();
    if (*frame).host_context.is_null() {
        let registration = socket
            .source
            .register(Interest::READABLE, Some(request.event()))
            .unwrap();
        TOKENS.with(|t| t.borrow_mut().push(current_waker().unwrap()));
        EVENTS.with(|e| e.borrow_mut().push(request.event()));
        REACTOR.with(|r| *r.borrow_mut() = Some(current_reactor().unwrap()));
        (*frame).host_context = Box::into_raw(Box::new(Operation { registration })).cast();
    }
    match socket.source.with_io(|s| s.read(request.bytes_mut())) {
        Ok(n) => {
            *count = n;
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            (*(*frame).host_context.cast::<Operation>())
                .registration
                .rearm(Interest::READABLE)
                .unwrap();
            let auto = AUTO.with(Cell::get);
            if auto != 0 {
                let peers = PEERS.with(|p| std::mem::take(&mut *p.borrow_mut()));
                for (mut peer, _) in peers {
                    WORKERS.with(|w| {
                        w.borrow_mut().push(std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(2));
                            if auto == 1 {
                                let _ = peer.write_all(&[0, 128, 255, 42]);
                            }
                            let _ = peer.shutdown(Shutdown::Write);
                        }))
                    });
                }
            }
            1
        }
        Err(_) => 7,
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, request: *mut ByteRequest, _: u32) {
    assert!(!(*request).event().is_active());
    let mut operation = Box::from_raw((*frame).host_context.cast::<Operation>());
    operation.registration.stop().unwrap();
    STOPS.with(|n| n.set(n.get() + 1));
    (*frame).host_context = std::ptr::null_mut();
}
#[export_name = "__joky_resource_io_v1_reactor_io_read"]
pub static READ: ResourceIoV1 = ResourceIoV1 {
    abi_version: 1,
    poll: Some(poll),
    stop: Some(stop),
};
pub fn setup() {
    check();
    AUTO.with(|a| a.set(0));
    PEERS.with(|p| p.borrow_mut().clear());
    POLLS.with(|p| p.set(0));
    STOPS.with(|p| p.set(0));
    TOKENS.with(|p| p.borrow_mut().clear());
    EVENTS.with(|p| p.borrow_mut().clear());
    REACTOR.with(|p| p.borrow_mut().take());
}
pub fn publish(bytes: &[u8]) {
    PEERS.with(|p| {
        for (peer, sent) in p.borrow_mut().iter_mut() {
            if !*sent {
                peer.write_all(bytes).unwrap();
                peer.shutdown(Shutdown::Write).unwrap();
                *sent = true;
            }
        }
    });
}
pub fn polls() -> usize {
    POLLS.with(Cell::get)
}
pub fn wake() {
    TOKENS.with(|t| {
        for token in t.borrow().iter() {
            token.wake()
        }
    });
}
pub fn check() {
    WORKERS.with(|w| {
        for worker in w.borrow_mut().drain(..) {
            worker.join().unwrap();
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0));
    EVENTS.with(|e| {
        let e = e.borrow();
        assert!(e.iter().all(|e| !e.is_active()));
        STOPS.with(|n| assert_eq!(n.get(), e.len()));
    });
    REACTOR.with(|r| {
        if let Some(r) = r.borrow().as_ref() {
            assert_eq!(r.registration_count(), 0);
        }
    });
}
pub struct ManualClock(Cell<Instant>);
impl ManualClock {
    pub fn new() -> Rc<Self> {
        Rc::new(Self(Cell::new(Instant::now())))
    }
    pub fn advance(&self) {
        self.0.set(self.0.get() + Duration::from_millis(20));
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
pub fn suite(mut prepare: impl FnMut(&str) -> Continuation) {
    for entry in [
        "receive",
        "eof",
        "timeout",
        "sibling_failure",
        "reuse",
        "unstarted",
        "dynamic",
    ] {
        for cancel in [false, true] {
            setup();
            let clock = ManualClock::new();
            let executor = HostExecutor::new(clock.clone(), 16);
            let group = TaskGroup::new();
            executor.spawn(&group, prepare(entry)).unwrap();
            executor.poll(1).unwrap();
            if cancel {
                executor.cancel_group(&group);
            } else {
                let before = polls();
                for _ in 0..8 {
                    assert_eq!(executor.poll(0).unwrap().polled, 0);
                }
                assert_eq!(polls(), before);
                // A false notification must only retry, never complete unknown I/O.
                wake();
                executor.poll(1).unwrap();
                if entry == "timeout" || entry == "sibling_failure" {
                    clock.advance();
                } else {
                    publish(if entry == "eof" {
                        &[]
                    } else {
                        &[0, 128, 255, 42]
                    });
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let report = executor.poll(1).unwrap();
                    if let Some(done) = report.completed.into_iter().next() {
                        if entry == "sibling_failure" {
                            assert!(done.result.is_err());
                        } else {
                            assert_eq!(done.result, Ok(42), "{entry}");
                        }
                        break;
                    }
                    assert!(Instant::now() < deadline, "{entry}");
                    std::thread::yield_now();
                }
            }
            assert_eq!(executor.task_count(), 0);
            check();
            wake();
            assert_eq!(executor.poll(1).unwrap().polled, 0);
        }
    }
}

pub fn blocking_suite(mut drive: impl FnMut(&str) -> Result<u64, joky::Error>) {
    for entry in [
        "receive",
        "eof",
        "timeout",
        "sibling_failure",
        "reuse",
        "unstarted",
        "dynamic",
    ] {
        setup();
        AUTO.with(|a| {
            a.set(if entry == "eof" {
                2
            } else if entry == "timeout" || entry == "sibling_failure" {
                0
            } else {
                1
            })
        });
        let result = drive(entry);
        if entry == "sibling_failure" {
            assert!(result.is_err());
        } else {
            assert_eq!(result, Ok(42), "{entry}");
        }
        check();
    }
    AUTO.with(|a| a.set(0));
}
