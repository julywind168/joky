//! UDP socket providers.

use super::*;

fn udp_address_from_args(
    arguments: *const u8,
    host_index: usize,
    port_index: usize,
) -> Option<std::net::SocketAddr> {
    let host = read_word(arguments, host_index, 8).map(|p| p as *mut u8)?;
    let len = read_word(arguments, host_index + 1, 8)?;
    let port = read_u16(arguments, port_index)? as usize;
    if jk_string_len(host) != len {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(host, len) };
    let host = std::str::from_utf8(bytes).ok()?;
    (host, port as u16).to_socket_addrs().ok()?.next()
}

pub(crate) unsafe extern "C" fn socket_udp_bind_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 3 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let Some(address) = udp_address_from_args(arguments, 0, 2) else {
        return 0;
    };
    let Some(c) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    match UdpSocket::bind(address) {
        Ok(socket) => {
            let id = insert_datagram(Arc::new(Mutex::new(socket)), c.scope_id());
            if let Some(resource) = allocate_socket_resource(id) {
                let p = [0, resource as usize, 0, 0];
                c.complete_suspend_with_payload(handle, operation, p.as_ptr().cast(), 32);
            } else {
                finish_error(
                    &c,
                    handle,
                    operation,
                    4,
                    "could not allocate socket resource",
                );
            }
        }
        Err(e) => finish_error(&c, handle, operation, 4, e.to_string()),
    }
    1
}

pub(crate) unsafe extern "C" fn socket_udp_connect_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 4 * 8 || result_size != 3 * 8 {
        return 0;
    }
    let id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let Some(address) = udp_address_from_args(arguments, 1, 3) else {
        return 0;
    };
    let Some(c) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) = id.and_then(|id| datagram_for_socket(id, c.scope_id())) else {
        finish_error(&c, handle, operation, 3, "unknown UDP socket handle");
        return 1;
    };
    match socket
        .lock()
        .map_err(|_| "UDP socket mutex poisoned".to_owned())
        .and_then(|s| s.connect(address).map_err(|e| e.to_string()))
    {
        Ok(()) => {
            let p = [0, 0, 0];
            c.complete_suspend_with_payload(handle, operation, p.as_ptr().cast(), 24);
        }
        Err(e) => finish_error(&c, handle, operation, 3, e),
    }
    1
}

pub(crate) unsafe extern "C" fn socket_udp_send_to_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 5 * 8 || result_size != 4 * 8 {
        return 0;
    }
    let id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let Some(address) = udp_address_from_args(arguments, 2, 4) else {
        return 0;
    };
    let bytes = read_word(arguments, 1, 8).map(|p| p as *mut u8);
    let Some(bytes) = bytes else { return 0 };
    let len = jk_bytes_length(bytes);
    let data = if len == 0 {
        Vec::new()
    } else {
        let p = jk_bytes_data(bytes);
        if p.is_null() {
            return 0;
        }
        unsafe { std::slice::from_raw_parts(p, len) }.to_vec()
    };
    let Some(c) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) = id.and_then(|id| datagram_for_socket(id, c.scope_id())) else {
        finish_error(&c, handle, operation, 4, "unknown UDP socket handle");
        return 1;
    };
    match socket
        .lock()
        .map_err(|_| "UDP socket mutex poisoned".to_owned())
        .and_then(|s| s.send_to(&data, address).map_err(|e| e.to_string()))
    {
        Ok(n) => {
            let p = [0, n, 0, 0];
            c.complete_suspend_with_payload(handle, operation, p.as_ptr().cast(), 32);
        }
        Err(e) => finish_error(&c, handle, operation, 4, e),
    }
    1
}

pub(crate) unsafe extern "C" fn socket_udp_send_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    };
    let id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let bytes = read_word(arguments, 1, 8).map(|p| p as *mut u8);
    let Some(bytes) = bytes else { return 0 };
    let len = jk_bytes_length(bytes);
    let data = if len == 0 {
        Vec::new()
    } else {
        let p = jk_bytes_data(bytes);
        if p.is_null() {
            return 0;
        }
        unsafe { std::slice::from_raw_parts(p, len) }.to_vec()
    };
    let Some(c) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket) = id.and_then(|id| datagram_for_socket(id, c.scope_id())) else {
        finish_error(&c, handle, operation, 4, "unknown UDP socket handle");
        return 1;
    };
    match socket
        .lock()
        .map_err(|_| "UDP socket mutex poisoned".to_owned())
        .and_then(|s| s.send(&data).map_err(|e| e.to_string()))
    {
        Ok(n) => {
            let p = [0, n, 0, 0];
            c.complete_suspend_with_payload(handle, operation, p.as_ptr().cast(), 32);
        }
        Err(e) => finish_error(&c, handle, operation, 4, e),
    }
    1
}

pub(crate) unsafe extern "C" fn socket_udp_recv_from_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 7 * 8 {
        return 0;
    };
    udp_recv_common(handle, operation, arguments, result_size, true)
}
pub(crate) unsafe extern "C" fn socket_udp_recv_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments_size != 2 * 8 || result_size != 4 * 8 {
        return 0;
    };
    udp_recv_common(handle, operation, arguments, result_size, false)
}
unsafe fn udp_recv_common(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    result_size: usize,
    with_source: bool,
) -> u8 {
    let id = read_word(arguments, 0, 8).and_then(socket_id_from_value);
    let max = read_word(arguments, 1, 8)
        .unwrap_or(0)
        .min(16 * 1024 * 1024);
    let Some(c) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let Some(socket_id) = id else {
        finish_error(
            &c,
            handle,
            operation,
            if with_source { 7 } else { 4 },
            "unknown UDP socket handle",
        );
        return 1;
    };
    let Some(socket) = datagram_for_socket(socket_id, c.scope_id()) else {
        finish_error(
            &c,
            handle,
            operation,
            if with_source { 7 } else { 4 },
            "unknown UDP socket handle",
        );
        return 1;
    };
    let c2 = c.clone();
    let h = handle as usize;
    let scope = c.scope_id();
    let io_id = register_udp_with_error(
        Arc::clone(&socket),
        Interest::READABLE,
        Box::new(move |_event, socket| {
            let mut buf = vec![0; max];
            let result = socket.lock().ok().and_then(|s| {
                let received = if with_source {
                    s.recv_from(&mut buf).map(|(n, a)| (n, Some(a)))
                } else {
                    s.recv(&mut buf).map(|n| (n, None))
                };
                match received {
                    Ok(value) => Some(Ok(value)),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                    Err(error) => Some(Err(error.to_string())),
                }
            });
            let Some(result) = result else { return false };
            match result {
                Ok((n, src)) => {
                    let b = super::super::bytes::jk_bytes_from_data(buf.as_ptr(), n);
                    if b.is_null() {
                        finish_error(
                            &c2,
                            h as *mut Continuation,
                            operation,
                            if with_source { 7 } else { 4 },
                            "could not allocate bytes result",
                        );
                    } else if let Some(a) = src {
                        let host = a.ip().to_string();
                        let hp = jk_string_from_utf8(host.as_ptr(), host.len());
                        let p = [
                            0,
                            b as usize,
                            hp as usize,
                            host.len(),
                            a.port() as usize,
                            0,
                            0,
                        ];
                        let _ = c2.complete_suspend_with_payload(
                            h as *mut Continuation,
                            operation,
                            p.as_ptr().cast(),
                            result_size,
                        );
                    } else {
                        let p = [0, b as usize, 0, 0];
                        let _ = c2.complete_suspend_with_payload(
                            h as *mut Continuation,
                            operation,
                            p.as_ptr().cast(),
                            result_size,
                        );
                    }
                }
                Err(e) => finish_error(
                    &c2,
                    h as *mut Continuation,
                    operation,
                    if with_source { 7 } else { 4 },
                    e,
                ),
            }
            forget_pending(scope, h, operation);
            true
        }),
        Some(registration_error_callback(
            c.clone(),
            h,
            operation,
            if with_source { 7 } else { 4 },
        )),
    );
    remember_pending(
        handle,
        operation,
        io_id,
        Some(socket_id),
        if with_source { 7 } else { 4 },
        &c,
    );
    1
}

pub(crate) unsafe extern "C" fn socket_udp_close_start(
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
