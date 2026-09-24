//! Socket resource tables, payload helpers, and pending I/O bookkeeping.

use super::*;
#[cfg(unix)]
use std::path::PathBuf;

pub(super) type Stream = Arc<Mutex<TcpStream>>;
pub(super) type Listener = Arc<Mutex<TcpListener>>;
pub(super) type Datagram = Arc<Mutex<UdpSocket>>;
#[cfg(unix)]
pub(super) type UnixStreamHandle = Arc<Mutex<UnixStream>>;
#[cfg(unix)]
pub(super) type UnixListenerHandle = Arc<Mutex<UnixListener>>;
#[cfg(unix)]
pub(super) type UnixDatagramHandle = Arc<Mutex<UnixDatagram>>;
pub(super) type PendingKey = (crate::runtime::scope::ScopeId, usize, u64);

pub(super) enum SocketResource {
    Stream(Stream),
    ReadHalf(Stream),
    WriteHalf(Stream),
    Listener(Listener),
    Datagram(Datagram),
    #[cfg(unix)]
    UnixStream(UnixStreamHandle),
    #[cfg(unix)]
    UnixListener {
        socket: UnixListenerHandle,
        path: PathBuf,
    },
    #[cfg(unix)]
    UnixDatagram {
        socket: UnixDatagramHandle,
        path: PathBuf,
    },
}

pub(super) struct SocketEntry {
    pub(super) scope_id: crate::runtime::scope::ScopeId,
    pub(super) resource: SocketResource,
}

pub(super) struct Pending {
    pub(super) scope_id: crate::runtime::scope::ScopeId,
    pub(super) io_id: u64,
    pub(super) socket: Option<u64>,
    pub(super) error_words: usize,
    pub(super) continuation: Continuation,
}

pub(super) fn cleanup_socket_resource(resource: SocketResource) {
    match resource {
        SocketResource::ReadHalf(stream) => {
            if let Ok(stream) = stream.lock() {
                let _ = stream.shutdown(std::net::Shutdown::Read);
            }
        }
        SocketResource::WriteHalf(stream) => {
            if let Ok(stream) = stream.lock() {
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        }
        #[cfg(unix)]
        SocketResource::UnixListener { path, .. } | SocketResource::UnixDatagram { path, .. } => {
            let _ = std::fs::remove_file(path);
        }
        _ => {}
    }
}

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);
static SOCKETS: OnceLock<Mutex<HashMap<u64, SocketEntry>>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashMap<PendingKey, Pending>>> = OnceLock::new();

pub(super) fn sockets() -> &'static Mutex<HashMap<u64, SocketEntry>> {
    SOCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn pending() -> &'static Mutex<HashMap<PendingKey, Pending>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn read_word(arguments: *const u8, index: usize, size: usize) -> Option<usize> {
    if arguments.is_null() {
        return None;
    }
    let offset = index.checked_mul(std::mem::size_of::<usize>())?;
    offset.checked_add(size)?;
    Some(unsafe { std::ptr::read_unaligned(arguments.add(offset).cast::<usize>()) })
}

pub(super) fn read_u16(arguments: *const u8, index: usize) -> Option<u16> {
    if arguments.is_null() {
        return None;
    }
    let offset = index.checked_mul(8)?;
    Some(unsafe { std::ptr::read_unaligned(arguments.add(offset).cast::<u16>()) })
}

pub(super) fn error_payload(words: usize, message: impl AsRef<[u8]>) -> Option<Vec<usize>> {
    let bytes = message.as_ref();
    let pointer = jk_string_from_utf8(bytes.as_ptr(), bytes.len());
    (!pointer.is_null()).then(|| {
        let mut payload = vec![0_usize; words];
        payload[0] = 1;
        payload[words - 2] = pointer as usize;
        payload[words - 1] = bytes.len();
        payload
    })
}

pub(super) fn finish_error(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    words: usize,
    message: impl AsRef<[u8]>,
) {
    let Some(payload) = error_payload(words, message) else {
        return;
    };
    let completed = continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        payload.len() * std::mem::size_of::<usize>(),
    );
    if !completed {
        let pointer = payload[words - 2] as *mut u8;
        jk_drop(pointer);
    }
}

pub(super) fn remember_pending(
    handle: *mut Continuation,
    operation: u64,
    io_id: u64,
    socket: Option<u64>,
    error_words: usize,
    continuation: &Continuation,
) {
    let key = (continuation.scope_id(), handle as usize, operation);
    pending().lock().expect("socket pending mutex").insert(
        key,
        Pending {
            scope_id: continuation.scope_id(),
            io_id,
            socket,
            error_words,
            continuation: continuation.clone(),
        },
    );

    // The reactor can observe an already-ready descriptor before this start
    // hook gets back from `register_tcp`. Likewise, task cancellation can win
    // the race before the registration is recorded. Remove a stale entry in
    // either case; only cancellation needs to disarm a still-queued reactor
    // registration.
    if !continuation.is_suspend_pending(operation) {
        let removed = pending()
            .lock()
            .expect("socket pending mutex")
            .remove(&key)
            .is_some();
        if removed && continuation.is_cancelled() {
            cancel_io(io_id);
        }
    }
}

pub(super) fn cancel_socket_pending(scope_id: crate::runtime::scope::ScopeId, socket: u64) {
    let pending = {
        let mut entries = pending().lock().expect("socket pending mutex");
        let keys = entries
            .iter()
            .filter_map(|(key, pending)| {
                (pending.scope_id == scope_id && pending.socket == Some(socket)).then_some(*key)
            })
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| entries.remove(&key).map(|pending| (key, pending)))
            .collect::<Vec<_>>()
    };
    for ((_, handle, operation), pending) in pending {
        cancel_io(pending.io_id);
        // A close is an operation-level cancellation, rather than structured
        // task cancellation. Wake every pending read/write with an ordinary
        // error payload so a sibling task cannot remain suspended forever.
        finish_error(
            &pending.continuation,
            handle as *mut Continuation,
            operation,
            pending.error_words,
            "socket closed",
        );
    }
}

pub(super) fn forget_pending(
    scope_id: crate::runtime::scope::ScopeId,
    handle: usize,
    operation: u64,
) {
    pending()
        .lock()
        .expect("socket pending mutex")
        .remove(&(scope_id, handle, operation));
}

pub(super) fn registration_error_callback(
    continuation: Continuation,
    handle: usize,
    operation: u64,
    words: usize,
) -> Box<dyn FnOnce(std::io::Error) + Send + 'static> {
    Box::new(move |error| {
        finish_error(
            &continuation,
            handle as *mut Continuation,
            operation,
            words,
            error.to_string(),
        );
        forget_pending(continuation.scope_id(), handle, operation);
    })
}

pub(super) fn socket_id_from_value(value: usize) -> Option<u64> {
    if value == 0 {
        return None;
    }
    let pointer = value as *mut u8;
    let header = unsafe { valid_header(pointer) }?;
    if RuntimeValueKind::from_raw(header.kind) != Some(RuntimeValueKind::NativeHandle)
        || header.payload_size < std::mem::size_of::<u64>()
    {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(pointer.cast::<u64>()) })
}

pub(super) fn stream_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<Stream> {
    sockets()
        .lock()
        .ok()
        .and_then(|sockets| match sockets.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::Stream(stream)
                | SocketResource::ReadHalf(stream)
                | SocketResource::WriteHalf(stream) => Some(Arc::clone(stream)),
                _ => None,
            },
            _ => None,
        })
}

pub(super) fn listener_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<Listener> {
    sockets()
        .lock()
        .ok()
        .and_then(|sockets| match sockets.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::Listener(listener) => Some(Arc::clone(listener)),
                _ => None,
            },
            _ => None,
        })
}

pub(super) fn datagram_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<Datagram> {
    sockets()
        .lock()
        .ok()
        .and_then(|entries| match entries.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::Datagram(socket) => Some(Arc::clone(socket)),
                _ => None,
            },
            _ => None,
        })
}

#[cfg(unix)]
pub(super) fn unix_stream_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<UnixStreamHandle> {
    sockets()
        .lock()
        .ok()
        .and_then(|entries| match entries.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::UnixStream(stream) => Some(Arc::clone(stream)),
                _ => None,
            },
            _ => None,
        })
}

#[cfg(unix)]
pub(super) fn unix_listener_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<UnixListenerHandle> {
    sockets()
        .lock()
        .ok()
        .and_then(|entries| match entries.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::UnixListener { socket, .. } => Some(Arc::clone(socket)),
                _ => None,
            },
            _ => None,
        })
}

#[cfg(unix)]
pub(super) fn unix_datagram_for_socket(
    socket_id: u64,
    scope_id: crate::runtime::scope::ScopeId,
) -> Option<UnixDatagramHandle> {
    sockets()
        .lock()
        .ok()
        .and_then(|entries| match entries.get(&socket_id) {
            Some(entry) if entry.scope_id == scope_id => match &entry.resource {
                SocketResource::UnixDatagram { socket, .. } => Some(Arc::clone(socket)),
                _ => None,
            },
            _ => None,
        })
}

pub(super) fn insert_stream(stream: Stream, scope_id: crate::runtime::scope::ScopeId) -> u64 {
    insert_resource(SocketResource::Stream(stream), scope_id)
}

pub(super) fn insert_resource(
    resource: SocketResource,
    scope_id: crate::runtime::scope::ScopeId,
) -> u64 {
    let socket = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets()
        .lock()
        .expect("socket map mutex")
        .insert(socket, SocketEntry { scope_id, resource });
    socket
}

pub(super) fn insert_listener(listener: Listener, scope_id: crate::runtime::scope::ScopeId) -> u64 {
    let socket = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets().lock().expect("socket map mutex").insert(
        socket,
        SocketEntry {
            scope_id,
            resource: SocketResource::Listener(listener),
        },
    );
    socket
}

pub(super) fn insert_datagram(socket: Datagram, scope_id: crate::runtime::scope::ScopeId) -> u64 {
    let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets().lock().expect("socket map mutex").insert(
        id,
        SocketEntry {
            scope_id,
            resource: SocketResource::Datagram(socket),
        },
    );
    id
}

#[cfg(unix)]
pub(super) fn insert_unix_stream(
    stream: UnixStreamHandle,
    scope_id: crate::runtime::scope::ScopeId,
) -> u64 {
    let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets().lock().expect("socket map mutex").insert(
        id,
        SocketEntry {
            scope_id,
            resource: SocketResource::UnixStream(stream),
        },
    );
    id
}

#[cfg(unix)]
pub(super) fn insert_unix_listener(
    listener: UnixListenerHandle,
    path: PathBuf,
    scope_id: crate::runtime::scope::ScopeId,
) -> u64 {
    let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets().lock().expect("socket map mutex").insert(
        id,
        SocketEntry {
            scope_id,
            resource: SocketResource::UnixListener {
                socket: listener,
                path,
            },
        },
    );
    id
}

#[cfg(unix)]
pub(super) fn insert_unix_datagram(
    socket: UnixDatagramHandle,
    path: PathBuf,
    scope_id: crate::runtime::scope::ScopeId,
) -> u64 {
    let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    sockets().lock().expect("socket map mutex").insert(
        id,
        SocketEntry {
            scope_id,
            resource: SocketResource::UnixDatagram { socket, path },
        },
    );
    id
}

#[cfg(unix)]
pub(super) fn unix_path_from_args(arguments: *const u8, index: usize) -> Option<PathBuf> {
    let pointer = read_word(arguments, index, 8).map(|pointer| pointer as *mut u8)?;
    let length = read_word(arguments, index + 1, 8)?;
    if jk_string_len(pointer) != length {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
    let path = std::str::from_utf8(bytes).ok()?;
    Some(PathBuf::from(path))
}
