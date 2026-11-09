use super::*;
use crate::runtime::{
    cps::CpsFrame,
    executor::{current_waker, HostExecutor, TaskGroup},
    suspension::Continuation,
};
use std::{cell::RefCell, io::Write, net::TcpListener, rc::Rc};

thread_local! { static CAPTURE: RefCell<Option<TaskWaker>> = const { RefCell::new(None) }; }
unsafe extern "C" fn capture(_: *mut CpsFrame) -> i32 {
    CAPTURE.with(|c| *c.borrow_mut() = current_waker());
    1
}
fn target() -> (HostExecutor, TaskGroup, TaskWaker) {
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor
        .spawn(&group, unsafe {
            Continuation::from_native(capture, CpsFrame::new(0), Rc::new(()))
        })
        .unwrap();
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    let waker = CAPTURE.with(|c| c.borrow_mut().take().unwrap());
    (executor, group, waker)
}
#[derive(Default)]
struct FakeSource {
    fail_stop: bool,
    fail_rearm: bool,
    fail_register: bool,
    stops: usize,
}
impl Source for FakeSource {
    fn register(&mut self, _: &Registry, _: Token, _: Interest) -> io::Result<()> {
        if self.fail_register {
            Err(io::Error::other("injected register failure"))
        } else {
            Ok(())
        }
    }
    fn reregister(&mut self, _: &Registry, _: Token, _: Interest) -> io::Result<()> {
        if self.fail_rearm {
            Err(io::Error::other("injected reregister failure"))
        } else {
            Ok(())
        }
    }
    fn deregister(&mut self, _: &Registry) -> io::Result<()> {
        self.stops += 1;
        if self.fail_stop {
            Err(io::Error::other("injected deregistration failure"))
        } else {
            Ok(())
        }
    }
}
#[test]
fn reactor_identity_limits_cancel_and_failed_deregistration_are_safe() {
    let (executor, group, waker) = target();
    let reactor = Reactor::new(1, 1).unwrap();
    let source = IoSource::new(FakeSource::default());
    let event = ResourceEvent::new().unwrap();
    let mut first = source
        .register_with(
            &reactor,
            Interest::READABLE,
            Some(event.clone()),
            waker.clone(),
        )
        .unwrap();
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    assert!(IoSource::new(FakeSource::default())
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    assert!(reactor.notify(first.token));
    assert!(reactor.notify(first.token));
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    event.invalidate();
    assert!(!reactor.notify(first.token));
    let stale = first.token;
    first.stop().unwrap();
    let other = Reactor::new(1, 1).unwrap();
    assert!(source
        .register_with(&other, Interest::READABLE, None, waker.clone())
        .is_err());
    let mut next = source
        .register_with(&reactor, Interest::WRITABLE, None, waker.clone())
        .unwrap();
    assert_ne!(stale, next.token);
    assert!(!reactor.notify(stale));
    next.rearm(Interest::READABLE).unwrap();
    executor.cancel_group(&group);
    reactor.notify(next.token);
    assert!(!executor.poll(1).unwrap().ready);
    source.with_io(|s| s.fail_stop = true);
    assert!(next.stop().is_err());
    assert!(!reactor.notify(next.token));
    assert_eq!(reactor.registration_count(), 0);
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    lock(&reactor.0.table).next = usize::MAX;
    let clean = IoSource::new(FakeSource::default());
    for _ in 0..2 {
        assert!(clean
            .register_with(&reactor, Interest::READABLE, None, waker.clone())
            .is_err());
        assert_eq!(reactor.registration_count(), 0);
    }
}
#[test]
fn reactor_bounded_batches_retain_uncollected_kernel_readiness() {
    let (_executor, _group, waker) = target();
    let reactor = Reactor::new(4, 1).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut sources = Vec::new();
    let mut registrations = Vec::new();
    let mut peers = Vec::new();
    for _ in 0..4 {
        let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        stream.set_nonblocking(true).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let source = IoSource::new(net::TcpStream::from_std(stream));
        registrations.push(
            source
                .register_with(&reactor, Interest::READABLE, None, waker.clone())
                .unwrap(),
        );
        peer.write_all(&[42]).unwrap();
        peers.push(peer);
        sources.push(source);
    }
    let mut delivered = 0;
    for _ in 0..20 {
        let report = reactor.poll(Some(Duration::from_millis(100))).unwrap();
        assert!(report.events <= 1);
        delivered += report.notified;
        if delivered >= 4 {
            break;
        }
    }
    assert!(delivered >= 4);
    drop(registrations);
    assert_eq!(reactor.registration_count(), 0);
    assert_eq!(reactor.poll(Some(Duration::ZERO)).unwrap().notified, 0);
    drop((sources, peers));
}

#[test]
fn reactor_latches_readiness_until_the_operation_rearms_or_stops() {
    let (executor, group, waker) = target();
    let reactor = Reactor::new(1, 1).unwrap();
    let source = IoSource::new(FakeSource::default());
    let mut registration = source
        .register_with(&reactor, Interest::READABLE, None, waker)
        .unwrap();
    assert!(!registration.is_ready());
    assert!(reactor.notify(registration.token));
    assert!(registration.is_ready());
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    // A budget slice may consume the root wake before reaching this operation.
    for _ in 0..3 {
        assert_eq!(reactor.poll(Some(Duration::ZERO)).unwrap().notified, 1);
        assert_eq!(executor.poll(1).unwrap().polled, 1);
    }
    registration.rearm(Interest::READABLE).unwrap();
    assert!(!registration.is_ready());
    assert_eq!(reactor.poll(Some(Duration::ZERO)).unwrap().notified, 0);
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    assert!(reactor.notify(registration.token));
    executor.poll(1).unwrap();
    registration.stop().unwrap();
    assert_eq!(reactor.poll(Some(Duration::ZERO)).unwrap().notified, 0);
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    executor.cancel_group(&group);
}

#[test]
fn net5b_reactor_direction_hints_limits_and_stale_tokens_are_independent() {
    let (_, _, waker) = target();
    let reactor = Reactor::new(2, 4).unwrap();
    let source = IoSource::new(FakeSource::default());
    let mut read = source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .unwrap();
    let kernel = lock(&source.0).token.unwrap();
    let mut write = source
        .register_with(&reactor, Interest::WRITABLE, None, waker.clone())
        .unwrap();
    assert_eq!(lock(&source.0).token, Some(kernel));
    assert_eq!(reactor.registration_count(), 2);
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    assert!(IoSource::new(FakeSource::default())
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    reactor.notify(read.token);
    reactor.notify(write.token);
    write.rearm(Interest::WRITABLE).unwrap();
    assert!(read.is_ready());
    assert!(!write.is_ready());
    reactor.notify(write.token);
    read.stop().unwrap();
    assert!(write.is_ready());
    assert_eq!(reactor.registration_count(), 1);
    assert!(lock(&reactor.0.table).routes[&kernel][0].is_none());
    let next_read = source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .unwrap();
    assert_ne!(read.token, next_read.token);
    assert!(!reactor.notify(read.token));
    assert!(write.is_ready());
    drop(next_read);
    write.stop().unwrap();
    assert!(lock(&reactor.0.table).routes.is_empty());
    let next = source
        .register_with(&reactor, Interest::READABLE, None, waker)
        .unwrap();
    assert_ne!(kernel, lock(&source.0).token.unwrap());
    drop(next);
    let limited = Reactor::new(1, 1).unwrap();
    let (_, _, waker) = target();
    let source = IoSource::new(FakeSource::default());
    let read = source
        .register_with(&limited, Interest::READABLE, None, waker.clone())
        .unwrap();
    assert!(source
        .register_with(&limited, Interest::WRITABLE, None, waker)
        .is_err());
    assert!(!lock(&source.0).poisoned);
    drop(read);
}
#[test]
fn net5b_reactor_failed_interest_changes_revoke_routes_and_wake_the_peer() {
    for operation in 0..3 {
        let (executor, _group, waker) = target();
        let reactor = Reactor::new(2, 2).unwrap();
        let source = IoSource::new(FakeSource::default());
        let mut read = source
            .register_with(&reactor, Interest::READABLE, None, waker.clone())
            .unwrap();
        if operation == 0 {
            source.with_io(|s| s.fail_rearm = true);
            assert!(source
                .register_with(&reactor, Interest::WRITABLE, None, waker)
                .is_err());
        } else {
            let mut write = source
                .register_with(&reactor, Interest::WRITABLE, None, waker)
                .unwrap();
            source.with_io(|s| s.fail_rearm = true);
            if operation == 1 {
                assert!(write.rearm(Interest::WRITABLE).is_err());
            } else {
                assert!(write.stop().is_err());
            }
            assert!(read.is_ready());
            assert!(write.stop().is_err() || operation == 2);
        }
        assert_eq!(executor.poll(1).unwrap().polled, 1);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        assert!(lock(&source.0).poisoned);
        assert!(lock(&reactor.0.table).routes.is_empty());
        assert!(read.is_ready());
        assert!(read.rearm(Interest::READABLE).is_err());
        assert!(read.stop().is_err());
        assert_eq!(reactor.registration_count(), 0);
    }
}

#[test]
fn net5b_reactor_failed_registration_never_publishes_or_reuses_a_source() {
    let (_, _, waker) = target();
    let reactor = Reactor::new(2, 2).unwrap();
    let source = IoSource::new(FakeSource {
        fail_register: true,
        ..Default::default()
    });
    let stale = lock(&reactor.0.table).next;
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    assert!(lock(&source.0).poisoned);
    assert!(lock(&reactor.0.table).routes.is_empty());
    assert_eq!(reactor.registration_count(), 0);
    assert!(!reactor.notify(stale));
    source.with_io(|s| s.fail_register = false);
    assert!(source
        .register_with(&reactor, Interest::WRITABLE, None, waker.clone())
        .is_err());
    let fresh = IoSource::new(FakeSource::default());
    let registration = fresh
        .register_with(&reactor, Interest::READABLE, None, waker)
        .unwrap();
    assert_ne!(stale, registration.token);
}

#[test]
fn shared_reactor_admission_is_atomic_across_threads() {
    use std::{sync::Barrier, thread};

    let reactor = Reactor::new(1, 1).unwrap();
    let start = Arc::new(Barrier::new(4));
    let admitted = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let reactor = reactor.clone();
            let start = start.clone();
            let admitted = admitted.clone();
            thread::spawn(move || {
                let (_executor, _group, waker) = target();
                let source = IoSource::new(FakeSource::default());
                start.wait();
                let socket = reactor.reserve_socket();
                let registration = source.register_with(&reactor, Interest::READABLE, None, waker);
                // Keep both successful reservations alive until all contenders
                // have attempted admission; neither limit is per-thread.
                admitted.wait();
                (socket, registration)
            })
        })
        .collect();
    start.wait();
    admitted.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(s, _)| s.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
    for (socket, registration) in &results {
        if let Err(error) = socket {
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
        }
        if let Err(error) = registration {
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
        }
    }
    assert_eq!(reactor.socket_count(), 1);
    assert_eq!(reactor.registration_count(), 1);
    drop(results);
    assert_eq!(reactor.socket_count(), 0);
    assert_eq!(reactor.registration_count(), 0);
}

#[test]
fn shared_reactor_transfers_live_tcp_directions_without_changing_poll() {
    use std::{io::Read, thread};

    let (executor, group, waker) = target();
    let reactor = Reactor::new(2, 2).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nonblocking(true).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let source = Arc::new(IoSource::new(net::TcpStream::from_std(stream)));
    let event = ResourceEvent::new().unwrap();
    let read = source
        .register_with(
            &reactor,
            Interest::READABLE,
            Some(event.clone()),
            waker.clone(),
        )
        .unwrap();
    let kernel = lock(&source.0).token.unwrap();
    let write = {
        let source = source.clone();
        let reactor = reactor.clone();
        let waker = waker.clone();
        thread::spawn(move || {
            source
                .register_with(&reactor, Interest::WRITABLE, None, waker)
                .unwrap()
        })
        .join()
        .unwrap()
    };
    assert_eq!(lock(&source.0).token, Some(kernel));
    peer.write_all(b"r").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !(read.is_ready() && write.is_ready()) {
        assert!(
            std::time::Instant::now() < deadline,
            "TCP readiness timed out"
        );
        // The event collector runs on neither I/O owner's thread.
        let reactor = reactor.clone();
        thread::spawn(move || reactor.poll(Some(Duration::from_millis(50))).unwrap())
            .join()
            .unwrap();
    }
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    let stale_read = read.token;
    thread::scope(|scope| {
        let source = &source;
        scope.spawn(move || {
            let mut read = read;
            let mut byte = [0];
            assert_eq!(source.with_io(|s| s.read(&mut byte)).unwrap(), 1);
            assert_eq!(&byte, b"r");
            event.invalidate();
            read.stop().unwrap();
        });
        scope.spawn(move || {
            let mut write = write;
            assert_eq!(source.with_io(|s| s.write(b"w")).unwrap(), 1);
            write.rearm(Interest::WRITABLE).unwrap();
            write.stop().unwrap();
        });
    });
    let mut byte = [0];
    peer.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"w");
    assert_eq!(reactor.registration_count(), 0);
    assert!(!reactor.notify(stale_read));
    // Deregistration permits a new operation on this Poll, not a new Poll.
    let other = Reactor::new(2, 2).unwrap();
    assert!(source
        .register_with(&other, Interest::READABLE, None, waker.clone())
        .is_err());
    let next = source
        .register_with(&reactor, Interest::READABLE, None, waker)
        .unwrap();
    assert_ne!(next.token, stale_read);
    assert!(!reactor.notify(stale_read));
    drop(next);
    executor.cancel_group(&group);
}

// Pause once after poll admission, before entering the kernel. Waiting for an
// explicit handshake never steals the driver mutex from the thread under test.
fn pause_next_poll(
    reactor: &Reactor,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered, polling) = std::sync::mpsc::channel();
    let (resume, paused) = std::sync::mpsc::channel();
    *lock(&reactor.0.before_poll) = Some(Box::new(move || {
        entered.send(()).unwrap();
        paused
            .recv_timeout(Duration::from_secs(5))
            .expect("poll was not resumed");
    }));
    (polling, resume)
}

#[test]
fn shared_reactor_close_interrupts_driver_and_retires_all_waiters() {
    use std::{sync::mpsc, thread, time::Instant};

    let (executor, group, waker) = target();
    let reactor = Reactor::new(2, 2).unwrap();
    let source = IoSource::new(FakeSource::default());
    let read = source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .unwrap();
    let write = source
        .register_with(&reactor, Interest::WRITABLE, None, waker.clone())
        .unwrap();
    let stale = read.token;
    let permit = reactor.reserve_socket().unwrap();
    let (polling, resume) = pause_next_poll(&reactor);
    let (finished, result) = mpsc::channel();
    let driver = {
        let reactor = reactor.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                // Spurious readiness and EINTR do not mean the driver has closed.
                let report = reactor.poll(Some(deadline.saturating_duration_since(Instant::now())));
                if report.is_err() || Instant::now() >= deadline {
                    finished.send(report).unwrap();
                    break;
                }
            }
        })
    };
    polling
        .recv_timeout(Duration::from_secs(5))
        .expect("driver did not reach poll");
    assert_eq!(
        reactor.poll(Some(Duration::ZERO)).unwrap_err().message(),
        "Readiness driver is already polling"
    );
    resume.send(()).unwrap();
    reactor.request_close();
    reactor.request_close();
    // Finish well before the OS poll's fallback timeout; merely seeing the
    // closed flag after that timeout would not prove close actually woke it.
    let result = result.recv_timeout(Duration::from_secs(5));
    if result.is_err() {
        let _ = reactor.0.signal.wake();
    }
    driver.join().unwrap();
    assert_eq!(
        result
            .expect("close did not interrupt poll")
            .unwrap_err()
            .message(),
        "Readiness driver is closed"
    );
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    assert!(!reactor.notify(stale));
    assert!(reactor.reserve_socket().is_err());
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker)
        .is_err());
    assert!(read.is_ready() && write.is_ready());
    assert!(read.rearm(Interest::READABLE).is_err());
    assert!(write.rearm(Interest::WRITABLE).is_err());
    // Logical close does not pretend that outstanding operations or socket
    // owners have already cleaned up. Both directions can stop on other threads.
    assert_eq!(reactor.registration_count(), 2);
    assert_eq!(reactor.socket_count(), 1);
    thread::scope(|scope| {
        scope.spawn(move || drop(read));
        scope.spawn(move || drop(write));
        scope.spawn(move || drop(permit));
    });
    assert_eq!(reactor.registration_count(), 0);
    assert_eq!(reactor.socket_count(), 0);
    assert!(lock(&source.0).token.is_none());
    assert_eq!(source.with_io(|s| s.stops), 1);
    assert!(lock(&reactor.0.table).routes.is_empty());
    executor.cancel_group(&group);
}

#[test]
fn shared_reactor_does_not_lose_an_edge_during_registration_publication() {
    use std::{sync::mpsc, thread, time::Instant};

    struct PausedSource {
        stream: net::TcpStream,
        registered: mpsc::Sender<()>,
        publish: mpsc::Receiver<()>,
    }
    impl Source for PausedSource {
        fn register(
            &mut self,
            registry: &Registry,
            token: Token,
            interest: Interest,
        ) -> io::Result<()> {
            registry.register(&mut self.stream, token, interest)?;
            self.registered.send(()).unwrap();
            self.publish.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        }
        fn reregister(
            &mut self,
            registry: &Registry,
            token: Token,
            interest: Interest,
        ) -> io::Result<()> {
            registry.reregister(&mut self.stream, token, interest)
        }
        fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
            registry.deregister(&mut self.stream)
        }
    }

    let (executor, group, waker) = target();
    let reactor = Reactor::new(1, 1).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    stream.set_nonblocking(true).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.write_all(b"ready before register").unwrap();
    let (registered, pending) = mpsc::channel();
    let (publish, resume) = mpsc::channel();
    let source = IoSource::new(PausedSource {
        stream: net::TcpStream::from_std(stream),
        registered,
        publish: resume,
    });
    let (polling, resume_poll) = pause_next_poll(&reactor);
    let (finished, result) = mpsc::channel();
    let driver = {
        let reactor = reactor.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let report = reactor
                    .poll(Some(deadline.saturating_duration_since(Instant::now())))
                    .unwrap();
                if report.notified != 0 || Instant::now() >= deadline {
                    finished.send(report).unwrap();
                    break;
                }
            }
        })
    };
    polling
        .recv_timeout(Duration::from_secs(5))
        .expect("driver did not reach poll");
    resume_poll.send(()).unwrap();
    let registering = {
        let reactor = reactor.clone();
        thread::spawn(move || {
            source
                .register_with(&reactor, Interest::READABLE, None, waker)
                .unwrap()
        })
    };
    pending.recv_timeout(Duration::from_secs(5)).unwrap();
    // The kernel source is live, but its route/waiter is not yet published.
    // Event dispatch must serialize behind that publication.
    assert!(matches!(
        reactor.0.table.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    publish.send(()).unwrap();
    let registration = registering.join().unwrap();
    let result = result.recv_timeout(Duration::from_secs(5));
    if result.is_err() {
        reactor.request_close();
    }
    driver.join().unwrap();
    assert_eq!(
        result
            .expect("driver did not dispatch published readiness")
            .notified,
        1
    );
    assert!(registration.is_ready());
    assert_eq!(executor.poll(1).unwrap().polled, 1);
    drop(registration);
    assert_eq!(reactor.registration_count(), 0);
    executor.cancel_group(&group);
}
