use super::*;
use crate::runtime::{
    executor::{current_reactor, HostExecutor, TaskGroup},
    suspension::Continuation,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
thread_local! {
    static CONTEXT: RefCell<Option<(*mut Arena, Operation)>> = const { RefCell::new(None) };
    static REACTOR: RefCell<Option<super::super::reactor::Reactor>> = const { RefCell::new(None) };
    static REGISTRATIONS: Cell<usize> = const { Cell::new(0) };
}
unsafe extern "C" fn probe(frame: *mut CpsFrame) -> i32 {
    let (arena, op) = CONTEXT.with(|c| c.borrow().unwrap());
    let output = poll(&mut *arena, &mut *frame, op);
    REACTOR.with(|r| *r.borrow_mut() = Some(current_reactor().unwrap()));
    REGISTRATIONS.with(|count| count.set(current_reactor().unwrap().registration_count()));
    match output {
        Ok(Some(Output::Scalar(count))) => {
            (*frame).result = count;
            0
        }
        Ok(Some(Output::Owner(buffer))) => {
            (*frame).result = buffer.as_ptr() as usize as u64;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::super::error::record_error(error);
            super::super::cps::RUNTIME_ERROR
        }
    }
}
fn run(
    executor: &HostExecutor,
    arena: &mut Arena,
    op: Operation,
    slots: Vec<u64>,
) -> Result<u64, Error> {
    CONTEXT.with(|c| *c.borrow_mut() = Some((arena, op)));
    let mut frame = CpsFrame::new(slots.len());
    frame.slots = slots;
    let task = unsafe { Continuation::from_native(probe, frame, Rc::new(())) };
    let group = TaskGroup::new();
    executor.spawn(&group, task).unwrap();
    let result = loop {
        let report = executor.poll(1).unwrap();
        if let Some(done) = report.completed.into_iter().next() {
            break done.result;
        }
        if !report.ready {
            executor.wait().unwrap();
        }
    };
    CONTEXT.with(|c| c.borrow_mut().take());
    result
}
fn pair() -> (TcpStream, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    (TcpStream::from_std(client), peer)
}
#[test]
fn net2_tcp_read_copy_peak_failure_stops_and_refunds_the_request() {
    let budget = Rc::new(super::super::arena::Budget {
        limit: Cell::new(8192),
        ..Default::default()
    });
    let executor = HostExecutor::default();
    let mut arena = Arena::with_budget(budget.clone(), None);
    let (stream, mut peer) = pair();
    let resource = adopt(&mut arena, Socket::Stream(IoSource::new(stream)), None).unwrap();
    let base = arena.bytes();
    peer.write_all(&[0, 128, 255, 42]).unwrap();
    // Permit the request snapshot, but reject the simultaneous language copy.
    budget
        .limit
        .set(base + std::mem::size_of::<Request>() + 4096);
    let mut code = 99u64;
    let slots = vec![
        resource.as_ptr() as usize as u64,
        4096,
        &mut code as *mut u64 as usize as u64,
    ];
    assert_eq!(
        run(&executor, &mut arena, Operation::Read, slots.clone())
            .unwrap_err()
            .code(),
        ErrorCode::ResourceLimit
    );
    assert_eq!(code, 99); // A runtime failure never publishes a business success.
    assert_eq!(arena.bytes(), base);
    assert_eq!(budget.bytes.get(), base);
    REGISTRATIONS.with(|count| assert_eq!(count.get(), 0));
    budget.limit.set(8192);
    peer.write_all(&[42]).unwrap();
    let bytes = run(&executor, &mut arena, Operation::Read, slots).unwrap() as usize as *mut Buffer;
    assert_eq!(code, 0);
    assert_eq!(unsafe { (&*bytes).bytes().unwrap() }, &[42]);
    unsafe {
        Buffer::release(&mut arena, bytes).unwrap();
        Buffer::release(&mut arena, resource.as_ptr()).unwrap();
    }
    assert_eq!(budget.bytes.get(), 0);
}
#[test]
fn net2_tcp_ipv6_scope_and_native_error_bits_roundtrip() {
    let endpoint = 6 | (443 << 8) | (42u64 << 32);
    let address = address(0xfe80000000000000, 1, endpoint).unwrap();
    assert_eq!(encode_address(address), (0xfe80000000000000, 1, endpoint));
    assert!(super::address(0, 0x7f000001, 4 | (42u64 << 32)).is_err());
    for (kind, code) in [
        (io::ErrorKind::ConnectionRefused, 1),
        (io::ErrorKind::ConnectionReset, 2),
        (io::ErrorKind::WriteZero, 11),
    ] {
        assert_eq!(error_code(&kind.into()), code);
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn socket_option(stream: &impl std::os::fd::AsRawFd, option: i32, value: &[i32]) {
    unsafe extern "C" {
        fn setsockopt(fd: i32, level: i32, option: i32, value: *const c_void, length: u32) -> i32;
    }
    #[cfg(target_os = "macos")]
    const SOL_SOCKET: i32 = 0xffff;
    #[cfg(target_os = "linux")]
    const SOL_SOCKET: i32 = 1;
    // Only test configuration uses the fd; production I/O always goes through Mio.
    assert_eq!(
        unsafe {
            setsockopt(
                stream.as_raw_fd(),
                SOL_SOCKET,
                option,
                value.as_ptr().cast(),
                std::mem::size_of_val(value) as u32,
            )
        },
        0
    );
}
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net2_tcp_partial_write_reports_the_real_prefix_and_reset_is_an_error() {
    let executor = HostExecutor::default();
    let mut arena = Arena::default();
    let (stream, mut peer) = pair();
    #[cfg(target_os = "macos")]
    const SO_SNDBUF: i32 = 0x1001;
    #[cfg(target_os = "linux")]
    const SO_SNDBUF: i32 = 7;
    socket_option(&stream, SO_SNDBUF, &[4096]);
    let resource = adopt(&mut arena, Socket::Stream(IoSource::new(stream)), None).unwrap();
    let data: Vec<_> = (0..super::super::byte_request::MAX_REQUEST_BYTES)
        .map(|i| i as u8)
        .collect();
    let input = Buffer::copy_bytes(&mut arena, &data).unwrap();
    let mut code = 99u64;
    let slots = vec![
        resource.as_ptr() as usize as u64,
        input.as_ptr() as usize as u64,
        &mut code as *mut u64 as usize as u64,
    ];
    let count = run(&executor, &mut arena, Operation::Write, slots).unwrap() as usize;
    assert_eq!(code, 0);
    assert!(count > 0 && count < data.len());
    let mut prefix = vec![0; count];
    peer.read_exact(&mut prefix).unwrap();
    assert_eq!(prefix, data[..count]);
    #[cfg(target_os = "macos")]
    const SO_LINGER: i32 = 0x80;
    #[cfg(target_os = "linux")]
    const SO_LINGER: i32 = 13;
    socket_option(&peer, SO_LINGER, &[1, 0]);
    drop(peer); // An abortive close sends a real reset.
    let slots = vec![
        resource.as_ptr() as usize as u64,
        16,
        &mut code as *mut u64 as usize as u64,
    ];
    let bytes = run(&executor, &mut arena, Operation::Read, slots).unwrap() as usize as *mut Buffer;
    assert_eq!(code & 0xffffffff, 2);
    assert!(unsafe { (&*bytes).bytes().unwrap().is_empty() });
    unsafe {
        Buffer::release(&mut arena, bytes).unwrap();
        Buffer::release(&mut arena, input.as_ptr()).unwrap();
        Buffer::release(&mut arena, resource.as_ptr()).unwrap();
    }
    assert_eq!(arena.bytes(), 0);
}

#[test]
fn net3_tcp_socket_admission_counts_idle_owners_and_refunds_failures() {
    let executor = HostExecutor::default();
    let mut arena = Arena::default();
    let mut code = 99u64;
    let slots = vec![0, 0x7f000001, 4, &mut code as *mut u64 as usize as u64];
    let first = run(&executor, &mut arena, Operation::Bind, slots.clone()).unwrap();
    let reactor = REACTOR.with(|r| r.borrow().clone().unwrap());
    assert_eq!(reactor.socket_count(), 1);
    assert_eq!(reactor.registration_count(), 0);
    let mut permits = Vec::new();
    while let Ok(permit) = reactor.reserve_socket() {
        permits.push(permit);
    }
    let baseline = arena.bytes();
    assert_eq!(
        run(&executor, &mut arena, Operation::Bind, slots.clone())
            .unwrap_err()
            .code(),
        ErrorCode::ResourceLimit
    );
    assert_eq!(arena.bytes(), baseline);
    permits.pop();
    let second = run(&executor, &mut arena, Operation::Bind, slots).unwrap();
    drop(permits);
    assert_eq!(reactor.socket_count(), 2);
    run(&executor, &mut arena, Operation::CloseListener, vec![first]).unwrap();
    assert_eq!(reactor.socket_count(), 1);
    // Pending acceptance reserves a future socket; cancellation refunds it.
    CONTEXT.with(|c| *c.borrow_mut() = Some((&mut arena, Operation::Accept)));
    let mut frame = CpsFrame::new(2);
    frame.slots = vec![second, &mut code as *mut u64 as usize as u64];
    let group = TaskGroup::new();
    executor
        .spawn(&group, unsafe {
            Continuation::from_native(probe, frame, Rc::new(()))
        })
        .unwrap();
    assert!(executor.poll(1).unwrap().completed.is_empty());
    assert_eq!(reactor.socket_count(), 2);
    executor.cancel_group(&group);
    assert_eq!(reactor.socket_count(), 1);
    assert_eq!(reactor.registration_count(), 0);
    CONTEXT.with(|c| c.borrow_mut().take());
    // Failed owner publication must refund admission as well as close the fd.
    let mut empty = Arena::with_budget(
        Rc::new(super::super::arena::Budget {
            limit: Cell::new(0),
            ..Default::default()
        }),
        None,
    );
    let socket = Socket::Listener(IoSource::new(
        TcpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap(),
    ));
    let permit = reactor.reserve_socket().unwrap();
    assert_eq!(
        adopt(&mut empty, socket, Some(permit)).unwrap_err().code(),
        ErrorCode::ResourceLimit
    );
    assert_eq!(reactor.socket_count(), 1);
    unsafe {
        Buffer::release(&mut arena, first as usize as *mut Buffer).unwrap();
        Buffer::release(&mut arena, second as usize as *mut Buffer).unwrap();
    }
    assert_eq!(reactor.socket_count(), 0);
    assert_eq!(arena.bytes(), 0);
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net3_tcp_backpressure_cancel_stops_registration_and_preserves_prefix() {
    use std::time::{Duration, Instant};
    let executor = HostExecutor::default();
    let mut arena = Arena::default();
    let (mut stream, mut peer) = pair();
    #[cfg(target_os = "macos")]
    const SO_SNDBUF: i32 = 0x1001;
    #[cfg(target_os = "linux")]
    const SO_SNDBUF: i32 = 7;
    socket_option(&stream, SO_SNDBUF, &[4096]);
    // Fill the kernel send buffer to WouldBlock without consuming the peer.
    let mut sent = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stream.write(&[42; 4096]) {
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected write: {other:?}"),
        }
        assert!(Instant::now() < deadline);
    }
    let resource = adopt(&mut arena, Socket::Stream(IoSource::new(stream)), None).unwrap();
    let input = Buffer::copy_bytes(&mut arena, &[42; 4096]).unwrap();
    let baseline = arena.bytes();
    let mut code = 99u64;
    CONTEXT.with(|c| *c.borrow_mut() = Some((&mut arena, Operation::Write)));
    // ACKs can free send space between the raw fill and registration. Keep
    // accounting for successful prefixes until an actual host request is pending.
    let group = loop {
        let mut frame = CpsFrame::new(3);
        frame.slots = vec![
            resource.as_ptr() as usize as u64,
            input.as_ptr() as usize as u64,
            &mut code as *mut u64 as usize as u64,
        ];
        let group = TaskGroup::new();
        code = 99;
        executor
            .spawn(&group, unsafe {
                Continuation::from_native(probe, frame, Rc::new(()))
            })
            .unwrap();
        let report = executor.poll(1).unwrap();
        if report.completed.is_empty() {
            break group;
        }
        let count = report.completed.into_iter().next().unwrap().result.unwrap() as usize;
        assert!(count > 0);
        sent += count;
        assert!(sent < 64 * 1024 * 1024 && Instant::now() < deadline);
    };
    assert!(arena.bytes() > baseline);
    let reactor = REACTOR.with(|r| r.borrow().clone().unwrap());
    assert_eq!(reactor.registration_count(), 1);
    executor.cancel_group(&group);
    assert_eq!(arena.bytes(), baseline);
    assert_eq!(reactor.registration_count(), 0);
    assert_eq!(code, 99);
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut prefix = vec![0; sent];
    peer.read_exact(&mut prefix).unwrap();
    assert!(prefix.iter().all(|&b| b == 42));
    // New writability after cancellation cannot revive the cancelled root.
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    let slots = vec![
        resource.as_ptr() as usize as u64,
        input.as_ptr() as usize as u64,
        &mut code as *mut u64 as usize as u64,
    ];
    let count = run(&executor, &mut arena, Operation::Write, slots).unwrap() as usize;
    assert!(count > 0 && count <= 4096);
    let mut tail = vec![0; count];
    peer.read_exact(&mut tail).unwrap();
    assert!(tail.iter().all(|&b| b == 42));
    unsafe {
        Buffer::release(&mut arena, resource.as_ptr()).unwrap();
        Buffer::release(&mut arena, input.as_ptr()).unwrap();
    }
    assert_eq!(arena.bytes(), 0);
}
