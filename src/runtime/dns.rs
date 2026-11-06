//! Bounded system name resolution. Workers never access task or engine storage.
use super::{
    arena::{Arena, ExternalCharge},
    borrowed_host::{STOP_COMPLETED, STOP_FAILED},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    executor::{current_waker, TaskWaker},
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard, OnceLock,
    },
    thread::{self, JoinHandle},
};

const WORKERS: usize = 4;
const QUEUED: usize = 64;
const REQUESTS: usize = 128;
const HOST_BYTES: usize = 1024;
const ADDRESSES: usize = 64;
const ERROR_BYTES: usize = 1024;
const PACKET_BYTES: usize = ADDRESSES * 24;
const INVALID_INPUT: u64 = 1;
const BUSY: u64 = 2;
const NO_ADDRESSES: u64 = 3;
const LIMIT_EXCEEDED: u64 = 4;
const SYSTEM_FAILURE: u64 = 5;

fn failure(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::HostError, message)
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub(crate) fn is_lookup(name: &str) -> bool {
    name == "__dns_lookup"
}
pub(crate) fn validate(signature: &FunctionSignature, suspends: bool) -> Result<(), Error> {
    let bytes = matches!(&signature.ret, Type::Record(r) if r.name == "joky/array.Array"
        && r.arguments == [Type::UInt8] && r.fields.len() == 1
        && matches!(&r.fields[0].ty, Type::Buffer(t) if **t == Type::UInt8));
    if !suspends
        || signature.params
            != [
                Type::reference(Type::String, false),
                Type::UInt16,
                Type::reference(Type::UInt64, true),
            ]
        || !bytes
    {
        return Err(Error::new_with_code(
            ErrorPhase::Abi,
            ErrorCode::AbiError,
            "DNS lookup requires its exact compiler-owned signature",
        ));
    }
    Ok(())
}

// Fixed-size publication storage bounds results even when the owner stops polling.
struct Packet {
    code: u64,
    length: usize,
    bytes: [u8; PACKET_BYTES],
}
impl Packet {
    fn empty() -> Self {
        Self {
            code: 0,
            length: 0,
            bytes: [0; PACKET_BYTES],
        }
    }
    fn error(code: u64, message: &str) -> Self {
        let mut packet = Self::empty();
        packet.code = code;
        let mut length = message.len().min(ERROR_BYTES);
        while !message.is_char_boundary(length) {
            length -= 1;
        }
        packet.bytes[..length].copy_from_slice(&message.as_bytes()[..length]);
        packet.length = length;
        packet
    }
    fn addresses(addresses: impl IntoIterator<Item = SocketAddr>) -> Self {
        let mut packet = Self::empty();
        for address in addresses {
            let (high, low, endpoint) = super::tcp::encode_address(address);
            let mut bytes = [0; 24];
            for (chunk, value) in bytes
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip([high, low, endpoint])
            {
                chunk.copy_from_slice(&value.to_be_bytes());
            }
            if packet.bytes[..packet.length]
                .as_chunks::<24>()
                .0
                .contains(&bytes)
            {
                continue;
            }
            if packet.length == PACKET_BYTES {
                return Self::error(LIMIT_EXCEEDED, "DNS address result limit exceeded");
            }
            packet.bytes[packet.length..packet.length + 24].copy_from_slice(&bytes);
            packet.length += 24;
        }
        if packet.length == 0 {
            return Self::error(NO_ADDRESSES, "Name resolution returned no addresses");
        }
        packet
    }
}
fn resolve(host: &str, port: u16) -> Packet {
    match (host, port).to_socket_addrs() {
        Ok(addresses) => Packet::addresses(addresses),
        // Format into bounded storage without allocating an unbounded diagnostic.
        Err(error) => {
            use std::fmt::Write;
            struct Diagnostic(String);
            impl std::fmt::Write for Diagnostic {
                fn write_str(&mut self, text: &str) -> std::fmt::Result {
                    let mut length = text.len().min(ERROR_BYTES - self.0.len());
                    while !text.is_char_boundary(length) {
                        length -= 1;
                    }
                    self.0.push_str(&text[..length]);
                    Ok(())
                }
            }
            let mut message = Diagnostic(String::with_capacity(ERROR_BYTES));
            let _ = write!(&mut message, "{error}");
            Packet::error(SYSTEM_FAILURE, &message.0)
        }
    }
}
fn immediate(host: &str, port: u16) -> Option<Packet> {
    if host.is_empty()
        || host.len() > HOST_BYTES
        || !host.is_ascii()
        || host.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'/' | b'\\' | b'[' | b']' | b'%' | b'@')
        })
    {
        return Some(Packet::error(INVALID_INPUT, "Invalid lookup host"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(Packet::addresses([SocketAddr::new(ip, port)]));
    }
    if host.contains(':') {
        return Some(Packet::error(
            INVALID_INPUT,
            "Lookup requires a host without a port",
        ));
    }
    None
}

struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
// Notifications carry no task storage and may outlive cancellation.
enum Wake {
    Local(TaskWaker),
    Parallel(super::parallel::pool::IoWaker),
}
impl Wake {
    fn current() -> Result<Self, Error> {
        if let Some(waker) = super::parallel::pool::io_waker() {
            Ok(Self::Parallel(waker))
        } else {
            current_waker()
                .map(Self::Local)
                .ok_or_else(|| failure("DNS lookup requires a task executor"))
        }
    }
    fn wake(&self) {
        match self {
            Self::Local(waker) => waker.wake(),
            Self::Parallel(waker) => waker.wake(),
        }
    }
}
impl From<TaskWaker> for Wake {
    fn from(waker: TaskWaker) -> Self {
        Self::Local(waker)
    }
}
struct State {
    cancelled: bool,
    running: bool,
    result: Option<Packet>,
    waker: Option<Wake>,
}
struct Ticket {
    state: Mutex<State>,
    _permit: Permit,
}
struct Job {
    host: String,
    port: u16,
    ticket: Arc<Ticket>,
}
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}
type Resolver = dyn Fn(&str, u16) -> Packet + Send + Sync;
struct Core {
    queue: Mutex<Queue>,
    signal: Condvar,
    outstanding: Arc<AtomicUsize>,
    live: AtomicUsize,
    resolver: Arc<Resolver>,
    queue_limit: usize,
    request_limit: usize,
}
struct Service {
    core: Arc<Core>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}
impl Service {
    fn new(
        workers: usize,
        queue_limit: usize,
        request_limit: usize,
        resolver: Arc<Resolver>,
    ) -> Result<Self, Error> {
        let core = Arc::new(Core {
            queue: Mutex::new(Queue {
                jobs: VecDeque::with_capacity(queue_limit),
                closed: false,
            }),
            signal: Condvar::new(),
            outstanding: Arc::default(),
            live: AtomicUsize::new(0),
            resolver,
            queue_limit,
            request_limit,
        });
        let service = Self {
            core,
            threads: Mutex::new(Vec::with_capacity(workers)),
        };
        for index in 0..workers {
            let core = service.core.clone();
            core.live.fetch_add(1, Ordering::AcqRel);
            match thread::Builder::new()
                .name(format!("joky-dns-{index}"))
                .spawn(move || worker(core))
            {
                Ok(handle) => lock(&service.threads).push(handle),
                Err(error) => {
                    service.core.live.fetch_sub(1, Ordering::AcqRel);
                    service.close();
                    for handle in lock(&service.threads).drain(..) {
                        let _ = handle.join();
                    }
                    return Err(failure(format!(
                        "DNS worker initialization failed: {error}"
                    )));
                }
            }
        }
        Ok(service)
    }
    fn submit(
        &self,
        host: &str,
        port: u16,
        waker: impl Into<Wake>,
    ) -> Result<Option<Arc<Ticket>>, Error> {
        let mut queue = lock(&self.core.queue);
        if queue.closed {
            return Err(failure("DNS service is closed"));
        }
        if queue.jobs.len() >= self.core.queue_limit {
            return Ok(None);
        }
        if self
            .core
            .outstanding
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.core.request_limit).then_some(n + 1)
            })
            .is_err()
        {
            return Ok(None);
        }
        let permit = Permit(self.core.outstanding.clone());
        let mut owned = String::new();
        owned
            .try_reserve_exact(host.len())
            .map_err(|_| failure("DNS input allocation failed"))?;
        owned.push_str(host);
        let ticket = Arc::new(Ticket {
            state: Mutex::new(State {
                cancelled: false,
                running: false,
                result: None,
                waker: Some(waker.into()),
            }),
            _permit: permit,
        });
        queue.jobs.push_back(Job {
            host: owned,
            port,
            ticket: ticket.clone(),
        });
        self.core.signal.notify_one();
        Ok(Some(ticket))
    }
    fn cancel(&self, ticket: &Arc<Ticket>) {
        let mut queue = lock(&self.core.queue);
        let mut state = lock(&ticket.state);
        state.cancelled = true;
        state.result = None;
        state.waker = None;
        queue.jobs.retain(|job| !Arc::ptr_eq(&job.ticket, ticket));
    }
    fn close(&self) {
        let mut queue = lock(&self.core.queue);
        queue.closed = true;
        for job in queue.jobs.drain(..) {
            let mut state = lock(&job.ticket.state);
            state.result = Some(Packet::error(SYSTEM_FAILURE, "DNS service is closed"));
            if let Some(waker) = &state.waker {
                waker.wake();
            }
        }
        self.core.signal.notify_all();
    }
    fn quiescent(&self) -> bool {
        if self.core.live.load(Ordering::Acquire) != 0 {
            return false;
        }
        // Only already-finished threads are joined; shutdown never waits for resolution.
        let mut threads = lock(&self.threads);
        if threads.iter().any(|thread| !thread.is_finished()) {
            return false;
        }
        for thread in threads.drain(..) {
            let _ = thread.join();
        }
        true
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.close();
    }
}
fn worker(core: Arc<Core>) {
    loop {
        let job = {
            let mut queue = lock(&core.queue);
            while queue.jobs.is_empty() && !queue.closed {
                queue = core
                    .signal
                    .wait(queue)
                    .unwrap_or_else(|error| error.into_inner());
            }
            match queue.jobs.pop_front() {
                Some(job) => job,
                None => break,
            }
        };
        {
            let mut state = lock(&job.ticket.state);
            if state.cancelled {
                continue;
            }
            state.running = true;
        }
        let result = catch_unwind(AssertUnwindSafe(|| (core.resolver)(&job.host, job.port)))
            .unwrap_or_else(|_| Packet::error(SYSTEM_FAILURE, "DNS resolver panicked"));
        let mut state = lock(&job.ticket.state);
        if !state.cancelled {
            state.result = Some(result);
            // Publish and notify under the same lock used to revoke this operation.
            if let Some(waker) = &state.waker {
                waker.wake();
            }
        }
    }
    core.live.fetch_sub(1, Ordering::AcqRel);
}

static SERVICE: OnceLock<Result<Arc<Service>, Error>> = OnceLock::new();
/// Prepare the shared fixed worker pool before polling DNS tasks.
/// VM module loading and blocking drivers do this automatically. Initialization
/// failures are sticky. Each separately linked runtime has its own service.
pub fn initialize() -> Result<(), Error> {
    SERVICE
        .get_or_init(|| Service::new(WORKERS, QUEUED, REQUESTS, Arc::new(resolve)).map(Arc::new))
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|service| {
            if lock(&service.core.queue).closed {
                Err(failure("DNS service is closed"))
            } else {
                Ok(())
            }
        })
}
fn service() -> Result<Arc<Service>, Error> {
    #[cfg(test)]
    if let Some(service) = TEST_SERVICE.with(|s| s.borrow().clone()) {
        return Ok(service);
    }
    SERVICE
        .get()
        .ok_or_else(|| failure("Initialize DNS before polling native DNS tasks"))?
        .as_ref()
        .cloned()
        .map_err(Clone::clone)
}
/// Permanently stop service admission. Returns true only once all workers have
/// exited and have been joined. Never waits for a running system resolver.
/// Cancel/destroy all task owners first; keep the runtime library loaded until
/// this returns true. System resolution has no bounded termination guarantee.
pub fn shutdown() -> bool {
    match SERVICE.get_or_init(|| Err(failure("DNS service is closed"))) {
        Ok(service) => {
            service.close();
            service.quiescent()
        }
        Err(_) => true,
    }
}
#[no_mangle]
pub extern "C" fn joky_dns_initialize() -> i32 {
    match initialize() {
        Ok(()) => 0,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
#[no_mangle]
pub extern "C" fn joky_dns_shutdown() -> i32 {
    i32::from(shutdown())
}

struct Request {
    service: Arc<Service>,
    ticket: Arc<Ticket>,
    _charge: ExternalCharge,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.service.cancel(&self.ticket);
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Request>()));
    }
}
unsafe fn output(
    arena: &mut Arena,
    frame: &CpsFrame,
    packet: &Packet,
) -> Result<NonNull<Buffer>, Error> {
    let owner = Buffer::copy_bytes(arena, &packet.bytes[..packet.length])?;
    *(frame.slots[2] as usize as *mut u64) = packet.code;
    Ok(owner)
}
/// The checked primitive supplies an owned String handle, UInt16 port and live
/// exclusive UInt64 error reference. Only owned copies reach worker threads.
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
) -> Result<Option<NonNull<Buffer>>, Error> {
    if matches!(frame.state, DONE | FAILED) {
        return Err(failure("DNS lookup has terminated"));
    }
    let result = (|| {
        if frame.host_context.is_null() {
            let host = super::owned_text::borrow(frame.slots[0] as usize as *const Buffer)?;
            let port = frame.slots[1] as u16;
            if let Some(packet) = immediate(host, port) {
                return output(arena, frame, &packet).map(Some);
            }
            let service = service()?;
            let waker = Wake::current()?;
            let charge = arena.reserve_external(
                std::mem::size_of::<Request>()
                    + std::mem::size_of::<Ticket>()
                    + 2 * std::mem::size_of::<usize>() // Arc allocation header.
                    + host.len(),
            )?;
            let Some(ticket) = service.submit(host, port, waker)? else {
                return output(
                    arena,
                    frame,
                    &Packet::error(BUSY, "DNS request limit exceeded"),
                )
                .map(Some);
            };
            frame.host_context = Box::into_raw(Box::new(Request {
                service,
                ticket,
                _charge: charge,
            }))
            .cast();
            frame.host_stop = Some(stop);
        }
        let request = &*frame.host_context.cast::<Request>();
        let mut state = lock(&request.ticket.state);
        let Some(packet) = state.result.take() else {
            return Ok(None);
        };
        // Keep task-side charges until the language copy has been created.
        output(arena, frame, &packet).map(Some)
    })();
    match &result {
        Ok(None) => {}
        Ok(Some(_)) => {
            frame.stop_host(STOP_COMPLETED);
            frame.state = DONE;
        }
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.state = FAILED;
        }
    }
    result
}
#[export_name = "__joky_dns_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame) -> i32 {
    match poll(&mut *super::cps::arena(frame), &mut *frame) {
        Ok(Some(owner)) => {
            (&mut (*frame).slots)[3] = owner.as_ptr() as usize as u64;
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
thread_local! { static TEST_SERVICE: std::cell::RefCell<Option<Arc<Service>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
#[path = "dns_tests.rs"]
mod tests;
