use super::*;
use crate::runtime::{
    cps::CpsFrame,
    executor::{current_waker, HostExecutor, TaskGroup},
    suspension::Continuation,
};
use std::{cell::RefCell, io::Write, net::TcpListener};

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
    reactor.0.next.set(usize::MAX);
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
    let kernel = source.0.borrow().token.unwrap();
    let mut write = source
        .register_with(&reactor, Interest::WRITABLE, None, waker.clone())
        .unwrap();
    assert_eq!(source.0.borrow().token, Some(kernel));
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
    assert!(reactor.0.routes.borrow()[&kernel][0].is_none());
    let next_read = source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .unwrap();
    assert_ne!(read.token, next_read.token);
    assert!(!reactor.notify(read.token));
    assert!(write.is_ready());
    drop(next_read);
    write.stop().unwrap();
    assert!(reactor.0.routes.borrow().is_empty());
    let next = source
        .register_with(&reactor, Interest::READABLE, None, waker)
        .unwrap();
    assert_ne!(kernel, source.0.borrow().token.unwrap());
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
    assert!(!source.0.borrow().poisoned);
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
        assert!(source.0.borrow().poisoned);
        assert!(reactor.0.routes.borrow().is_empty());
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
    let stale = reactor.0.next.get();
    assert!(source
        .register_with(&reactor, Interest::READABLE, None, waker.clone())
        .is_err());
    assert!(source.0.borrow().poisoned);
    assert!(reactor.0.routes.borrow().is_empty());
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
