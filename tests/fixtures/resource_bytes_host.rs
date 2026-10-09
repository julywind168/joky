use joky::runtime::{
    byte_request::{ByteIoKind, ByteRequest, ResourceIoV1},
    cps::CpsFrame,
    resource::{ResourceEvent, ResourceFactoryV1},
};
use std::{
    cell::RefCell,
    ffi::c_void,
    io::{Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
};
struct Socket {
    stream: TcpStream,
    peer: TcpStream,
}
thread_local! {
    static LIVE: RefCell<i32> = const {RefCell::new(0)};
    static EVENTS: RefCell<Vec<ResourceEvent>> = const {RefCell::new(Vec::new())};
    static STOPS: RefCell<Vec<u32>> = const {RefCell::new(Vec::new())};
    static SENT: RefCell<Vec<u8>> = const {RefCell::new(Vec::new())};
}
unsafe extern "C" fn create(_: u64, output: *mut *mut c_void) -> i32 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.write_all(&[0, 255, 42, 128]).unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    // Start only once loopback input is available; scheduler readiness is NET1b.
    assert_eq!(stream.peek(&mut [0; 4]).unwrap(), 4);
    stream.set_nonblocking(true).unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    *output = Box::into_raw(Box::new(Socket { stream, peer })).cast();
    LIVE.with(|n| *n.borrow_mut() += 1);
    0
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Socket>()));
    LIVE.with(|n| *n.borrow_mut() -= 1);
}
#[export_name = "__joky_resource_factory_v1_byte_make"]
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
    let request = &mut *request;
    if (*frame).host_context.is_null() {
        let event = request.event();
        assert!(event.is_active());
        EVENTS.with(|events| events.borrow_mut().push(event.clone()));
        (*frame).host_context = Box::into_raw(Box::new(event)).cast();
        return 1;
    }
    let socket = &mut *request.resource().cast::<Socket>();
    if request.bytes().len() == 7 {
        *count = 8;
        return 0;
    }
    if request.kind() == ByteIoKind::Write && request.bytes().len() == 6 {
        return 0;
    }
    let result = match request.kind() {
        ByteIoKind::Read => socket.stream.read(request.bytes_mut()),
        ByteIoKind::Write => socket
            .stream
            .write(&request.bytes()[..request.bytes().len().min(2)])
            .inspect(|&n| {
                let mut received = vec![0; n];
                socket.peer.read_exact(&mut received).unwrap();
                SENT.with(|sent| sent.borrow_mut().extend_from_slice(&received));
            }),
    };
    match result {
        Ok(n) => {
            *count = n;
            0
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 1,
        Err(_) => 7,
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, request: *mut ByteRequest, reason: u32) {
    let event = Box::from_raw((*frame).host_context.cast::<ResourceEvent>());
    assert!(!event.is_active());
    assert!(!(*request).event().is_active());
    assert!((*(*request).resource().cast::<Socket>())
        .stream
        .peer_addr()
        .is_ok());
    STOPS.with(|stops| stops.borrow_mut().push(reason));
    (*frame).host_context = std::ptr::null_mut();
}
#[export_name = "__joky_resource_io_v1_read"]
pub static READ: ResourceIoV1 = ResourceIoV1 {
    abi_version: 1,
    poll: Some(poll),
    stop: Some(stop),
};
#[export_name = "__joky_resource_io_v1_write"]
pub static WRITE: ResourceIoV1 = READ;
pub fn reset() {
    LIVE.with(|n| assert_eq!(*n.borrow(), 0));
    EVENTS.with(|events| events.borrow_mut().clear());
    STOPS.with(|stops| stops.borrow_mut().clear());
    SENT.with(|sent| sent.borrow_mut().clear());
}
pub fn check() {
    LIVE.with(|n| assert_eq!(*n.borrow(), 0));
    EVENTS.with(|events| {
        let events = events.borrow();
        assert!(events.iter().all(|event| !event.is_active()));
        for (i, event) in events.iter().enumerate() {
            assert!(events[..i]
                .iter()
                .all(|old| old.identity() != event.identity()));
        }
        STOPS.with(|stops| assert_eq!(stops.borrow().len(), events.len()));
    });
}
pub fn sent() -> Vec<u8> {
    SENT.with(|sent| sent.borrow().clone())
}
