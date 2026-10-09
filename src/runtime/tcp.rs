//! Compiler-checked TCP operations. Only bounded host storage crosses Pending.
use super::{
    arena::{Arena, ExternalCharge},
    borrowed_host::{STOP_COMPLETED, STOP_FAILED},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    reactor::{IoRegistration, IoSource},
    resource::{OwnedResource, ResourceEvent, ResourceLease},
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use mio::{
    net::{TcpListener, TcpStream},
    Interest,
};
use std::{
    ffi::c_void,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV6},
    ptr::NonNull,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    Bind,
    Connect,
    Accept,
    Read,
    Write,
    StreamAddr,
    ListenerAddr,
    Shutdown,
    CloseStream,
    CloseListener,
}
pub(crate) fn operation(name: &str) -> Option<Operation> {
    Some(match name {
        "__tcp_bind" => Operation::Bind,
        "__tcp_connect" => Operation::Connect,
        "__tcp_accept" => Operation::Accept,
        "__tcp_read" => Operation::Read,
        "__tcp_write" => Operation::Write,
        "__tcp_stream_addr" => Operation::StreamAddr,
        "__tcp_listener_addr" => Operation::ListenerAddr,
        "__tcp_shutdown" => Operation::Shutdown,
        "__tcp_close_stream" => Operation::CloseStream,
        "__tcp_close_listener" => Operation::CloseListener,
        _ => return None,
    })
}
impl Operation {
    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        let stream = Type::Resource("tcp.StreamOwner".into());
        let listener = Type::Resource("tcp.ListenerOwner".into());
        let reference = |ty| Type::reference(ty, true);
        let scalar = Type::UInt64;
        let error = reference(scalar.clone());
        let bytes = |ty: &Type| matches!(ty, Type::Record(r) if r.name == "joky/array.Array" && r.arguments == [Type::UInt8] && r.fields.len() == 1 && matches!(&r.fields[0].ty, Type::Buffer(element) if **element == Type::UInt8));
        let (params, ret) = match self {
            Self::Bind | Self::Connect => (
                vec![
                    scalar.clone(),
                    scalar.clone(),
                    scalar.clone(),
                    error.clone(),
                ],
                if self == Self::Bind {
                    listener.clone()
                } else {
                    stream.clone()
                },
            ),
            Self::Accept => (
                vec![reference(listener.clone()), error.clone()],
                stream.clone(),
            ),
            Self::Read => (
                vec![reference(stream.clone()), scalar.clone(), error.clone()],
                signature.ret.clone(),
            ),
            Self::Write => (
                vec![
                    reference(stream.clone()),
                    signature.params.get(1).cloned().unwrap_or(Type::Unit),
                    error.clone(),
                ],
                scalar.clone(),
            ),
            Self::StreamAddr => (
                vec![
                    reference(stream.clone()),
                    scalar.clone(),
                    error.clone(),
                    error.clone(),
                    error.clone(),
                ],
                scalar.clone(),
            ),
            Self::ListenerAddr => (
                vec![
                    reference(listener.clone()),
                    error.clone(),
                    error.clone(),
                    error.clone(),
                ],
                scalar.clone(),
            ),
            Self::Shutdown => (
                vec![reference(stream.clone()), scalar.clone()],
                scalar.clone(),
            ),
            Self::CloseStream => (vec![reference(stream.clone())], scalar.clone()),
            Self::CloseListener => (vec![reference(listener.clone())], scalar.clone()),
        };
        if !suspends
            || signature.params != params
            || signature.ret != ret
            || self == Self::Read && !bytes(&signature.ret)
            || self == Self::Write && !signature.params.get(1).is_some_and(bytes)
        {
            return Err(failure(
                "TCP operation requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}
pub(crate) fn validate_external_resource(name: &str) -> Result<(), Error> {
    if matches!(name, "tcp.StreamOwner" | "tcp.ListenerOwner") {
        return Err(failure(
            "Built-in TCP resources cannot use external factories",
        ));
    }
    Ok(())
}
fn failure(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::AbiError, message)
}
// Stable language kind in the low word, signed native OS code in the high word.
fn error_code(error: &io::Error) -> u64 {
    let kind = match error.kind() {
        io::ErrorKind::ConnectionRefused => 1,
        io::ErrorKind::ConnectionReset => 2,
        io::ErrorKind::ConnectionAborted => 3,
        io::ErrorKind::NotConnected => 4,
        io::ErrorKind::AddrInUse => 5,
        io::ErrorKind::AddrNotAvailable => 6,
        io::ErrorKind::BrokenPipe => 7,
        io::ErrorKind::PermissionDenied => 8,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => 9,
        io::ErrorKind::TimedOut => 10,
        io::ErrorKind::WriteZero => 11,
        _ => 12,
    };
    ((error.raw_os_error().unwrap_or(0) as u32 as u64) << 32) | kind
}
fn address(high: u64, low: u64, endpoint: u64) -> io::Result<SocketAddr> {
    if (endpoint >> 24) & 255 != 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let ip = match endpoint & 255 {
        4 if high == 0 && low <= u32::MAX as u64 && endpoint >> 32 == 0 => {
            IpAddr::V4(Ipv4Addr::from(low as u32))
        }
        6 => IpAddr::V6(Ipv6Addr::from(((high as u128) << 64) | low as u128)),
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    };
    Ok(match ip {
        IpAddr::V4(ip) => SocketAddr::new(IpAddr::V4(ip), (endpoint >> 8) as u16),
        IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(
            ip,
            (endpoint >> 8) as u16,
            0,
            (endpoint >> 32) as u32,
        )),
    })
}
fn encode_address(address: SocketAddr) -> (u64, u64, u64) {
    let (high, low, family) = match address.ip() {
        IpAddr::V4(ip) => (0, u32::from(ip) as u64, 4),
        IpAddr::V6(ip) => {
            let bits = u128::from(ip);
            ((bits >> 64) as u64, bits as u64, 6)
        }
    };
    let scope = match address {
        SocketAddr::V6(addr) => addr.scope_id() as u64,
        SocketAddr::V4(_) => 0,
    };
    (
        high,
        low,
        family | ((address.port() as u64) << 8) | (scope << 32),
    )
}
enum Socket {
    Stream(IoSource<TcpStream>),
    Listener(IoSource<TcpListener>),
    Closed,
}
struct Owner {
    socket: Socket,
    _charge: ExternalCharge,
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
fn adopt(arena: &mut Arena, socket: Socket) -> Result<NonNull<Buffer>, Error> {
    let source_size = match &socket {
        Socket::Stream(_) => IoSource::<TcpStream>::allocation_size(),
        Socket::Listener(_) => IoSource::<TcpListener>::allocation_size(),
        Socket::Closed => 0,
    };
    // Charge the owner and Rc source allocation. Kernel buffers and Mio's platform
    // internals are outside task payload accounting.
    let charge = arena.reserve_external(std::mem::size_of::<Owner>() + source_size)?;
    #[cfg(test)]
    LIVE.with(|count| count.set(count.get() + 1));
    let token = NonNull::new(
        Box::into_raw(Box::new(Owner {
            socket,
            _charge: charge,
        }))
        .cast(),
    )
    .unwrap();
    let resource = match OwnedResource::from_token(token, release) {
        Ok(resource) => resource,
        Err(error) => {
            unsafe {
                release(token.as_ptr());
            }
            return Err(error);
        }
    };
    Buffer::external(arena, resource)
}
enum Registration {
    Stream(IoRegistration<TcpStream>),
    Listener(IoRegistration<TcpListener>),
}
impl Registration {
    fn is_ready(&self) -> bool {
        match self {
            Self::Stream(r) => r.is_ready(),
            Self::Listener(r) => r.is_ready(),
        }
    }
    fn rearm(&self, interest: Interest) -> Result<(), Error> {
        match self {
            Self::Stream(r) => r.rearm(interest),
            Self::Listener(r) => r.rearm(interest),
        }
    }
    fn stop(&mut self) -> Result<(), Error> {
        match self {
            Self::Stream(r) => r.stop(),
            Self::Listener(r) => r.stop(),
        }
    }
}
pub(crate) struct Request {
    event: ResourceEvent,
    registration: Option<Registration>,
    connecting: Option<IoSource<TcpStream>>,
    lease: Option<ResourceLease>,
    bytes: Vec<u8>,
    _charge: ExternalCharge,
}
impl Request {
    fn stop_io(&mut self) -> Result<(), Error> {
        self.event.invalidate();
        if let Some(mut registration) = self.registration.take() {
            registration.stop()?;
        }
        Ok(())
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.event.invalidate();
        // Registration owns the source until deregistration completes.
        self.registration.take();
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    (*frame).tcp_request.take();
}
pub(crate) enum Output {
    Owner(NonNull<Buffer>),
    Scalar(u64),
}
unsafe fn owner(request: &mut Request) -> &mut Owner {
    &mut *request.lease.as_ref().unwrap().token().cast::<Owner>()
}
unsafe fn set_error(frame: &CpsFrame, op: Operation, code: u64) {
    let index = match op {
        Operation::Bind | Operation::Connect => 3,
        Operation::Accept => 1,
        Operation::Read | Operation::Write => 2,
        _ => unreachable!(),
    };
    *(frame.slots[index] as usize as *mut u64) = code;
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
unsafe fn begin(arena: &mut Arena, frame: &CpsFrame, op: Operation) -> Result<Request, Error> {
    let lease = if matches!(op, Operation::Bind | Operation::Connect) {
        None
    } else {
        Some(
            (frame.slots[0] as usize as *mut Buffer)
                .as_mut()
                .ok_or_else(|| failure("Null TCP receiver"))?
                .resource_mut()?
                .lease()?,
        )
    };
    let length = match op {
        Operation::Read => {
            usize::try_from(frame.slots[1]).map_err(|_| failure("TCP read limit overflow"))?
        }
        Operation::Write => (&*(frame.slots[1] as usize as *const Buffer))
            .bytes()?
            .len(),
        _ => 0,
    };
    let connecting_size = if op == Operation::Connect {
        IoSource::<TcpStream>::allocation_size()
    } else {
        0
    };
    let charge =
        arena.reserve_external(std::mem::size_of::<Request>() + length + connecting_size)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| failure("TCP request allocation failed"))?;
    if op == Operation::Write {
        bytes.extend_from_slice((&*(frame.slots[1] as usize as *const Buffer)).bytes()?);
    } else {
        bytes.resize(length, 0);
    }
    Ok(Request {
        event: ResourceEvent::new()?,
        registration: None,
        connecting: None,
        lease,
        bytes,
        _charge: charge,
    })
}
unsafe fn drive(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
) -> Result<Option<Output>, Error> {
    // Validate limits before reserving any host storage. Invalid user requests are Result errors.
    if frame.tcp_request.is_none() && matches!(op, Operation::Read | Operation::Write) {
        let length = if op == Operation::Read {
            frame.slots[1]
        } else {
            (&*(frame.slots[1] as usize as *const Buffer))
                .bytes()?
                .len() as u64
        };
        if length > super::byte_request::MAX_REQUEST_BYTES as u64
            || op == Operation::Read && length == 0
        {
            set_error(frame, op, 9);
            return Ok(Some(if op == Operation::Read {
                Output::Owner(Buffer::copy_bytes(arena, &[])?)
            } else {
                Output::Scalar(0)
            }));
        }
    }
    if frame.tcp_request.is_none() {
        frame.tcp_request = Some(Box::new(begin(arena, frame, op)?));
    }
    frame.host_stop = Some(stop);
    let request = frame.tcp_request.as_mut().unwrap();
    if matches!(op, Operation::Bind | Operation::Connect) {
        let addr = address(frame.slots[0], frame.slots[1], frame.slots[2]);
        if op == Operation::Bind {
            let result = addr.and_then(TcpListener::bind);
            let (socket, code) = match result {
                Ok(listener) => (Socket::Listener(IoSource::new(listener)), 0),
                Err(e) => (Socket::Closed, error_code(&e)),
            };
            set_error(frame, op, code);
            return Ok(Some(Output::Owner(adopt(arena, socket)?)));
        }
        if request.connecting.is_none() {
            match addr.and_then(TcpStream::connect) {
                Ok(stream) => {
                    let source = IoSource::new(stream);
                    request.registration = Some(Registration::Stream(
                        source.register(Interest::WRITABLE, Some(request.event.clone()))?,
                    ));
                    request.connecting = Some(source);
                }
                Err(e) => {
                    set_error(frame, op, error_code(&e));
                    return Ok(Some(Output::Owner(adopt(arena, Socket::Closed)?)));
                }
            }
        }
        // Connecting sockets require a writable hint before inspecting SO_ERROR.
        // On some platforms peer_addr before that hint reports a transient OS
        // error that must not be published as the connection's business result.
        if !request.registration.as_ref().unwrap().is_ready() {
            return Ok(None);
        }
        let source = request.connecting.as_ref().unwrap();
        let result = source.with_io(|s| {
            s.take_error().and_then(|error| match error {
                Some(e) => Err(e),
                None => s.peer_addr().map(|_| ()),
            })
        });
        match result {
            Ok(()) => {
                request.stop_io()?;
                let socket = Socket::Stream(request.connecting.take().unwrap());
                set_error(frame, op, 0);
                Ok(Some(Output::Owner(adopt(arena, socket)?)))
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                // Keep the collected writable hint: no WouldBlock consumed it.
                super::executor::current_waker().unwrap().wake();
                Ok(None)
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::NotConnected =>
            {
                request
                    .registration
                    .as_ref()
                    .unwrap()
                    .rearm(Interest::WRITABLE)?;
                Ok(None)
            }
            Err(e) => {
                request.stop_io()?;
                set_error(frame, op, error_code(&e));
                Ok(Some(Output::Owner(adopt(arena, Socket::Closed)?)))
            }
        }
    } else {
        let socket = &mut owner(request).socket as *mut Socket;
        match op {
            Operation::Accept => {
                let Socket::Listener(source) = &*socket else {
                    return Err(failure("TCP listener is closed"));
                };
                if request.registration.is_none() {
                    request.registration = Some(Registration::Listener(
                        source.register(Interest::READABLE, Some(request.event.clone()))?,
                    ));
                }
                match source.with_io(|s| s.accept()) {
                    Ok((stream, _)) => {
                        request.stop_io()?;
                        set_error(frame, op, 0);
                        Ok(Some(Output::Owner(adopt(
                            arena,
                            Socket::Stream(IoSource::new(stream)),
                        )?)))
                    }
                    Err(e) if retry(&e) => {
                        request
                            .registration
                            .as_ref()
                            .unwrap()
                            .rearm(Interest::READABLE)?;
                        if e.kind() == io::ErrorKind::Interrupted {
                            super::executor::current_waker().unwrap().wake();
                        }
                        Ok(None)
                    }
                    Err(e) => {
                        request.stop_io()?;
                        set_error(frame, op, error_code(&e));
                        Ok(Some(Output::Owner(adopt(arena, Socket::Closed)?)))
                    }
                }
            }
            Operation::Read | Operation::Write => {
                let Socket::Stream(source) = &*socket else {
                    return Err(failure("TCP stream is closed"));
                };
                if op == Operation::Write && request.bytes.is_empty() {
                    set_error(frame, op, 0);
                    return Ok(Some(Output::Scalar(0)));
                }
                let interest = if op == Operation::Read {
                    Interest::READABLE
                } else {
                    Interest::WRITABLE
                };
                if request.registration.is_none() {
                    request.registration = Some(Registration::Stream(
                        source.register(interest, Some(request.event.clone()))?,
                    ));
                }
                let result = if op == Operation::Read {
                    source.with_io(|s| s.read(&mut request.bytes))
                } else {
                    source
                        .with_io(|s| s.write(&request.bytes))
                        .and_then(|count| {
                            if count == 0 {
                                Err(io::ErrorKind::WriteZero.into())
                            } else {
                                Ok(count)
                            }
                        })
                };
                let (count, code) = match result {
                    Ok(count) => (count, 0),
                    Err(e) if retry(&e) => {
                        request.registration.as_ref().unwrap().rearm(interest)?;
                        if e.kind() == io::ErrorKind::Interrupted {
                            super::executor::current_waker().unwrap().wake();
                        }
                        return Ok(None);
                    }
                    Err(e) => (0, error_code(&e)),
                };
                request.stop_io()?;
                // Both host bytes and language output are charged at the copy peak.
                let output = if op == Operation::Read {
                    Output::Owner(Buffer::copy_bytes(arena, &request.bytes[..count])?)
                } else {
                    Output::Scalar(count as u64)
                };
                set_error(frame, op, code);
                Ok(Some(output))
            }
            Operation::StreamAddr | Operation::ListenerAddr => {
                let result = match &*socket {
                    Socket::Stream(source) => source.with_io(|s| {
                        if frame.slots[1] == 0 {
                            s.local_addr()
                        } else {
                            s.peer_addr()
                        }
                    }),
                    Socket::Listener(source) => source.with_io(|s| s.local_addr()),
                    Socket::Closed => Err(io::ErrorKind::NotConnected.into()),
                };
                match result {
                    Ok(addr) => {
                        let (hi, lo, endpoint) = encode_address(addr);
                        let offset = if op == Operation::StreamAddr { 2 } else { 1 };
                        for (index, value) in [hi, lo, endpoint].into_iter().enumerate() {
                            *(frame.slots[offset + index] as usize as *mut u64) = value;
                        }
                        Ok(Some(Output::Scalar(0)))
                    }
                    Err(e) => Ok(Some(Output::Scalar(error_code(&e)))),
                }
            }
            Operation::Shutdown => {
                let result = match &*socket {
                    Socket::Stream(source) => match frame.slots[1] {
                        0 => source.with_io(|s| s.shutdown(Shutdown::Read)),
                        1 => source.with_io(|s| s.shutdown(Shutdown::Write)),
                        2 => source.with_io(|s| s.shutdown(Shutdown::Both)),
                        _ => Err(io::ErrorKind::InvalidInput.into()),
                    },
                    _ => Err(io::ErrorKind::NotConnected.into()),
                };
                Ok(Some(Output::Scalar(
                    result.err().as_ref().map(error_code).unwrap_or(0),
                )))
            }
            Operation::CloseStream | Operation::CloseListener => {
                *socket = Socket::Closed;
                Ok(Some(Output::Scalar(0)))
            }
            _ => unreachable!(),
        }
    }
}
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
) -> Result<Option<Output>, Error> {
    if matches!(frame.state, DONE | FAILED) {
        return Err(failure("TCP operation has terminated"));
    }
    let result = drive(arena, frame, op);
    match &result {
        Ok(None) => {}
        Ok(Some(_)) => {
            frame.stop_host(STOP_COMPLETED);
            frame.state = DONE;
        }
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.tcp_request = None;
            frame.state = FAILED;
        }
    }
    result
}
#[export_name = "__joky_tcp_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame, op: u32) -> i32 {
    let op = match op {
        0 => Operation::Bind,
        1 => Operation::Connect,
        2 => Operation::Accept,
        3 => Operation::Read,
        4 => Operation::Write,
        5 => Operation::StreamAddr,
        6 => Operation::ListenerAddr,
        7 => Operation::Shutdown,
        8 => Operation::CloseStream,
        9 => Operation::CloseListener,
        _ => {
            super::error::record_error(failure("Invalid TCP operation"));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(Output::Owner(owner))) => {
            if op == Operation::Read {
                (&mut (*frame).slots)[3] = owner.as_ptr() as usize as u64;
            } else {
                (*frame).result = owner.as_ptr() as usize as u64;
            }
            0
        }
        Ok(Some(Output::Scalar(value))) => {
            (*frame).result = value;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
thread_local! { pub(crate) static LIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
impl Drop for Owner {
    fn drop(&mut self) {
        #[cfg(test)]
        LIVE.with(|count| count.set(count.get() - 1));
    }
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;
