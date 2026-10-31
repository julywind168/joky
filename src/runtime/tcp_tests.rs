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
    poll_probe(frame, arena, op)
}
unsafe extern "C" fn duplex_probe(frame: *mut CpsFrame) -> i32 {
    let arena = CONTEXT.with(|c| c.borrow().unwrap().0);
    let op = match (*frame).slots.last().copied().unwrap() {
        12 => Operation::ReadHalf,
        13 => Operation::WriteHalf,
        _ => unreachable!(),
    };
    poll_probe(frame, arena, op)
}
unsafe fn poll_probe(frame: *mut CpsFrame, arena: *mut Arena, op: Operation) -> i32 {
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
    let budget = std::sync::Arc::new(super::super::arena::Budget {
        limit: (8192).into(),
        ..Default::default()
    });
    let executor = HostExecutor::default();
    let mut arena = Arena::with_budget(budget.clone(), None);
    let (stream, mut peer) = pair();
    let resource = adopt_stream(&mut arena, IoSource::new(stream), None).unwrap();
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
// The request must borrow the original owner across bounded writes. A payload
// snapshot would exceed this budget; cancellation must refund the request alone.
#[test]
fn tcp_write_all_borrows_pending_input_and_refunds_on_completion_or_cancel() {
    use std::time::{Duration, Instant};
    for cancel in [false, true] {
        let budget = std::sync::Arc::new(super::super::arena::Budget {
            limit: super::super::arena::MAX_BYTES.into(),
            ..Default::default()
        });
        let executor = HostExecutor::default();
        let mut arena = Arena::with_budget(budget.clone(), None);
        let (stream, mut peer) = pair();
        let resource = adopt_stream(&mut arena, IoSource::new(stream), None).unwrap();
        let data: Vec<_> = (0..3 * super::super::buffer::BYTE_CHUNK + 17)
            .map(|index| index as u8)
            .collect();
        let input = Buffer::copy_bytes(&mut arena, &data).unwrap();
        let baseline = arena.bytes();
        budget.limit.set(baseline + std::mem::size_of::<Request>());
        let mut code = 99u64;
        let mut frame = CpsFrame::new(3);
        frame.slots = vec![
            resource.as_ptr() as usize as u64,
            input.as_ptr() as usize as u64,
            &mut code as *mut u64 as usize as u64,
        ];
        CONTEXT.with(|c| *c.borrow_mut() = Some((&mut arena, Operation::WriteAll)));
        let group = TaskGroup::new();
        executor
            .spawn(&group, unsafe {
                Continuation::from_native(probe, frame, Rc::new(()))
            })
            .unwrap();
        let first = executor.poll(1).unwrap();
        assert!(first.completed.is_empty());
        assert_eq!(arena.bytes(), baseline + std::mem::size_of::<Request>());
        assert_eq!(code, 99);
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let receiver = std::thread::spawn(move || {
            let mut received = Vec::new();
            peer.read_to_end(&mut received).unwrap();
            received
        });
        if cancel {
            executor.cancel_group(&group);
            assert_eq!(code, 99);
        } else {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "write_all failed to resume");
                let report = executor.poll(1).unwrap();
                if let Some(done) = report.completed.into_iter().next() {
                    assert_eq!(done.result.unwrap(), data.len() as u64);
                    break;
                }
                if !report.ready {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            assert_eq!(code, 0);
        }
        assert_eq!(arena.bytes(), baseline);
        REACTOR.with(|r| assert_eq!(r.borrow().as_ref().unwrap().registration_count(), 0));
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        // The language owner remains valid after the request descriptor is gone.
        assert_eq!(unsafe { input.as_ref().bytes().unwrap() }, data);
        unsafe {
            Buffer::release(&mut arena, resource.as_ptr()).unwrap();
            Buffer::release(&mut arena, input.as_ptr()).unwrap();
        }
        assert_eq!(budget.bytes.get(), 0);
        let received = receiver.join().unwrap();
        if cancel {
            assert!(received.len() <= super::super::buffer::BYTE_CHUNK);
            assert_eq!(received, data[..received.len()]);
        } else {
            assert_eq!(received, data);
        }
        CONTEXT.with(|c| c.borrow_mut().take());
    }
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
    let resource = adopt_stream(&mut arena, IoSource::new(stream), None).unwrap();
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
        std::sync::Arc::new(super::super::arena::Budget {
            limit: (0).into(),
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
    let resource = adopt_stream(&mut arena, IoSource::new(stream), None).unwrap();
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

fn split(executor: &HostExecutor, arena: &mut Arena, stream: NonNull<Buffer>) -> (u64, u64) {
    let owner = stream.as_ptr() as usize as u64;
    let read = run(executor, arena, Operation::SplitReader, vec![owner]).unwrap();
    let write = run(executor, arena, Operation::SplitWriter, vec![owner]).unwrap();
    unsafe {
        Buffer::release(arena, stream.as_ptr()).unwrap();
    }
    (read, write)
}
fn spawn_direction(
    executor: &HostExecutor,
    arena: &mut Arena,
    op: Operation,
    mut slots: Vec<u64>,
) -> (TaskGroup, super::super::executor::TaskId) {
    CONTEXT.with(|c| *c.borrow_mut() = Some((arena, op)));
    slots.push(op as u64);
    let mut frame = CpsFrame::new(slots.len());
    frame.slots = slots;
    let group = TaskGroup::new();
    let id = executor
        .spawn(&group, unsafe {
            Continuation::from_native(duplex_probe, frame, Rc::new(()))
        })
        .unwrap();
    (group, id)
}
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net5b_tcp_two_pending_directions_cancel_independently_and_preserve_the_peer() {
    use std::time::{Duration, Instant};
    for cancel_read in [false, true] {
        let executor = HostExecutor::default();
        let mut arena = Arena::default();
        let (mut stream, mut peer) = pair();
        #[cfg(target_os = "macos")]
        let (send_buffer, receive_buffer) = (0x1001, 0x1002);
        #[cfg(target_os = "linux")]
        let (send_buffer, receive_buffer) = (7, 8);
        socket_option(&stream, send_buffer, &[4096]);
        socket_option(&peer, receive_buffer, &[4096]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut sent = 0;
        loop {
            match stream.write(&[42; 4096]) {
                Ok(count) => sent += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                other => panic!("unexpected write: {other:?}"),
            }
            assert!(Instant::now() < deadline);
        }
        let stream = adopt_stream(&mut arena, IoSource::new(stream), None).unwrap();
        let (reader, writer) = split(&executor, &mut arena, stream);
        let input = Buffer::copy_bytes(&mut arena, &[42; 4096]).unwrap();
        let baseline = arena.bytes();
        let mut read_code = 99u64;
        let mut write_code: u64;
        let read_slots = vec![reader, 1, &mut read_code as *mut u64 as usize as u64];
        let (mut read_group, mut read_id) = spawn_direction(
            &executor,
            &mut arena,
            Operation::ReadHalf,
            read_slots.clone(),
        );
        assert!(executor.poll(1).unwrap().completed.is_empty());
        let reactor = REACTOR.with(|r| r.borrow().clone().unwrap());
        assert_eq!(reactor.registration_count(), 1);
        let (write_group, write_id) = loop {
            write_code = 99;
            let (group, id) = spawn_direction(
                &executor,
                &mut arena,
                Operation::WriteHalf,
                vec![
                    writer,
                    input.as_ptr() as usize as u64,
                    &mut write_code as *mut u64 as usize as u64,
                ],
            );
            let report = executor.poll(1).unwrap();
            if report.completed.is_empty() {
                break (group, id);
            }
            sent += report.completed.into_iter().next().unwrap().result.unwrap() as usize;
            assert!(Instant::now() < deadline && sent < 64 * 1024 * 1024);
        };
        assert_eq!(reactor.registration_count(), 2);
        assert_eq!(read_code, 99);
        if cancel_read {
            executor.cancel_group(&read_group);
            assert_eq!(reactor.registration_count(), 1);
            assert_eq!(write_code, 99);
            executor.cancel_group(&write_group);
            assert_eq!(reactor.registration_count(), 0);
            (read_group, read_id) =
                spawn_direction(&executor, &mut arena, Operation::ReadHalf, read_slots);
            assert!(executor.poll(1).unwrap().completed.is_empty());
            assert_eq!(reactor.registration_count(), 1);
        } else {
            executor.cancel_group(&write_group);
            assert_eq!(reactor.registration_count(), 1);
            assert_eq!(write_code, 99);
        }
        peer.write_all(&[255]).unwrap();
        let output = loop {
            let report = executor.poll(1).unwrap();
            let mut output = None;
            for done in report.completed {
                if done.id == read_id {
                    output = Some(done.result.unwrap() as usize as *mut Buffer);
                } else {
                    assert_eq!(done.id, write_id);
                    sent += done.result.unwrap() as usize;
                }
            }
            if let Some(output) = output {
                break output;
            }
            assert!(Instant::now() < deadline);
        };
        assert_eq!(read_code, 0);
        executor.cancel_group(&read_group);
        assert_eq!(unsafe { (&*output).bytes().unwrap() }, &[255]);
        assert_eq!(write_code, 99);
        assert_eq!(reactor.registration_count(), 0);
        unsafe {
            Buffer::release(&mut arena, output).unwrap();
        }
        assert_eq!(arena.bytes(), baseline);
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut prefix = vec![0; sent];
        peer.read_exact(&mut prefix).unwrap();
        assert!(prefix.iter().all(|b| *b == 42));
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        let count = run(
            &executor,
            &mut arena,
            Operation::WriteHalf,
            vec![
                writer,
                input.as_ptr() as usize as u64,
                &mut write_code as *mut u64 as usize as u64,
            ],
        )
        .unwrap() as usize;
        assert_eq!(write_code, 0);
        assert!(count > 0 && count <= 4096);
        let mut tail = vec![0; count];
        peer.read_exact(&mut tail).unwrap();
        assert!(tail.iter().all(|b| *b == 42));
        unsafe {
            for resource in [reader, writer, input.as_ptr() as usize as u64] {
                Buffer::release(&mut arena, resource as usize as *mut Buffer).unwrap();
            }
        }
        assert_eq!(arena.bytes(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn net5b_tcp_split_publication_failures_refund_owners_and_keep_one_socket_permit() {
    let executor = HostExecutor::default();
    let mut context = Arena::default();
    let mut code = 99u64;
    let listener = run(
        &executor,
        &mut context,
        Operation::Bind,
        vec![0, 0x7f000001, 4, &mut code as *mut u64 as usize as u64],
    )
    .unwrap();
    let reactor = REACTOR.with(|r| r.borrow().clone().unwrap());
    run(
        &executor,
        &mut context,
        Operation::CloseListener,
        vec![listener],
    )
    .unwrap();
    unsafe {
        Buffer::release(&mut context, listener as usize as *mut Buffer).unwrap();
    }
    for stage in [Operation::SplitReader, Operation::SplitWriter] {
        let mut failures = 0;
        let mut successes = 0;
        for allowance in (0..1024).step_by(8) {
            let budget = std::sync::Arc::new(super::super::arena::Budget {
                limit: (8192).into(),
                ..Default::default()
            });
            let mut arena = Arena::with_budget(budget.clone(), None);
            let (stream, _peer) = pair();
            let owner = adopt_stream(
                &mut arena,
                IoSource::new(stream),
                Some(reactor.reserve_socket().unwrap()),
            )
            .unwrap();
            let mut owners = vec![owner.as_ptr() as usize as u64];
            if stage == Operation::SplitWriter {
                owners.push(
                    run(
                        &executor,
                        &mut arena,
                        Operation::SplitReader,
                        vec![owners[0]],
                    )
                    .unwrap(),
                );
            }
            budget.limit.set(arena.bytes() + allowance);
            match run(&executor, &mut arena, stage, vec![owners[0]]) {
                Ok(half) => {
                    owners.push(half);
                    successes += 1;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit);
                    failures += 1;
                }
            }
            assert_eq!(reactor.socket_count(), 1);
            assert_eq!(reactor.registration_count(), 0);
            unsafe {
                let length = owners.len();
                for (index, owner) in owners.into_iter().enumerate() {
                    Buffer::release(&mut arena, owner as usize as *mut Buffer).unwrap();
                    assert_eq!(reactor.socket_count(), usize::from(index + 1 < length));
                }
            }
            assert_eq!(reactor.socket_count(), 0);
            assert_eq!(budget.bytes.get(), 0);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        assert!(failures > 0 && successes > 0);
    }
}

#[test]
fn parallel_tcp_packets_require_builtin_shared_quiescent_owners() {
    use crate::runtime::parallel::{OwnedBuffer, Storage, TaskBudget};
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Connection>();
    let budget = TaskBudget::new(1 << 20);
    let root = Storage::new(vec![budget.clone()]).unwrap();
    let mut arena = Arena::with_storage(root.clone());
    let owner = adopt(&mut arena, Socket::Closed, None).unwrap();
    let lease = unsafe { (*owner.as_ptr()).resource_mut().unwrap().lease().unwrap() };
    assert!(unsafe { OwnedBuffer::new(&arena, owner) }.is_err());
    drop(lease);
    let packet = unsafe { OwnedBuffer::new(&arena, owner) }.unwrap();
    drop(arena);
    std::thread::spawn(move || drop(packet)).join().unwrap();

    let mut arena = Arena::with_storage(root.clone());
    let mut local = Arena::default();
    let charge = OwnerCharge::new(&mut local, std::mem::size_of::<Owner>()).unwrap();
    LIVE.with(|count| count.set(count.get() + 1));
    let token = NonNull::new(
        Box::into_raw(Box::new(Owner {
            socket: Socket::Closed,
            permit: None,
            charge,
            local: true,
        }))
        .cast(),
    )
    .unwrap();
    let local_resource = OwnedResource::from_token(token, release).unwrap();
    let owner = Buffer::external(&mut arena, local_resource).unwrap();
    // A shared header must not launder a local refund handle into a packet.
    assert!(unsafe { OwnedBuffer::new(&arena, owner) }.is_err());
    unsafe {
        Buffer::release(&mut arena, owner.as_ptr()).unwrap();
    }
    assert_eq!(local.bytes(), 0);

    unsafe extern "C" fn foreign_release(token: *mut c_void) {
        drop(Box::from_raw(token.cast::<u8>()));
    }
    let resource = OwnedResource::from_token(
        NonNull::new(Box::into_raw(Box::new(7u8)).cast()).unwrap(),
        foreign_release,
    )
    .unwrap();
    let owner = Buffer::external(&mut arena, resource).unwrap();
    assert!(unsafe { OwnedBuffer::new(&arena, owner) }.is_err());
    unsafe {
        Buffer::release(&mut arena, owner.as_ptr()).unwrap();
    }
    drop((arena, root));
    assert_eq!(budget.used(), 0);
}
