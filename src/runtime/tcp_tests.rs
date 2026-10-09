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
    static REGISTRATIONS: Cell<usize> = const { Cell::new(0) };
}
unsafe extern "C" fn probe(frame: *mut CpsFrame) -> i32 {
    let (arena, op) = CONTEXT.with(|c| c.borrow().unwrap());
    let output = poll(&mut *arena, &mut *frame, op);
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
    let resource = adopt(&mut arena, Socket::Stream(IoSource::new(stream))).unwrap();
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
    let resource = adopt(&mut arena, Socket::Stream(IoSource::new(stream))).unwrap();
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
