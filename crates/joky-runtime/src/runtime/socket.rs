//! Nonblocking stream and datagram socket providers.
//!
//! Socket state is kept outside language values. A stream resource can own
//! either a connected stream or a listening socket. The provider copies byte
//! arguments before returning from the suspend start hook and publishes only
//! flattened result payloads to continuations.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{ErrorKind, Read, Write};
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mio::net::{TcpListener, TcpStream, UdpSocket};
#[cfg(unix)]
use mio::net::{UnixDatagram, UnixListener, UnixStream};
use mio::Interest;

use super::bytes::{jk_bytes_data, jk_bytes_length};
use super::continuation::{jk_continuation_complete_suspend_with_payload, Continuation};
use super::managed::{jk_alloc_native_handle, jk_drop, valid_header, RuntimeValueKind};
use super::reactor::{
    cancel_io, register_tcp_listener_with_error, register_tcp_with_error, register_udp_with_error,
};
#[cfg(unix)]
use super::reactor::{
    register_unix_datagram_with_error, register_unix_listener_with_error, register_unix_with_error,
};
use super::string::{jk_string_from_utf8, jk_string_len};
mod resources;
mod udp;
#[cfg(unix)]
mod unix;

use resources::*;
pub(crate) use udp::*;
#[cfg(unix)]
pub(crate) use unix::*;

use super::provider::ProviderScope;

/// `(effect, name)`-keyed hook table: the runtime's side of the registration
/// contract. Socket effects share operation names (`close`, `read`), so the
/// effect qualifies each entry.
fn operation_hooks() -> Vec<(
    (&'static str, &'static str),
    crate::runtime::provider::ProviderStart,
)> {
    let mut hooks = Vec::new();
    let mut push =
        |effect: &'static str,
         table: &[(&'static str, crate::runtime::provider::ProviderStart)]| {
            for (name, start) in table {
                hooks.push(((effect, *name), *start));
            }
        };
    push(
        "tcp",
        &[
            ("connect", socket_connect_start),
            ("listen", socket_listen_start),
            ("accept", socket_accept_start),
            ("split", socket_split_start),
            ("read_half", socket_read_start),
            ("write_half", socket_write_start),
            ("close_read", socket_close_start),
            ("close_write", socket_close_start),
            ("read", socket_read_start),
            ("write", socket_write_start),
            ("close", socket_close_start),
        ],
    );
    push(
        "udp",
        &[
            ("bind", socket_udp_bind_start),
            ("connect", socket_udp_connect_start),
            ("send_to", socket_udp_send_to_start),
            ("send", socket_udp_send_start),
            ("recv", socket_udp_recv_start),
            ("recv_from", socket_udp_recv_from_start),
            ("close", socket_udp_close_start),
        ],
    );
    #[cfg(unix)]
    push(
        "unix",
        &[
            ("connect", socket_unix_connect_start),
            ("listen", socket_unix_listen_start),
            ("accept", socket_unix_accept_start),
            ("read", socket_unix_read_start),
            ("write", socket_unix_write_start),
            ("close", socket_unix_close_start),
        ],
    );
    #[cfg(unix)]
    push(
        "unix_dgram",
        &[
            ("bind", socket_unix_dgram_bind_start),
            ("send_to", socket_unix_dgram_send_to_start),
            ("recv_from", socket_unix_dgram_recv_from_start),
            ("close", socket_unix_dgram_close_start),
        ],
    );
    hooks
}

/// Register socket hooks for `(effect, name, operation_id)` entries derived
/// from the checked effect declarations. Unknown names are ignored.
pub(crate) fn register_operations(
    entries: &[crate::runtime::provider::ProviderOperationEntry],
) -> Option<ProviderScope> {
    let scope = crate::runtime::scope::current_or_default();
    let mut registration = ProviderScope::new(Arc::clone(&scope));
    let hooks = operation_hooks();
    for entry in entries {
        if let Some(&(_, start)) = hooks
            .iter()
            .find(|((effect, name), _)| *effect == entry.effect && *name == entry.name)
        {
            if !registration.register_operation(
                entry.operation,
                start as *mut c_void,
                socket_cancel as *mut c_void,
            ) {
                return None;
            }
        }
    }
    if registration.is_empty() {
        return None;
    }
    let scope_id = registration.scope_id();
    registration.on_shutdown(move || {
        let socket_entries = {
            let mut entries = sockets().lock().expect("socket map mutex");
            let ids = entries
                .iter()
                .filter_map(|(socket, entry)| (entry.scope_id == scope_id).then_some(*socket))
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|socket| entries.remove(&socket).map(|entry| (socket, entry)))
                .collect::<Vec<_>>()
        };
        for (socket, entry) in socket_entries {
            cancel_socket_pending(scope_id, socket);
            cleanup_socket_resource(entry.resource);
        }
        let io_ids = {
            let mut entries = pending().lock().expect("socket pending mutex");
            entries
                .extract_if(|_, pending| pending.scope_id == scope_id)
                .map(|(_, pending)| pending.io_id)
                .collect::<Vec<_>>()
        };
        for io_id in io_ids {
            cancel_io(io_id);
        }
    });
    Some(registration)
}

fn allocate_socket_resource(socket: u64) -> Option<*mut u8> {
    let resource = jk_alloc_native_handle(
        std::mem::size_of::<u64>(),
        std::mem::align_of::<u64>(),
        Some(socket_drop_callback),
    );
    if resource.is_null() {
        return None;
    }
    unsafe { std::ptr::write_unaligned(resource.cast::<u64>(), socket) };
    Some(resource)
}

unsafe extern "C" fn socket_drop_callback(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    let socket_id = std::ptr::read_unaligned(payload.cast::<u64>());
    let entry = sockets()
        .lock()
        .expect("socket map mutex")
        .remove(&socket_id);
    if let Some(entry) = entry {
        let scope_id = entry.scope_id;
        cancel_socket_pending(scope_id, socket_id);
        cleanup_socket_resource(entry.resource);
    }
}

unsafe extern "C" fn socket_cancel(handle: *mut Continuation, operation: u64) -> u8 {
    let handle_address = handle as usize;
    let key = pending()
        .lock()
        .expect("socket pending mutex")
        .keys()
        .find(|(_, address, token)| *address == handle_address && *token == operation)
        .copied();
    let Some(key) = key else {
        return 0;
    };
    let Some(pending) = pending().lock().expect("socket pending mutex").remove(&key) else {
        return 0;
    };
    cancel_io(pending.io_id);
    1
}

unsafe extern "C" fn socket_connect_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 3 * std::mem::size_of::<usize>()
        || result_size != 4 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let host =
        read_word(arguments, 0, std::mem::size_of::<usize>()).map(|pointer| pointer as *mut u8);
    let length = read_word(arguments, 1, std::mem::size_of::<usize>()).unwrap_or(0);
    let port = read_u16(arguments, 2).unwrap_or(0);
    let Some(host) = host else { return 0 };
    if jk_string_len(host) != length {
        return 0;
    }
    let host = unsafe { std::slice::from_raw_parts(host, length) };
    let Ok(host) = std::str::from_utf8(host) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let handle_address = handle as usize;
    let Some(address) = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
    else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "could not resolve socket address",
        );
        return 1;
    };
    let stream = match TcpStream::connect(address) {
        Ok(stream) => stream,
        Err(error) => {
            finish_error(&continuation, handle, operation, 4, error.to_string());
            return 1;
        }
    };
    let stream = Arc::new(Mutex::new(stream));
    let stream_for_callback = Arc::clone(&stream);
    let continuation_for_callback = continuation.clone();
    let scope_id = continuation.scope_id();
    let io_id = register_tcp_with_error(
        Arc::clone(&stream),
        Interest::WRITABLE,
        Box::new(move |_event, stream| {
            let result = stream
                .lock()
                .ok()
                .and_then(|stream| stream.take_error().ok().flatten());
            if let Some(error) = result {
                finish_error(
                    &continuation_for_callback,
                    handle_address as *mut Continuation,
                    operation,
                    4,
                    error.to_string(),
                );
                forget_pending(scope_id, handle_address, operation);
                return true;
            }
            let socket = insert_stream(stream_for_callback.clone(), scope_id);
            let Some(resource) = allocate_socket_resource(socket) else {
                sockets().lock().expect("socket map mutex").remove(&socket);
                finish_error(
                    &continuation_for_callback,
                    handle_address as *mut Continuation,
                    operation,
                    4,
                    "could not allocate socket resource",
                );
                forget_pending(scope_id, handle_address, operation);
                return true;
            };
            let payload = [0_usize, resource as usize, 0, 0];
            let completed = continuation_for_callback.complete_suspend_with_payload(
                handle_address as *mut Continuation,
                operation,
                payload.as_ptr().cast(),
                payload.len() * std::mem::size_of::<usize>(),
            );
            if !completed {
                sockets().lock().expect("socket map mutex").remove(&socket);
                jk_drop(payload[1] as *mut u8);
            }
            forget_pending(scope_id, handle_address, operation);
            true
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle_address,
            operation,
            4,
        )),
    );
    remember_pending(handle, operation, io_id, None, 4, &continuation);
    1
}

unsafe extern "C" fn socket_listen_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 3 * std::mem::size_of::<usize>()
        || result_size != 4 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let host =
        read_word(arguments, 0, std::mem::size_of::<usize>()).map(|pointer| pointer as *mut u8);
    let length = read_word(arguments, 1, std::mem::size_of::<usize>()).unwrap_or(0);
    let port = read_u16(arguments, 2).unwrap_or(0);
    let Some(host) = host else { return 0 };
    if jk_string_len(host) != length {
        return 0;
    }
    let host = unsafe { std::slice::from_raw_parts(host, length) };
    let Ok(host) = std::str::from_utf8(host) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(address) = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next())
    else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "could not resolve socket address",
        );
        return 1;
    };
    let listener = match TcpListener::bind(address) {
        Ok(listener) => Arc::new(Mutex::new(listener)),
        Err(error) => {
            finish_error(&continuation, handle, operation, 4, error.to_string());
            return 1;
        }
    };
    let socket = insert_listener(listener, continuation.scope_id());
    let Some(resource) = allocate_socket_resource(socket) else {
        sockets().lock().expect("socket map mutex").remove(&socket);
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "could not allocate socket resource",
        );
        return 1;
    };
    let payload = [0_usize, resource as usize, 0, 0];
    let completed = continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        payload.len() * std::mem::size_of::<usize>(),
    );
    if !completed {
        sockets().lock().expect("socket map mutex").remove(&socket);
        jk_drop(resource);
    }
    1
}

unsafe extern "C" fn socket_accept_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != std::mem::size_of::<usize>()
        || result_size != 4 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let listener_id =
        read_word(arguments, 0, std::mem::size_of::<usize>()).and_then(socket_id_from_value);
    let Some(listener_id) = listener_id else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(listener) = listener_for_socket(listener_id, continuation.scope_id()) else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown socket listener",
        );
        return 1;
    };
    let handle_address = handle as usize;
    let continuation_for_callback = continuation.clone();
    let scope_id = continuation.scope_id();
    let io_id = register_tcp_listener_with_error(
        Arc::clone(&listener),
        Interest::READABLE,
        Box::new(move |_event, listener| {
            let result = listener
                .lock()
                .ok()
                .and_then(|listener| match listener.accept() {
                    Ok((stream, _address)) => Some(Ok(stream)),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                    Err(error) => Some(Err(error)),
                });
            let Some(result) = result else { return false };
            match result {
                Ok(stream) => {
                    let socket = insert_stream(Arc::new(Mutex::new(stream)), scope_id);
                    let Some(resource) = allocate_socket_resource(socket) else {
                        sockets().lock().expect("socket map mutex").remove(&socket);
                        finish_error(
                            &continuation_for_callback,
                            handle_address as *mut Continuation,
                            operation,
                            4,
                            "could not allocate socket resource",
                        );
                        forget_pending(scope_id, handle_address, operation);
                        return true;
                    };
                    let payload = [0_usize, resource as usize, 0, 0];
                    if !continuation_for_callback.complete_suspend_with_payload(
                        handle_address as *mut Continuation,
                        operation,
                        payload.as_ptr().cast(),
                        payload.len() * std::mem::size_of::<usize>(),
                    ) {
                        sockets().lock().expect("socket map mutex").remove(&socket);
                        jk_drop(resource);
                    }
                }
                Err(error) => finish_error(
                    &continuation_for_callback,
                    handle_address as *mut Continuation,
                    operation,
                    4,
                    error.to_string(),
                ),
            }
            forget_pending(scope_id, handle_address, operation);
            true
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle_address,
            operation,
            4,
        )),
    );
    remember_pending(
        handle,
        operation,
        io_id,
        Some(listener_id),
        4,
        &continuation,
    );
    1
}

fn clone_tcp_stream(stream: &TcpStream) -> std::io::Result<TcpStream> {
    #[cfg(unix)]
    let stream = {
        use std::os::fd::AsFd;
        std::net::TcpStream::from(stream.as_fd().try_clone_to_owned()?)
    };
    #[cfg(windows)]
    let stream = {
        use std::os::windows::io::AsSocket;
        std::net::TcpStream::from(stream.as_socket().try_clone_to_owned()?)
    };
    Ok(TcpStream::from_std(stream))
}

unsafe extern "C" fn socket_split_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    // Result((TcpStream, TcpStream), String): tag, two handles, error ptr/len.
    if arguments_size != std::mem::size_of::<usize>()
        || result_size != 5 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let socket_id =
        read_word(arguments, 0, std::mem::size_of::<usize>()).and_then(socket_id_from_value);
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket_id) = socket_id else {
        finish_error(&continuation, handle, operation, 5, "unknown socket handle");
        return 1;
    };
    let Some(stream) = stream_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(&continuation, handle, operation, 5, "unknown socket handle");
        return 1;
    };
    let scope_id = continuation.scope_id();
    // Each direction needs a distinct OS descriptor and mio registration.
    // Duplicating an Arc would register the same mio source twice.
    let write_stream = stream
        .lock()
        .map_err(|_| std::io::Error::other("TCP stream mutex poisoned"))
        .and_then(|stream| clone_tcp_stream(&stream));
    let write_stream = match write_stream {
        Ok(stream) => Arc::new(Mutex::new(stream)),
        Err(error) => {
            finish_error(&continuation, handle, operation, 5, error.to_string());
            return 1;
        }
    };
    let left = insert_resource(SocketResource::ReadHalf(stream), scope_id);
    let right = insert_resource(SocketResource::WriteHalf(write_stream), scope_id);
    let Some(left_resource) = allocate_socket_resource(left) else {
        sockets().lock().expect("socket map mutex").remove(&left);
        sockets().lock().expect("socket map mutex").remove(&right);
        finish_error(
            &continuation,
            handle,
            operation,
            5,
            "could not allocate socket resource",
        );
        return 1;
    };
    let Some(right_resource) = allocate_socket_resource(right) else {
        jk_drop(left_resource);
        sockets().lock().expect("socket map mutex").remove(&left);
        sockets().lock().expect("socket map mutex").remove(&right);
        finish_error(
            &continuation,
            handle,
            operation,
            5,
            "could not allocate socket resource",
        );
        return 1;
    };
    let payload = [
        0_usize,
        left_resource as usize,
        right_resource as usize,
        0,
        0,
    ];
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        payload.len() * std::mem::size_of::<usize>(),
    ) {
        jk_drop(left_resource);
        jk_drop(right_resource);
    }
    1
}

unsafe extern "C" fn socket_read_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * std::mem::size_of::<usize>()
        || result_size != 4 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let socket_id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let max = read_word(arguments, 1, 8)
        .unwrap_or(0)
        .min(16 * 1024 * 1024);
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket_id) = socket_id else {
        finish_error(&continuation, handle, operation, 4, "unknown socket handle");
        return 1;
    };
    let Some(socket) = stream_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(&continuation, handle, operation, 4, "unknown socket handle");
        return 1;
    };
    let handle_address = handle as usize;
    let continuation_for_callback = continuation.clone();
    let scope_id = continuation.scope_id();
    let io_id = register_tcp_with_error(
        Arc::clone(&socket),
        Interest::READABLE,
        Box::new(move |_event, stream| {
            let mut bytes = vec![0_u8; max];
            let result = stream
                .lock()
                .ok()
                .and_then(|mut stream| match stream.read(&mut bytes) {
                    Ok(length) => Some(Ok(length)),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                    Err(error) => Some(Err(error)),
                });
            let Some(result) = result else { return false };
            match result {
                Ok(length) => {
                    let value = super::bytes::jk_bytes_from_data(bytes.as_ptr(), length);
                    if value.is_null() {
                        finish_error(
                            &continuation_for_callback,
                            handle_address as *mut Continuation,
                            operation,
                            4,
                            "could not allocate bytes result",
                        );
                        forget_pending(scope_id, handle_address, operation);
                        return true;
                    }
                    let payload = [0_usize, value as usize, 0, 0];
                    if !continuation_for_callback.complete_suspend_with_payload(
                        handle_address as *mut Continuation,
                        operation,
                        payload.as_ptr().cast(),
                        4 * std::mem::size_of::<usize>(),
                    ) {
                        jk_drop(value);
                    }
                }
                Err(error) => finish_error(
                    &continuation_for_callback,
                    handle_address as *mut Continuation,
                    operation,
                    4,
                    error.to_string(),
                ),
            }
            forget_pending(scope_id, handle_address, operation);
            true
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle_address,
            operation,
            4,
        )),
    );
    remember_pending(handle, operation, io_id, Some(socket_id), 4, &continuation);
    1
}

unsafe extern "C" fn socket_write_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * std::mem::size_of::<usize>()
        || result_size != 4 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let socket_id = read_word(arguments, 0, 8)
        .and_then(socket_id_from_value)
        .unwrap_or(0);
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) = stream_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(&continuation, handle, operation, 4, "unknown socket handle");
        return 1;
    };
    let bytes = read_word(arguments, 1, 8).map(|value| value as *mut u8);
    let Some(bytes) = bytes else { return 0 };
    let length = jk_bytes_length(bytes);
    let data = if length == 0 {
        Vec::new()
    } else {
        let pointer = jk_bytes_data(bytes);
        if pointer.is_null() {
            return 0;
        }
        unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
    };
    let handle_address = handle as usize;
    let offset = Arc::new(AtomicUsize::new(0));
    let offset_for_callback = Arc::clone(&offset);
    let data = Arc::new(data);
    let data_for_callback = Arc::clone(&data);
    let continuation_for_callback = continuation.clone();
    let scope_id = continuation.scope_id();
    let io_id = register_tcp_with_error(
        Arc::clone(&socket),
        Interest::WRITABLE,
        Box::new(move |_event, stream| {
            let start = offset_for_callback.load(Ordering::Acquire);
            let result = stream.lock().ok().and_then(|mut stream| {
                match stream.write(&data_for_callback[start..]) {
                    Ok(length) => Some(Ok(length)),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                    Err(error) => Some(Err(error)),
                }
            });
            let Some(result) = result else { return false };
            let written = match result {
                Ok(written) => written,
                Err(error) => {
                    finish_error(
                        &continuation_for_callback,
                        handle_address as *mut Continuation,
                        operation,
                        4,
                        error.to_string(),
                    );
                    forget_pending(scope_id, handle_address, operation);
                    return true;
                }
            };
            let next = start.saturating_add(written).min(data_for_callback.len());
            offset_for_callback.store(next, Ordering::Release);
            if next != data_for_callback.len() {
                return false;
            }
            let payload = [0_usize, next, 0, 0];
            let _ = continuation_for_callback.complete_suspend_with_payload(
                handle_address as *mut Continuation,
                operation,
                payload.as_ptr().cast(),
                4 * std::mem::size_of::<usize>(),
            );
            forget_pending(scope_id, handle_address, operation);
            true
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle_address,
            operation,
            4,
        )),
    );
    remember_pending(handle, operation, io_id, Some(socket_id), 4, &continuation);
    1
}

unsafe extern "C" fn socket_close_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != std::mem::size_of::<usize>()
        || result_size != 3 * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let socket_id = read_word(arguments, 0, 8)
        .and_then(socket_id_from_value)
        .unwrap_or(0);
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let scope_id = continuation.scope_id();
    let removed = {
        let mut entries = sockets().lock().expect("socket map mutex");
        if entries
            .get(&socket_id)
            .is_some_and(|entry| entry.scope_id == scope_id)
        {
            entries.remove(&socket_id)
        } else {
            None
        }
    };
    let Some(entry) = removed else {
        finish_error(&continuation, handle, operation, 3, "unknown socket handle");
        return 1;
    };
    // Closing a handle also disarms provider-owned readiness registrations on
    // that resource. Their callbacks may still race with cancellation, but
    // the continuation generation/state check makes those completions harmless.
    cancel_socket_pending(scope_id, socket_id);
    cleanup_socket_resource(entry.resource);
    let payload = [0_usize, 0, 0];
    unsafe {
        jk_continuation_complete_suspend_with_payload(
            handle,
            operation,
            payload.as_ptr().cast(),
            payload.len() * std::mem::size_of::<usize>(),
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn resource_snapshot(
    scope: Option<crate::runtime::scope::ScopeId>,
) -> (usize, usize, usize) {
    let sockets = sockets().lock().unwrap();
    let pending = pending().lock().unwrap();
    (
        sockets
            .values()
            .filter(|entry| scope.is_none_or(|id| id == entry.scope_id))
            .count(),
        pending
            .values()
            .filter(|entry| scope.is_none_or(|id| id == entry.scope_id))
            .count(),
        sockets.capacity() + pending.capacity(),
    )
}
