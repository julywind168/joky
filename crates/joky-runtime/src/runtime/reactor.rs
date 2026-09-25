//! Runtime reactor for timer and socket readiness notifications.
//!
//! The reactor never executes language code. It owns a single event-loop
//! thread and turns completed registrations into scheduler jobs.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use mio::net::UnixDatagram;
use mio::net::{TcpListener, TcpStream, UdpSocket};
#[cfg(unix)]
use mio::net::{UnixListener, UnixStream};
use mio::{event::Event, Events, Interest, Poll, Token, Waker};

pub(crate) type TimerCallback = Box<dyn FnOnce() + Send + 'static>;
pub(crate) type TcpReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<TcpStream>>) -> bool + Send + 'static>;
pub(crate) type TcpListenerReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<TcpListener>>) -> bool + Send + 'static>;
pub(crate) type UdpReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<UdpSocket>>) -> bool + Send + 'static>;
#[cfg(unix)]
pub(crate) type UnixReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<UnixStream>>) -> bool + Send + 'static>;
#[cfg(unix)]
pub(crate) type UnixListenerReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<UnixListener>>) -> bool + Send + 'static>;
#[cfg(unix)]
pub(crate) type UnixDatagramReadyCallback =
    Box<dyn FnMut(Event, Arc<Mutex<UnixDatagram>>) -> bool + Send + 'static>;
pub(crate) type TcpRegistrationErrorCallback = Box<dyn FnOnce(Error) + Send + 'static>;

enum ReactorCommand {
    #[cfg(any(test, feature = "test-support"))]
    Snapshot(
        Option<crate::runtime::scope::ScopeId>,
        std::sync::mpsc::Sender<ResourceSnapshot>,
    ),
    Register {
        id: u64,
        deadline: Instant,
        callback: TimerCallback,
        on_cancel: Option<TimerCallback>,
    },
    RegisterTcp {
        id: u64,
        stream: Arc<Mutex<TcpStream>>,
        interest: Interest,
        dynamic_interest: Option<Arc<Mutex<Interest>>>,
        callback: TcpReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    RegisterTcpListener {
        id: u64,
        listener: Arc<Mutex<TcpListener>>,
        interest: Interest,
        callback: TcpListenerReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    RegisterUdp {
        id: u64,
        socket: Arc<Mutex<UdpSocket>>,
        interest: Interest,
        callback: UdpReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    #[cfg(unix)]
    RegisterUnix {
        id: u64,
        stream: Arc<Mutex<UnixStream>>,
        interest: Interest,
        callback: UnixReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    #[cfg(unix)]
    RegisterUnixListener {
        id: u64,
        listener: Arc<Mutex<UnixListener>>,
        interest: Interest,
        callback: UnixListenerReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    #[cfg(unix)]
    RegisterUnixDatagram {
        id: u64,
        socket: Arc<Mutex<UnixDatagram>>,
        interest: Interest,
        callback: UnixDatagramReadyCallback,
        on_error: Option<TcpRegistrationErrorCallback>,
    },
    CancelTimer(u64),
    CancelIo(u64),
}

struct TimerRegistration {
    deadline: Instant,
    callback: Option<TimerCallback>,
    on_cancel: Option<TimerCallback>,
}

struct TcpRegistration {
    stream: Arc<Mutex<TcpStream>>,
    interest: Interest,
    dynamic_interest: Option<Arc<Mutex<Interest>>>,
    callback: TcpReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
}

struct TcpListenerRegistration {
    listener: Arc<Mutex<TcpListener>>,
    interest: Interest,
    callback: TcpListenerReadyCallback,
}

struct UdpRegistration {
    socket: Arc<Mutex<UdpSocket>>,
    interest: Interest,
    callback: UdpReadyCallback,
}

#[cfg(unix)]
struct UnixRegistration {
    stream: Arc<Mutex<UnixStream>>,
    interest: Interest,
    callback: UnixReadyCallback,
}

#[cfg(unix)]
struct UnixListenerRegistration {
    listener: Arc<Mutex<UnixListener>>,
    interest: Interest,
    callback: UnixListenerReadyCallback,
}

#[cfg(unix)]
struct UnixDatagramRegistration {
    socket: Arc<Mutex<UnixDatagram>>,
    interest: Interest,
    callback: UnixDatagramReadyCallback,
}

struct RequestRecord {
    timer: bool,
    #[cfg(any(test, feature = "test-support"))]
    scope: crate::runtime::scope::ScopeId,
    cancelled: bool,
}

struct Reactor {
    commands: Mutex<VecDeque<ReactorCommand>>,
    wake: Arc<Waker>,
    next_id: AtomicU64,
    pending: AtomicU64,
    requests: Mutex<HashMap<u64, RequestRecord>>,
}

impl Reactor {
    fn reserve(&self, timer: bool) -> u64 {
        let id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("reactor request namespace exhausted");
        self.requests
            .lock()
            .expect("reactor requests mutex")
            .insert(
                id,
                RequestRecord {
                    timer,
                    #[cfg(any(test, feature = "test-support"))]
                    scope: crate::runtime::scope::current_id(),
                    cancelled: false,
                },
            );
        self.pending.fetch_add(1, Ordering::AcqRel);
        id
    }

    fn request_cancel(&self, id: u64, timer: bool) -> bool {
        let mut requests = self.requests.lock().expect("reactor requests mutex");
        let Some(request) = requests.get_mut(&id) else {
            return false;
        };
        if request.timer != timer || request.cancelled {
            return false;
        }
        request.cancelled = true;
        true
    }

    fn is_cancelled(&self, id: u64) -> bool {
        self.requests
            .lock()
            .expect("reactor requests mutex")
            .get(&id)
            .is_some_and(|request| request.cancelled)
    }
}

/// Reserved for cross-thread reactor commands. Socket registrations will use
/// tokens allocated from the same namespace, leaving this token permanently
/// available for `Waker` notifications.
const WAKE_TOKEN: Token = Token(0);

static REACTOR: OnceLock<Arc<Reactor>> = OnceLock::new();

fn reactor() -> &'static Arc<Reactor> {
    REACTOR.get_or_init(|| {
        let poll = Poll::new().expect("failed to create Joky reactor poll");
        let wake = Arc::new(
            Waker::new(poll.registry(), WAKE_TOKEN).expect("failed to create Joky reactor waker"),
        );
        let reactor = Arc::new(Reactor {
            commands: Mutex::new(VecDeque::new()),
            wake,
            next_id: AtomicU64::new(1),
            pending: AtomicU64::new(0),
            requests: Mutex::new(HashMap::new()),
        });
        let thread_reactor = Arc::clone(&reactor);
        thread::Builder::new()
            .name("joky-reactor".to_string())
            .spawn(move || run(thread_reactor, poll))
            .expect("failed to start Joky reactor");
        reactor
    })
}

/// Register a timer with a distinct cancellation callback.  The cancellation
/// hook is also run when a `CancelTimer` command wins before the registration
/// command has reached the reactor thread, so every accepted timer has one
/// terminal path.
/// Test convenience: register a timer without a cancellation hook.
#[cfg(test)]
pub(crate) fn register_timer(deadline: Instant, callback: TimerCallback) -> u64 {
    register_timer_with_cancel(deadline, callback, None)
}

pub(crate) fn register_timer_with_cancel(
    deadline: Instant,
    callback: TimerCallback,
    on_cancel: Option<TimerCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(true);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::Register {
            id,
            deadline,
            callback,
            on_cancel,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

pub(crate) fn cancel_timer(id: u64) {
    if id == 0 {
        return;
    }
    let reactor = reactor();
    if !reactor.request_cancel(id, true) {
        return;
    }
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::CancelTimer(id));
    reactor.wake.wake().expect("failed to wake Joky reactor");
}

/// Register a nonblocking TCP stream and report registration failures through
/// the supplied callback. The callback runs on the reactor thread and is
/// invoked only when `mio` cannot register the source; cancellation before the
/// command is processed simply drops it.
/// Test convenience wrapper over [`register_tcp_with_error`]; production
/// registration reports errors through the `_with_error` form.
#[cfg(test)]
pub(crate) fn register_tcp(
    stream: Arc<Mutex<TcpStream>>,
    interest: Interest,
    callback: TcpReadyCallback,
) -> u64 {
    register_tcp_with_error(stream, interest, callback, None)
}

pub(crate) fn register_tcp_with_error(
    stream: Arc<Mutex<TcpStream>>,
    interest: Interest,
    callback: TcpReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    register_tcp_dynamic(stream, interest, None, callback, on_error)
}

/// TLS changes direction during an operation; never poll writable while only
/// waiting for input, since a writable socket would otherwise busy-loop.
pub(crate) fn register_tcp_dynamic(
    stream: Arc<Mutex<TcpStream>>,
    interest: Interest,
    dynamic_interest: Option<Arc<Mutex<Interest>>>,
    callback: TcpReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterTcp {
            id,
            stream,
            interest,
            dynamic_interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

/// Register a nonblocking TCP listener and report registration failures to the
/// provider instead of leaving its continuation suspended indefinitely.
pub(crate) fn register_tcp_listener_with_error(
    listener: Arc<Mutex<TcpListener>>,
    interest: Interest,
    callback: TcpListenerReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterTcpListener {
            id,
            listener,
            interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

pub(crate) fn register_udp_with_error(
    socket: Arc<Mutex<UdpSocket>>,
    interest: Interest,
    callback: UdpReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterUdp {
            id,
            socket,
            interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

#[cfg(unix)]
pub(crate) fn register_unix_with_error(
    stream: Arc<Mutex<UnixStream>>,
    interest: Interest,
    callback: UnixReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterUnix {
            id,
            stream,
            interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

#[cfg(unix)]
pub(crate) fn register_unix_listener_with_error(
    listener: Arc<Mutex<UnixListener>>,
    interest: Interest,
    callback: UnixListenerReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterUnixListener {
            id,
            listener,
            interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

#[cfg(unix)]
pub(crate) fn register_unix_datagram_with_error(
    socket: Arc<Mutex<UnixDatagram>>,
    interest: Interest,
    callback: UnixDatagramReadyCallback,
    on_error: Option<TcpRegistrationErrorCallback>,
) -> u64 {
    let reactor = reactor();
    let id = reactor.reserve(false);
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::RegisterUnixDatagram {
            id,
            socket,
            interest,
            callback,
            on_error,
        });
    reactor.wake.wake().expect("failed to wake Joky reactor");
    id
}

fn finish_pending(reactor: &Reactor, id: u64) {
    let removed = reactor
        .requests
        .lock()
        .expect("reactor requests mutex")
        .remove(&id);
    debug_assert!(removed.is_some(), "reactor request finished twice");
    let previous = reactor.pending.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(previous > 0, "reactor pending operation underflow");
}

pub(crate) fn cancel_io(id: u64) {
    if id == 0 {
        return;
    }
    let reactor = reactor();
    if !reactor.request_cancel(id, false) {
        return;
    }
    reactor
        .commands
        .lock()
        .expect("reactor command mutex")
        .push_back(ReactorCommand::CancelIo(id));
    reactor.wake.wake().expect("failed to wake Joky reactor");
}

fn run(reactor: Arc<Reactor>, mut poll: Poll) {
    let mut events = Events::with_capacity(128);
    let mut timers = HashMap::<u64, TimerRegistration>::new();
    let mut tcp = HashMap::<u64, TcpRegistration>::new();
    let mut tcp_listeners = HashMap::<u64, TcpListenerRegistration>::new();
    let mut udp = HashMap::<u64, UdpRegistration>::new();
    #[cfg(unix)]
    let mut unix = HashMap::<u64, UnixRegistration>::new();
    #[cfg(unix)]
    let mut unix_listeners = HashMap::<u64, UnixListenerRegistration>::new();
    #[cfg(unix)]
    let mut unix_datagrams = HashMap::<u64, UnixDatagramRegistration>::new();
    let mut deadlines = BTreeSet::<(Instant, u64)>::new();
    loop {
        let mut callbacks = Vec::new();
        {
            let mut commands = reactor.commands.lock().expect("reactor command mutex");
            while let Some(command) = commands.pop_front() {
                match command {
                    #[cfg(any(test, feature = "test-support"))]
                    ReactorCommand::Snapshot(scope, reply) => {
                        let requests = reactor.requests.lock().unwrap();
                        let _ = reply.send(ResourceSnapshot {
                            scope_requests: requests
                                .values()
                                .filter(|r| scope.is_none_or(|id| r.scope == id))
                                .count(),
                            request_capacity: requests.capacity(),
                        });
                    }
                    ReactorCommand::Register {
                        id,
                        deadline,
                        callback,
                        on_cancel,
                    } => {
                        if reactor.is_cancelled(id) {
                            callbacks.push((id, on_cancel.unwrap_or(callback)));
                            continue;
                        }
                        deadlines.insert((deadline, id));
                        timers.insert(
                            id,
                            TimerRegistration {
                                deadline,
                                callback: Some(callback),
                                on_cancel,
                            },
                        );
                    }
                    ReactorCommand::RegisterTcp {
                        id,
                        stream,
                        interest,
                        dynamic_interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = stream
                            .lock()
                            .map_err(|_| Error::other("TCP stream mutex poisoned"))
                            .and_then(|mut stream| {
                                poll.registry().register(&mut *stream, token, interest)
                            });
                        if registered.is_ok() {
                            tcp.insert(
                                id,
                                TcpRegistration {
                                    stream,
                                    interest,
                                    dynamic_interest,
                                    callback,
                                    on_error,
                                },
                            );
                        } else {
                            // TLS cancellation/drop must poison the session before waking
                            // a caller that might immediately try another operation.
                            drop(callback);
                            if let Some(on_error) = on_error {
                                on_error(registered.expect_err("TCP registration failed"));
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    ReactorCommand::RegisterTcpListener {
                        id,
                        listener,
                        interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = listener
                            .lock()
                            .map_err(|_| Error::other("TCP listener mutex poisoned"))
                            .and_then(|mut listener| {
                                poll.registry().register(&mut *listener, token, interest)
                            });
                        if registered.is_ok() {
                            tcp_listeners.insert(
                                id,
                                TcpListenerRegistration {
                                    listener,
                                    interest,
                                    callback,
                                },
                            );
                        } else {
                            if let Some(on_error) = on_error {
                                on_error(registered.expect_err("TCP listener registration failed"));
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    ReactorCommand::RegisterUdp {
                        id,
                        socket,
                        interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = socket
                            .lock()
                            .map_err(|_| Error::other("UDP socket mutex poisoned"))
                            .and_then(|mut socket| {
                                poll.registry().register(&mut *socket, token, interest)
                            });
                        if registered.is_ok() {
                            udp.insert(
                                id,
                                UdpRegistration {
                                    socket,
                                    interest,
                                    callback,
                                },
                            );
                        } else {
                            if let Some(on_error) = on_error {
                                on_error(registered.expect_err("UDP registration failed"));
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    #[cfg(unix)]
                    ReactorCommand::RegisterUnix {
                        id,
                        stream,
                        interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = stream
                            .lock()
                            .map_err(|_| Error::other("Unix stream mutex poisoned"))
                            .and_then(|mut stream| {
                                poll.registry().register(&mut *stream, token, interest)
                            });
                        if registered.is_ok() {
                            unix.insert(
                                id,
                                UnixRegistration {
                                    stream,
                                    interest,
                                    callback,
                                },
                            );
                        } else {
                            if let Some(on_error) = on_error {
                                on_error(registered.expect_err("Unix registration failed"));
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    #[cfg(unix)]
                    ReactorCommand::RegisterUnixListener {
                        id,
                        listener,
                        interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = listener
                            .lock()
                            .map_err(|_| Error::other("Unix listener mutex poisoned"))
                            .and_then(|mut listener| {
                                poll.registry().register(&mut *listener, token, interest)
                            });
                        if registered.is_ok() {
                            unix_listeners.insert(
                                id,
                                UnixListenerRegistration {
                                    listener,
                                    interest,
                                    callback,
                                },
                            );
                        } else {
                            if let Some(on_error) = on_error {
                                on_error(
                                    registered.expect_err("Unix listener registration failed"),
                                );
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    #[cfg(unix)]
                    ReactorCommand::RegisterUnixDatagram {
                        id,
                        socket,
                        interest,
                        callback,
                        on_error,
                    } => {
                        if reactor.is_cancelled(id) {
                            finish_pending(&reactor, id);
                            continue;
                        }
                        let token = Token(id as usize);
                        let registered = socket
                            .lock()
                            .map_err(|_| Error::other("Unix datagram mutex poisoned"))
                            .and_then(|mut socket| {
                                poll.registry().register(&mut *socket, token, interest)
                            });
                        if registered.is_ok() {
                            unix_datagrams.insert(
                                id,
                                UnixDatagramRegistration {
                                    socket,
                                    interest,
                                    callback,
                                },
                            );
                        } else {
                            if let Some(on_error) = on_error {
                                on_error(
                                    registered.expect_err("Unix datagram registration failed"),
                                );
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                    ReactorCommand::CancelTimer(id) => {
                        if let Some(mut timer) = timers.remove(&id) {
                            deadlines.remove(&(timer.deadline, id));
                            if let Some(callback) =
                                timer.on_cancel.take().or_else(|| timer.callback.take())
                            {
                                callbacks.push((id, callback));
                            }
                        }
                    }
                    ReactorCommand::CancelIo(id) => {
                        if let Some(registration) = tcp.remove(&id) {
                            if let Ok(mut stream) = registration.stream.lock() {
                                let _ = poll.registry().deregister(&mut *stream);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                        if let Some(registration) = tcp_listeners.remove(&id) {
                            if let Ok(mut listener) = registration.listener.lock() {
                                let _ = poll.registry().deregister(&mut *listener);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                        #[cfg(unix)]
                        if let Some(registration) = unix.remove(&id) {
                            if let Ok(mut stream) = registration.stream.lock() {
                                let _ = poll.registry().deregister(&mut *stream);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                        #[cfg(unix)]
                        if let Some(registration) = unix_listeners.remove(&id) {
                            if let Ok(mut listener) = registration.listener.lock() {
                                let _ = poll.registry().deregister(&mut *listener);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                        #[cfg(unix)]
                        if let Some(registration) = unix_datagrams.remove(&id) {
                            if let Ok(mut socket) = registration.socket.lock() {
                                let _ = poll.registry().deregister(&mut *socket);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                        if let Some(registration) = udp.remove(&id) {
                            if let Ok(mut socket) = registration.socket.lock() {
                                let _ = poll.registry().deregister(&mut *socket);
                            }
                            finish_pending(&reactor, id);
                            continue;
                        }
                    }
                }
            }
        }
        for (id, callback) in callbacks {
            callback();
            finish_pending(&reactor, id);
        }

        let now = Instant::now();
        let mut due = Vec::new();
        while let Some((deadline, id)) = deadlines.first().copied() {
            if deadline > now {
                break;
            }
            deadlines.pop_first();
            if let Some(mut timer) = timers.remove(&id) {
                if let Some(callback) = timer.callback.take() {
                    due.push((id, callback));
                }
            }
        }
        for (id, callback) in due {
            callback();
            finish_pending(&reactor, id);
        }

        let wait_for = deadlines
            .first()
            .map(|(deadline, _)| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(100));
        // Poll is the reactor's sole blocking point. The Waker token makes
        // timer registration/cancellation (and future socket commands) wake
        // this thread without a second condition-variable path.
        let _ = poll.poll(&mut events, Some(wait_for));
        for event in events.iter() {
            if event.token() == WAKE_TOKEN {
                continue;
            }
            let id = event.token().0 as u64;
            if let Some(registration) = tcp.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.stream));
                if remove {
                    if let Some(registration) = tcp.remove(&id) {
                        if let Ok(mut stream) = registration.stream.lock() {
                            let _ = poll.registry().deregister(&mut *stream);
                        }
                        finish_pending(&reactor, id);
                    }
                } else {
                    if let Some(interest) = &registration.dynamic_interest {
                        registration.interest = *interest.lock().expect("TCP dynamic interest");
                    }
                    let result = registration
                        .stream
                        .lock()
                        .map_err(|_| Error::other("TCP stream mutex poisoned"))
                        .and_then(|mut stream| {
                            poll.registry().reregister(
                                &mut *stream,
                                Token(id as usize),
                                registration.interest,
                            )
                        });
                    if let Err(error) = result {
                        if let Some(mut registration) = tcp.remove(&id) {
                            if let Ok(mut stream) = registration.stream.lock() {
                                let _ = poll.registry().deregister(&mut *stream);
                            }
                            let on_error = registration.on_error.take();
                            drop(registration);
                            if let Some(on_error) = on_error {
                                on_error(error);
                            }
                            finish_pending(&reactor, id);
                        }
                    }
                }
                continue;
            }
            if let Some(registration) = tcp_listeners.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.listener));
                if remove {
                    if let Some(registration) = tcp_listeners.remove(&id) {
                        if let Ok(mut listener) = registration.listener.lock() {
                            let _ = poll.registry().deregister(&mut *listener);
                        }
                        finish_pending(&reactor, id);
                    }
                } else if let Ok(mut listener) = registration.listener.lock() {
                    let _ = poll.registry().reregister(
                        &mut *listener,
                        Token(id as usize),
                        registration.interest,
                    );
                }
                continue;
            }
            #[cfg(unix)]
            if let Some(registration) = unix.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.stream));
                if remove {
                    if let Some(registration) = unix.remove(&id) {
                        if let Ok(mut stream) = registration.stream.lock() {
                            let _ = poll.registry().deregister(&mut *stream);
                        }
                        finish_pending(&reactor, id);
                    }
                } else if let Ok(mut stream) = registration.stream.lock() {
                    let _ = poll.registry().reregister(
                        &mut *stream,
                        Token(id as usize),
                        registration.interest,
                    );
                }
                continue;
            }
            #[cfg(unix)]
            if let Some(registration) = unix_listeners.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.listener));
                if remove {
                    if let Some(registration) = unix_listeners.remove(&id) {
                        if let Ok(mut listener) = registration.listener.lock() {
                            let _ = poll.registry().deregister(&mut *listener);
                        }
                        finish_pending(&reactor, id);
                    }
                } else if let Ok(mut listener) = registration.listener.lock() {
                    let _ = poll.registry().reregister(
                        &mut *listener,
                        Token(id as usize),
                        registration.interest,
                    );
                }
                continue;
            }
            #[cfg(unix)]
            if let Some(registration) = unix_datagrams.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.socket));
                if remove {
                    if let Some(registration) = unix_datagrams.remove(&id) {
                        if let Ok(mut socket) = registration.socket.lock() {
                            let _ = poll.registry().deregister(&mut *socket);
                        }
                        finish_pending(&reactor, id);
                    }
                } else if let Ok(mut socket) = registration.socket.lock() {
                    let _ = poll.registry().reregister(
                        &mut *socket,
                        Token(id as usize),
                        registration.interest,
                    );
                }
                continue;
            }
            if let Some(registration) = udp.get_mut(&id) {
                let remove =
                    (registration.callback)(event.clone(), Arc::clone(&registration.socket));
                if remove {
                    if let Some(registration) = udp.remove(&id) {
                        if let Ok(mut socket) = registration.socket.lock() {
                            let _ = poll.registry().deregister(&mut *socket);
                        }
                        finish_pending(&reactor, id);
                    }
                } else if let Ok(mut socket) = registration.socket.lock() {
                    let _ = poll.registry().reregister(
                        &mut *socket,
                        Token(id as usize),
                        registration.interest,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub(crate) struct ResourceSnapshot {
    pub scope_requests: usize,
    pub request_capacity: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn resource_snapshot(scope: Option<crate::runtime::scope::ScopeId>) -> ResourceSnapshot {
    let Some(reactor) = REACTOR.get() else {
        return ResourceSnapshot::default();
    };
    let (send, receive) = std::sync::mpsc::channel();
    reactor
        .commands
        .lock()
        .unwrap()
        .push_back(ReactorCommand::Snapshot(scope, send));
    reactor.wake.wake().unwrap();
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("reactor snapshot timed out")
}
