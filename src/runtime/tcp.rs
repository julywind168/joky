//! Compiler-checked TCP operations. Only bounded host storage crosses Pending.
use super::{
    arena::{Arena, ExternalCharge},
    borrowed_host::{STOP_COMPLETED, STOP_FAILED},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    reactor::{IoRegistration, IoSource, SocketPermit},
    resource::{OwnedResource, ResourceEvent, ResourceLease},
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use mio::{
    net::{TcpListener, TcpStream},
    Interest,
};
use std::{
    cell::Cell,
    ffi::c_void,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV6},
    ptr::NonNull,
    rc::Rc,
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
    SplitReader,
    SplitWriter,
    ReadHalf,
    WriteHalf,
    ReaderAddr,
    WriterAddr,
    ShutdownReader,
    ShutdownWriter,
    CloseReader,
    CloseWriter,
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
        "__tcp_split_reader" => Operation::SplitReader,
        "__tcp_split_writer" => Operation::SplitWriter,
        "__tcp_read_half" => Operation::ReadHalf,
        "__tcp_write_half" => Operation::WriteHalf,
        "__tcp_reader_addr" => Operation::ReaderAddr,
        "__tcp_writer_addr" => Operation::WriterAddr,
        "__tcp_shutdown_reader" => Operation::ShutdownReader,
        "__tcp_shutdown_writer" => Operation::ShutdownWriter,
        "__tcp_close_reader" => Operation::CloseReader,
        "__tcp_close_writer" => Operation::CloseWriter,
        _ => return None,
    })
}
impl Operation {
    pub(crate) fn is_read(self) -> bool {
        matches!(self, Self::Read | Self::ReadHalf)
    }
    fn is_write(self) -> bool {
        matches!(self, Self::Write | Self::WriteHalf)
    }

    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        let reader = Type::Resource("tcp.ReaderOwner".into());
        let writer = Type::Resource("tcp.WriterOwner".into());
        let stream = match self {
            Self::ReadHalf | Self::ReaderAddr | Self::ShutdownReader | Self::CloseReader => {
                reader.clone()
            }
            Self::WriteHalf | Self::WriterAddr | Self::ShutdownWriter | Self::CloseWriter => {
                writer.clone()
            }
            _ => Type::Resource("tcp.StreamOwner".into()),
        };
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
            Self::Read | Self::ReadHalf => (
                vec![reference(stream.clone()), scalar.clone(), error.clone()],
                signature.ret.clone(),
            ),
            Self::Write | Self::WriteHalf => (
                vec![
                    reference(stream.clone()),
                    signature.params.get(1).cloned().unwrap_or(Type::Unit),
                    error.clone(),
                ],
                scalar.clone(),
            ),
            Self::StreamAddr | Self::ReaderAddr | Self::WriterAddr => (
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
            Self::SplitReader => (vec![reference(stream.clone())], reader),
            Self::SplitWriter => (vec![reference(stream.clone())], writer),
            Self::ShutdownReader | Self::ShutdownWriter | Self::CloseReader | Self::CloseWriter => {
                (vec![reference(stream.clone())], scalar.clone())
            }
        };
        if !suspends
            || signature.params != params
            || signature.ret != ret
            || self.is_read() && !bytes(&signature.ret)
            || self.is_write() && !signature.params.get(1).is_some_and(bytes)
        {
            return Err(failure(
                "TCP operation requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}
pub(crate) fn validate_external_resource(name: &str) -> Result<(), Error> {
    if matches!(
        name,
        "domain.StateOwner"
            | "domain.SnapshotOwner"
            | "domain.CandidateOwner"
            | "domain.CommitOwner"
            | "domain.DeadlineOwner"
            | "tcp.StreamOwner"
            | "tcp.ListenerOwner"
            | "tcp.ReaderOwner"
            | "tcp.WriterOwner"
    ) {
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
pub(crate) fn encode_address(address: SocketAddr) -> (u64, u64, u64) {
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
struct Connection {
    source: IoSource<TcpStream>,
    addresses: [Option<SocketAddr>; 2],
    _permit: Option<SocketPermit>,
    read_closed: Cell<bool>,
    write_closed: Cell<bool>,
    _charge: ExternalCharge,
}
impl Connection {
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let closed = match how {
            Shutdown::Read => self.read_closed.get(),
            Shutdown::Write => self.write_closed.get(),
            Shutdown::Both => self.read_closed.get() && self.write_closed.get(),
        };
        if closed {
            return Ok(());
        }
        self.source.with_io(|s| s.shutdown(how))?;
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            self.read_closed.set(true);
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            self.write_closed.set(true);
        }
        Ok(())
    }
}
enum Socket {
    Stream(Rc<Connection>),
    Reader(Rc<Connection>),
    Writer(Rc<Connection>),
    // Owns the unissued writer between the two checked split primitives.
    Splitting(Rc<Connection>),
    Listener(IoSource<TcpListener>),
    Closed,
}
struct Owner {
    socket: Socket,
    permit: Option<SocketPermit>,
    _charge: ExternalCharge,
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
fn adopt(
    arena: &mut Arena,
    socket: Socket,
    permit: Option<SocketPermit>,
) -> Result<NonNull<Buffer>, Error> {
    let source_size = match &socket {
        Socket::Listener(_) => IoSource::<TcpListener>::allocation_size(),
        _ => 0,
    };
    // Charge the owner and Rc source allocation. Kernel buffers and Mio's platform
    // internals are outside task payload accounting.
    let charge = arena.reserve_external(std::mem::size_of::<Owner>() + source_size)?;
    #[cfg(test)]
    LIVE.with(|count| count.set(count.get() + 1));
    let token = NonNull::new(
        Box::into_raw(Box::new(Owner {
            socket,
            permit,
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
fn adopt_stream(
    arena: &mut Arena,
    source: IoSource<TcpStream>,
    permit: Option<SocketPermit>,
) -> Result<NonNull<Buffer>, Error> {
    let charge = arena.reserve_external(
        std::mem::size_of::<Connection>()
            + 2 * std::mem::size_of::<usize>()
            + IoSource::<TcpStream>::allocation_size(),
    )?;
    let addresses = source.with_io(|s| [s.local_addr().ok(), s.peer_addr().ok()]);
    let connection = Rc::new(Connection {
        source,
        addresses,
        _permit: permit,
        read_closed: Cell::new(false),
        write_closed: Cell::new(false),
        _charge: charge,
    });
    adopt(arena, Socket::Stream(connection), None)
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
    permit: Option<SocketPermit>,
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
        Operation::Read | Operation::Write | Operation::ReadHalf | Operation::WriteHalf => 2,
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
        Operation::Read | Operation::ReadHalf => {
            usize::try_from(frame.slots[1]).map_err(|_| failure("TCP read limit overflow"))?
        }
        Operation::Write | Operation::WriteHalf => (&*(frame.slots[1] as usize as *const Buffer))
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
    if op.is_write() {
        bytes.extend_from_slice((&*(frame.slots[1] as usize as *const Buffer)).bytes()?);
    } else {
        bytes.resize(length, 0);
    }
    let permit = if matches!(op, Operation::Bind | Operation::Connect | Operation::Accept) {
        Some(super::executor::current_reactor()?.reserve_socket()?)
    } else {
        None
    };
    Ok(Request {
        event: ResourceEvent::new()?,
        registration: None,
        connecting: None,
        permit,
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
    if frame.tcp_request.is_none() && (op.is_read() || op.is_write()) {
        let length = if op.is_read() {
            frame.slots[1]
        } else {
            (&*(frame.slots[1] as usize as *const Buffer))
                .bytes()?
                .len() as u64
        };
        if length > super::byte_request::MAX_REQUEST_BYTES as u64 || op.is_read() && length == 0 {
            set_error(frame, op, 9);
            return Ok(Some(if op.is_read() {
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
            let permit = if code == 0 {
                request.permit.take()
            } else {
                None
            };
            set_error(frame, op, code);
            return Ok(Some(Output::Owner(adopt(arena, socket, permit)?)));
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
                    return Ok(Some(Output::Owner(adopt(arena, Socket::Closed, None)?)));
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
                let source = request.connecting.take().unwrap();
                let permit = request.permit.take();
                set_error(frame, op, 0);
                Ok(Some(Output::Owner(adopt_stream(arena, source, permit)?)))
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
                Ok(Some(Output::Owner(adopt(arena, Socket::Closed, None)?)))
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
                        let permit = request.permit.take();
                        set_error(frame, op, 0);
                        Ok(Some(Output::Owner(adopt_stream(
                            arena,
                            IoSource::new(stream),
                            permit,
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
                        Ok(Some(Output::Owner(adopt(arena, Socket::Closed, None)?)))
                    }
                }
            }
            Operation::Read | Operation::Write | Operation::ReadHalf | Operation::WriteHalf => {
                let connection = match (&*socket, op) {
                    (Socket::Stream(c), Operation::Read | Operation::Write)
                    | (Socket::Reader(c), Operation::ReadHalf)
                    | (Socket::Writer(c), Operation::WriteHalf) => c,
                    _ => return Err(failure("Invalid TCP direction owner")),
                };
                if op == Operation::ReadHalf && connection.read_closed.get() {
                    set_error(frame, op, 0);
                    return Ok(Some(Output::Owner(Buffer::copy_bytes(arena, &[])?)));
                }
                if op == Operation::WriteHalf && connection.write_closed.get() {
                    set_error(frame, op, 4);
                    return Ok(Some(Output::Scalar(0)));
                }
                let source = &connection.source;
                if op.is_write() && request.bytes.is_empty() {
                    set_error(frame, op, 0);
                    return Ok(Some(Output::Scalar(0)));
                }
                let interest = if op.is_read() {
                    Interest::READABLE
                } else {
                    Interest::WRITABLE
                };
                if request.registration.is_none() {
                    request.registration = Some(Registration::Stream(
                        source.register(interest, Some(request.event.clone()))?,
                    ));
                }
                let result = if op.is_read() {
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
                let output = if op.is_read() {
                    Output::Owner(Buffer::copy_bytes(arena, &request.bytes[..count])?)
                } else {
                    Output::Scalar(count as u64)
                };
                set_error(frame, op, code);
                Ok(Some(output))
            }
            Operation::StreamAddr
            | Operation::ReaderAddr
            | Operation::WriterAddr
            | Operation::ListenerAddr => {
                let result = match &*socket {
                    Socket::Reader(c) | Socket::Writer(c) => {
                        let peer = usize::from(frame.slots[1] != 0);
                        if let Some(address) = c.addresses[peer] {
                            Ok(address)
                        } else {
                            c.source.with_io(|s| {
                                if peer == 0 {
                                    s.local_addr()
                                } else {
                                    s.peer_addr()
                                }
                            })
                        }
                    }
                    Socket::Stream(c) => c.source.with_io(|s| {
                        if frame.slots[1] == 0 {
                            s.local_addr()
                        } else {
                            s.peer_addr()
                        }
                    }),
                    Socket::Listener(source) => source.with_io(|s| s.local_addr()),
                    _ => Err(io::ErrorKind::NotConnected.into()),
                };
                match result {
                    Ok(addr) => {
                        let (hi, lo, endpoint) = encode_address(addr);
                        let offset = if op == Operation::ListenerAddr { 1 } else { 2 };
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
                    Socket::Stream(c) => match frame.slots[1] {
                        0 => c.shutdown(Shutdown::Read),
                        1 => c.shutdown(Shutdown::Write),
                        2 => c.shutdown(Shutdown::Both),
                        _ => Err(io::ErrorKind::InvalidInput.into()),
                    },
                    _ => Err(io::ErrorKind::NotConnected.into()),
                };
                Ok(Some(Output::Scalar(
                    result.err().as_ref().map(error_code).unwrap_or(0),
                )))
            }
            Operation::SplitReader => {
                let Socket::Stream(c) = &*socket else {
                    return Err(failure("TCP stream cannot be split twice"));
                };
                let c = c.clone();
                *socket = Socket::Splitting(c.clone());
                Ok(Some(Output::Owner(adopt(arena, Socket::Reader(c), None)?)))
            }
            Operation::SplitWriter => {
                let Socket::Splitting(c) = &*socket else {
                    return Err(failure("TCP split has no unissued writer"));
                };
                let writer = adopt(arena, Socket::Writer(c.clone()), None)?;
                *socket = Socket::Closed;
                Ok(Some(Output::Owner(writer)))
            }
            Operation::ShutdownReader
            | Operation::ShutdownWriter
            | Operation::CloseReader
            | Operation::CloseWriter => {
                let result = match (&*socket, op) {
                    (Socket::Reader(c), Operation::ShutdownReader | Operation::CloseReader) => {
                        c.shutdown(Shutdown::Read)
                    }
                    (Socket::Writer(c), Operation::ShutdownWriter | Operation::CloseWriter) => {
                        c.shutdown(Shutdown::Write)
                    }
                    _ => return Err(failure("Invalid TCP direction owner")),
                };
                if matches!(op, Operation::CloseReader | Operation::CloseWriter) {
                    *socket = Socket::Closed;
                }
                Ok(Some(Output::Scalar(
                    result.err().as_ref().map(error_code).unwrap_or(0),
                )))
            }
            Operation::CloseStream | Operation::CloseListener => {
                *socket = Socket::Closed;
                owner(request).permit.take();
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
        10 => Operation::SplitReader,
        11 => Operation::SplitWriter,
        12 => Operation::ReadHalf,
        13 => Operation::WriteHalf,
        14 => Operation::ReaderAddr,
        15 => Operation::WriterAddr,
        16 => Operation::ShutdownReader,
        17 => Operation::ShutdownWriter,
        18 => Operation::CloseReader,
        19 => Operation::CloseWriter,

        _ => {
            super::error::record_error(failure("Invalid TCP operation"));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(Output::Owner(owner))) => {
            if op.is_read() {
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
        match &self.socket {
            Socket::Reader(c) => {
                let _ = c.shutdown(Shutdown::Read);
            }
            Socket::Writer(c) | Socket::Splitting(c) => {
                let _ = c.shutdown(Shutdown::Write);
            }
            _ => {}
        }
        #[cfg(test)]
        LIVE.with(|count| count.set(count.get() - 1));
    }
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;
