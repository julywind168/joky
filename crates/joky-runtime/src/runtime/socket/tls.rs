//! Verified client TLS. Socket ownership transfers on upgrade; one operation
//! owns the TLS machine at a time. No blocking socket I/O or network threads.
use super::*;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};

const MAX_IO: usize = 16 * 1024 * 1024;
const MAX_CA: usize = 1024 * 1024;

pub(super) struct Session {
    pub(super) stream: Stream,
    connection: ClientConnection,
    busy: bool,
    failed: bool,
}

enum Work {
    Handshake,
    Read(usize),
    Write { data: Vec<u8>, offset: usize },
    Close,
}
enum Output {
    Handshake,
    Bytes(Vec<u8>),
    Written(usize),
    Closed,
}
enum Progress {
    Wait(Interest),
    Done(Output),
}

// Dropping an unfinished readiness callback is cancellation. Poison and shut
// down the connection: replaying a partly sent TLS operation is not safe.
struct Operation {
    session: Arc<Mutex<Session>>,
    work: Work,
    finished: bool,
}
impl Drop for Operation {
    fn drop(&mut self) {
        let mut session = self.session.lock().expect("TLS session");
        if !self.finished {
            session.busy = false;
            session.failed = true;
            if let Ok(stream) = session.stream.lock() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

fn config(ca: &[u8]) -> Result<Arc<ClientConfig>, String> {
    if ca.len() > MAX_CA {
        return Err("TLS: CA bundle limit exceeded".into());
    }
    let mut roots = RootCertStore::empty();
    if ca.is_empty() {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    } else {
        for cert in rustls_pemfile::certs(&mut &*ca) {
            roots
                .add(cert.map_err(|_| "TLS: invalid CA PEM")?)
                .map_err(|_| "TLS: invalid CA certificate")?;
        }
        if roots.is_empty() {
            return Err("TLS: CA bundle contains no certificates".into());
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.enable_early_data = false;
    // Each connection verifies its certificate; no shared resumption cache.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

impl Operation {
    fn fail(&mut self) {
        let mut state = self.session.lock().expect("TLS session");
        state.failed = true;
        state.busy = false;
        let _ = state
            .stream
            .lock()
            .expect("TLS socket")
            .shutdown(std::net::Shutdown::Both);
        self.finished = true;
    }

    fn drive(&mut self) -> Result<Progress, String> {
        let mut state = self.session.lock().map_err(|_| "TLS: poisoned session")?;
        if state.failed {
            return Err("TLS: connection is unusable".into());
        }
        let socket = Arc::clone(&state.stream);
        let mut stream = socket.lock().map_err(|_| "TLS: poisoned socket")?;
        let conn = &mut state.connection;
        // Bound each reactor turn, including application writes.
        for _ in 0..64 {
            while conn.wants_write() {
                match conn.write_tls(&mut *stream) {
                    Ok(0) => return Err("TLS: transport write returned zero".into()),
                    Ok(_) => (),
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        return Ok(Progress::Wait(Interest::WRITABLE))
                    }
                    Err(e) => return Err(format!("TLS: {e}")),
                }
            }
            match &mut self.work {
                Work::Handshake if !conn.is_handshaking() => {
                    return Ok(Progress::Done(Output::Handshake))
                }
                Work::Read(max) => {
                    let mut data = vec![0; (*max).min(65536)];
                    match conn.reader().read(&mut data) {
                        Ok(length) => {
                            data.truncate(length);
                            return Ok(Progress::Done(Output::Bytes(data)));
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => (),
                        Err(e) => return Err(format!("TLS: {e}")),
                    }
                }
                Work::Write { data, offset } => {
                    if *offset == data.len() {
                        return Ok(Progress::Done(Output::Written(*offset)));
                    }
                    let end = (*offset + 16384).min(data.len());
                    let written = conn
                        .writer()
                        .write(&data[*offset..end])
                        .map_err(|e| format!("TLS: {e}"))?;
                    if written == 0 {
                        return Err("TLS: plaintext write returned zero".into());
                    }
                    *offset += written;
                    continue;
                }
                Work::Close => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return Ok(Progress::Done(Output::Closed));
                }
                Work::Handshake => (),
            }
            match conn.read_tls(&mut *stream) {
                Ok(0) => return Err("TLS: unexpected transport EOF (missing close_notify)".into()),
                Ok(_) => {
                    conn.process_new_packets()
                        .map_err(|e| format!("TLS: {e}"))?;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    return Ok(Progress::Wait(Interest::READABLE))
                }
                Err(e) => return Err(format!("TLS: {e}")),
            }
        }
        // Budget yield, not a blocked read: buffered plaintext or a completed
        // handshake may already be ready. Schedule another turn even without
        // a new readable edge. Real WouldBlock paths select their own interest.
        Ok(Progress::Wait(Interest::WRITABLE))
    }
}

fn publish(
    op: &mut Operation,
    result: Result<Output, String>,
    continuation: &Continuation,
    handle: usize,
    token: u64,
    socket_id: Option<u64>,
    words: usize,
) {
    // Drop operation locks before waking code that may immediately close.
    let output = match result {
        Ok(value) => value,
        Err(error) => {
            op.fail();
            finish_error(
                continuation,
                handle as *mut Continuation,
                token,
                words,
                error,
            );
            return;
        }
    };
    let mut owned = None;
    let payload = match output {
        Output::Handshake => {
            let id = insert_resource(
                SocketResource::Tls(Arc::clone(&op.session)),
                continuation.scope_id(),
            );
            let Some(value) = allocate_socket_resource(id) else {
                op.fail();
                sockets().lock().expect("socket map").remove(&id);
                finish_error(
                    continuation,
                    handle as *mut Continuation,
                    token,
                    words,
                    "TLS: allocation failed",
                );
                return;
            };
            owned = Some(value);
            vec![0, value as usize, 0, 0]
        }
        Output::Bytes(bytes) => {
            let value = super::super::bytes::jk_bytes_from_data(bytes.as_ptr(), bytes.len());
            if value.is_null() {
                op.fail();
                finish_error(
                    continuation,
                    handle as *mut Continuation,
                    token,
                    words,
                    "TLS: allocation failed",
                );
                return;
            }
            owned = Some(value);
            vec![0, value as usize, 0, 0]
        }
        Output::Written(length) => vec![0, length, 0, 0],
        Output::Closed => {
            if let Some(id) = socket_id {
                sockets().lock().expect("socket map").remove(&id);
            }
            vec![0, 0, 0]
        }
    };
    op.finished = true;
    op.session.lock().expect("TLS session").busy = false;
    if !continuation.complete_suspend_with_payload(
        handle as *mut Continuation,
        token,
        payload.as_ptr().cast(),
        words * 8,
    ) {
        op.finished = false;
        if let Some(value) = owned {
            jk_drop(value);
        }
    }
}

fn start(
    mut op: Operation,
    continuation: Continuation,
    handle: usize,
    token: u64,
    socket: Option<u64>,
    words: usize,
) {
    let interest = match op.drive() {
        Ok(Progress::Wait(interest)) => interest,
        Ok(Progress::Done(value)) => {
            publish(
                &mut op,
                Ok(value),
                &continuation,
                handle,
                token,
                socket,
                words,
            );
            return;
        }
        Err(error) => {
            publish(
                &mut op,
                Err(error),
                &continuation,
                handle,
                token,
                socket,
                words,
            );
            return;
        }
    };
    let stream = Arc::clone(&op.session.lock().expect("TLS session").stream);
    let dynamic = Arc::new(Mutex::new(interest));
    let next_interest = Arc::clone(&dynamic);
    let callback_continuation = continuation.clone();
    let scope = continuation.scope_id();
    let io = super::super::reactor::register_tcp_dynamic(
        stream,
        interest,
        Some(dynamic),
        Box::new(move |_, _| match op.drive() {
            Ok(Progress::Wait(interest)) => {
                *next_interest.lock().expect("TLS interest") = interest;
                false
            }
            result => {
                publish(
                    &mut op,
                    result.map(|progress| match progress {
                        Progress::Done(value) => value,
                        _ => unreachable!(),
                    }),
                    &callback_continuation,
                    handle,
                    token,
                    socket,
                    words,
                );
                forget_pending(scope, handle, token);
                true
            }
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle,
            token,
            words,
        )),
    );
    remember_pending(
        handle as *mut Continuation,
        token,
        io,
        socket,
        words,
        &continuation,
    );
}

unsafe fn bytes_arg(arguments: *const u8, index: usize) -> Option<Vec<u8>> {
    let value = read_word(arguments, index, 8)? as *mut u8;
    if value.is_null() {
        return None;
    }
    let len = jk_bytes_length(value);
    if len > MAX_IO {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    let data = jk_bytes_data(value);
    if data.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(data, len).to_vec())
}

pub(super) unsafe extern "C" fn upgrade_start(
    handle: *mut Continuation,
    token: u64,
    args: *const u8,
    size: usize,
    _: *mut u8,
    result_size: usize,
) -> u8 {
    if size != 32 || result_size != 32 {
        return 0;
    }
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let result = (|| {
        let id = read_word(args, 0, 8)
            .and_then(socket_id_from_value)
            .ok_or("TLS: invalid TCP handle")?;
        let resource = {
            let mut entries = sockets().lock().expect("socket map");
            if !entries.get(&id).is_some_and(|entry| {
                entry.scope_id == continuation.scope_id()
                    && matches!(entry.resource, SocketResource::Stream(_))
            }) {
                return Err("TLS: upgrade requires a TCP stream".to_owned());
            }
            entries.remove(&id).expect("verified socket").resource
        };
        cancel_socket_pending(continuation.scope_id(), id);
        let SocketResource::Stream(stream) = resource else {
            unreachable!()
        };
        let name = read_word(args, 1, 8).ok_or("TLS: invalid server name")? as *mut u8;
        let len = read_word(args, 2, 8).ok_or("TLS: invalid server name")?;
        if name.is_null() || jk_string_len(name) != len || len > 253 {
            return Err("TLS: invalid server name".into());
        }
        let name = std::str::from_utf8(std::slice::from_raw_parts(name, len))
            .map_err(|_| "TLS: invalid server name")?;
        let name = ServerName::try_from(name.to_owned()).map_err(|_| "TLS: invalid server name")?;
        let ca = bytes_arg(args, 3).ok_or("TLS: invalid CA bundle")?;
        let mut connection =
            ClientConnection::new(config(&ca)?, name).map_err(|e| format!("TLS: {e}"))?;
        connection.set_buffer_limit(Some(65536));
        Ok(Operation {
            session: Arc::new(Mutex::new(Session {
                stream,
                connection,
                busy: true,
                failed: false,
            })),
            work: Work::Handshake,
            finished: false,
        })
    })();
    match result {
        Ok(op) => start(op, continuation, handle as usize, token, None, 4),
        Err(error) => finish_error(&continuation, handle, token, 4, error),
    }
    1
}

unsafe fn operate(
    handle: *mut Continuation,
    token: u64,
    args: *const u8,
    size: usize,
    result_size: usize,
    kind: u8,
) -> u8 {
    let words = if kind == 2 { 3 } else { 4 };
    if size != if kind == 2 { 8 } else { 16 } || result_size != words * 8 {
        return 0;
    }
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let result = (|| {
        let id = read_word(args, 0, 8)
            .and_then(socket_id_from_value)
            .ok_or("TLS: invalid handle")?;
        let session = {
            let entries = sockets().lock().expect("socket map");
            match entries.get(&id) {
                Some(entry) if entry.scope_id == continuation.scope_id() => match &entry.resource {
                    SocketResource::Tls(session) => Arc::clone(session),
                    _ => return Err("TLS: invalid handle"),
                },
                _ => return Err("TLS: invalid handle"),
            }
        };
        let work = match kind {
            0 => {
                let max = read_word(args, 1, 8).ok_or("TLS: invalid read limit")?;
                if max > MAX_IO {
                    return Err("TLS: read limit exceeded");
                }
                Work::Read(max)
            }
            1 => Work::Write {
                data: bytes_arg(args, 1).ok_or("TLS: write limit exceeded")?,
                offset: 0,
            },
            _ => Work::Close,
        };
        {
            let mut state = session.lock().expect("TLS session");
            if state.busy {
                return Err("TLS: concurrent operation is unsupported");
            }
            if state.failed {
                return Err("TLS: connection is unusable");
            }
            state.busy = true;
            if kind == 2 {
                state.connection.send_close_notify();
            }
        }
        Ok((
            id,
            Operation {
                session,
                work,
                finished: false,
            },
        ))
    })();
    match result {
        Ok((id, op)) => start(op, continuation, handle as usize, token, Some(id), words),
        Err(error) => finish_error(&continuation, handle, token, words, error),
    }
    1
}
pub(super) unsafe extern "C" fn read_start(
    h: *mut Continuation,
    t: u64,
    a: *const u8,
    s: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    operate(h, t, a, s, r, 0)
}
pub(super) unsafe extern "C" fn write_start(
    h: *mut Continuation,
    t: u64,
    a: *const u8,
    s: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    operate(h, t, a, s, r, 1)
}
pub(super) unsafe extern "C" fn close_start(
    h: *mut Continuation,
    t: u64,
    a: *const u8,
    s: usize,
    _: *mut u8,
    r: usize,
) -> u8 {
    operate(h, t, a, s, r, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Arc<Mutex<Session>>, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        client.set_nonblocking(true).unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let connection = ClientConnection::new(
            config(&[]).unwrap(),
            ServerName::try_from("localhost").unwrap(),
        )
        .unwrap();
        (
            Arc::new(Mutex::new(Session {
                stream: Arc::new(Mutex::new(TcpStream::from_std(client))),
                connection,
                busy: true,
                failed: false,
            })),
            peer,
        )
    }

    #[test]
    fn completed_operation_drop_cannot_clear_a_successors_busy_flag() {
        let (session, _peer) = session();
        let operation = Operation {
            session: session.clone(),
            work: Work::Handshake,
            finished: true,
        };
        // A completed callback can be dropped after its successor has started.
        drop(operation);
        let state = session.lock().unwrap();
        assert!(state.busy);
        assert!(!state.failed);
    }

    #[test]
    fn cancellation_and_error_poison_and_shutdown_before_reuse() {
        for cancelled in [false, true] {
            let (session, mut peer) = session();
            let mut operation = Operation {
                session: session.clone(),
                work: Work::Handshake,
                finished: false,
            };
            if !cancelled {
                operation.fail();
            }
            drop(operation);
            let state = session.lock().unwrap();
            assert!(state.failed);
            assert!(!state.busy);
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn roots_validate_empty_malformed_and_oversized_bundles() {
        assert!(config(&[]).is_ok());
        for invalid in [
            b"not a PEM".as_slice(),
            b"-----BEGIN CERTIFICATE-----\n???\n-----END CERTIFICATE-----",
        ] {
            assert!(config(invalid).is_err());
        }
        assert!(config(&vec![0; MAX_CA + 1]).unwrap_err().contains("limit"));
    }
}
