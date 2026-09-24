//! Unix stream and datagram socket providers.

use super::*;

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_connect_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let Some(path) = unix_path_from_args(arguments, 0) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            finish_error(&continuation, handle, operation, 4, error.to_string());
            return 1;
        }
    };
    let stream = Arc::new(Mutex::new(stream));
    let stream_for_callback = Arc::clone(&stream);
    let continuation_for_callback = continuation.clone();
    let handle_address = handle as usize;
    let scope_id = continuation.scope_id();
    let io_id = register_unix_with_error(
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
            let socket = insert_unix_stream(stream_for_callback.clone(), scope_id);
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

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_listen_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let Some(path) = unix_path_from_args(arguments, 0) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => Arc::new(Mutex::new(listener)),
        Err(error) => {
            finish_error(&continuation, handle, operation, 4, error.to_string());
            return 1;
        }
    };
    let socket = insert_unix_listener(listener, path, continuation.scope_id());
    let Some(resource) = allocate_socket_resource(socket) else {
        let entry = sockets().lock().expect("socket map mutex").remove(&socket);
        if let Some(entry) = entry {
            cleanup_socket_resource(entry.resource);
        }
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
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        payload.len() * std::mem::size_of::<usize>(),
    ) {
        let entry = sockets().lock().expect("socket map mutex").remove(&socket);
        if let Some(entry) = entry {
            cleanup_socket_resource(entry.resource);
        }
        jk_drop(resource);
    }
    1
}

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_accept_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 8 || result_size != 4 * 8 {
        return 0;
    }
    let listener_id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let Some(listener_id) = listener_id else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(listener) = unix_listener_for_socket(listener_id, continuation.scope_id()) else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown Unix socket listener",
        );
        return 1;
    };
    let continuation_for_callback = continuation.clone();
    let handle_address = handle as usize;
    let scope_id = continuation.scope_id();
    let io_id = register_unix_listener_with_error(
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
                    let socket = insert_unix_stream(Arc::new(Mutex::new(stream)), scope_id);
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

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_read_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
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
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown Unix socket handle",
        );
        return 1;
    };
    let Some(socket) = unix_stream_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown Unix socket handle",
        );
        return 1;
    };
    let continuation_for_callback = continuation.clone();
    let handle_address = handle as usize;
    let scope_id = continuation.scope_id();
    let io_id = register_unix_with_error(
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
                    let value = super::super::bytes::jk_bytes_from_data(bytes.as_ptr(), length);
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

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_write_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let socket_id = read_word(arguments, 0, 8)
        .and_then(socket_id_from_value)
        .unwrap_or(0);
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) = unix_stream_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown Unix socket handle",
        );
        return 1;
    };
    let bytes = read_word(arguments, 1, 8).map(|value| value as *mut u8);
    let Some(bytes) = bytes else {
        return 0;
    };
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
    let continuation_for_callback = continuation.clone();
    let handle_address = handle as usize;
    let offset = Arc::new(AtomicUsize::new(0));
    let offset_for_callback = Arc::clone(&offset);
    let data = Arc::new(data);
    let data_for_callback = Arc::clone(&data);
    let scope_id = continuation.scope_id();
    let io_id = register_unix_with_error(
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

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_close_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    result: *mut u8,
    result_size: usize,
) -> u8 {
    socket_close_start(
        handle,
        operation,
        arguments,
        arguments_size,
        result,
        result_size,
    )
}

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_dgram_bind_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let Some(path) = unix_path_from_args(arguments, 0) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let socket = match UnixDatagram::bind(&path) {
        Ok(socket) => Arc::new(Mutex::new(socket)),
        Err(error) => {
            finish_error(&continuation, handle, operation, 4, error.to_string());
            return 1;
        }
    };
    let socket_id = insert_unix_datagram(socket, path, continuation.scope_id());
    let Some(resource) = allocate_socket_resource(socket_id) else {
        let entry = sockets()
            .lock()
            .expect("socket map mutex")
            .remove(&socket_id);
        if let Some(entry) = entry {
            cleanup_socket_resource(entry.resource);
        }
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
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        payload.len() * std::mem::size_of::<usize>(),
    ) {
        let entry = sockets()
            .lock()
            .expect("socket map mutex")
            .remove(&socket_id);
        if let Some(entry) = entry {
            cleanup_socket_resource(entry.resource);
        }
        jk_drop(resource);
    }
    1
}

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_dgram_send_to_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 4 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let socket_id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let Some(path) = unix_path_from_args(arguments, 2) else {
        return 0;
    };
    let bytes = read_word(arguments, 1, 8).map(|pointer| pointer as *mut u8);
    let Some(bytes) = bytes else {
        return 0;
    };
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
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) =
        socket_id.and_then(|id| unix_datagram_for_socket(id, continuation.scope_id()))
    else {
        finish_error(
            &continuation,
            handle,
            operation,
            4,
            "unknown Unix datagram socket handle",
        );
        return 1;
    };
    match socket
        .lock()
        .map_err(|_| "Unix datagram mutex poisoned".to_owned())
        .and_then(|socket| {
            socket
                .send_to(&data, &path)
                .map_err(|error| error.to_string())
        }) {
        Ok(written) => {
            let payload = [0_usize, written, 0, 0];
            continuation.complete_suspend_with_payload(
                handle,
                operation,
                payload.as_ptr().cast(),
                payload.len() * 8,
            );
        }
        Err(error) => finish_error(&continuation, handle, operation, 4, error),
    }
    1
}

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_dgram_recv_from_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 6 * 8 {
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
        finish_error(
            &continuation,
            handle,
            operation,
            6,
            "unknown Unix datagram socket handle",
        );
        return 1;
    };
    let Some(socket) = unix_datagram_for_socket(socket_id, continuation.scope_id()) else {
        finish_error(
            &continuation,
            handle,
            operation,
            6,
            "unknown Unix datagram socket handle",
        );
        return 1;
    };
    let continuation_for_callback = continuation.clone();
    let handle_address = handle as usize;
    let scope_id = continuation.scope_id();
    let io_id = register_unix_datagram_with_error(
        Arc::clone(&socket),
        Interest::READABLE,
        Box::new(move |_event, socket| {
            let mut bytes = vec![0_u8; max];
            let result = socket
                .lock()
                .ok()
                .and_then(|socket| match socket.recv_from(&mut bytes) {
                    Ok((length, address)) => Some(Ok((length, address))),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                    Err(error) => Some(Err(error.to_string())),
                });
            let Some(result) = result else {
                return false;
            };
            match result {
                Ok((length, address)) => {
                    let value = super::super::bytes::jk_bytes_from_data(bytes.as_ptr(), length);
                    let path = address
                        .as_pathname()
                        .and_then(|path| path.to_str())
                        .unwrap_or("");
                    let path_value = jk_string_from_utf8(path.as_ptr(), path.len());
                    if value.is_null() || path_value.is_null() {
                        if !value.is_null() {
                            jk_drop(value);
                        }
                        if !path_value.is_null() {
                            jk_drop(path_value);
                        }
                        finish_error(
                            &continuation_for_callback,
                            handle_address as *mut Continuation,
                            operation,
                            6,
                            "could not allocate Unix datagram result",
                        );
                    } else {
                        let payload = [
                            0_usize,
                            value as usize,
                            path_value as usize,
                            path.len(),
                            0,
                            0,
                        ];
                        if !continuation_for_callback.complete_suspend_with_payload(
                            handle_address as *mut Continuation,
                            operation,
                            payload.as_ptr().cast(),
                            6 * 8,
                        ) {
                            jk_drop(value);
                            jk_drop(path_value);
                        }
                    }
                }
                Err(error) => finish_error(
                    &continuation_for_callback,
                    handle_address as *mut Continuation,
                    operation,
                    6,
                    error,
                ),
            }
            forget_pending(scope_id, handle_address, operation);
            true
        }),
        Some(registration_error_callback(
            continuation.clone(),
            handle_address,
            operation,
            6,
        )),
    );
    remember_pending(handle, operation, io_id, Some(socket_id), 6, &continuation);
    1
}

#[cfg(unix)]
pub(crate) unsafe extern "C" fn socket_unix_dgram_close_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    socket_close_start(
        handle,
        operation,
        arguments,
        arguments_size,
        std::ptr::null_mut(),
        result_size,
    )
}
