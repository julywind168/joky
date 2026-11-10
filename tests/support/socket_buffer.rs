use std::{io, mem, os::fd::AsRawFd};

pub fn set(socket: &impl AsRawFd, option: libc::c_int, size: libc::c_int) {
    let mut actual: libc::c_int = 0;
    let mut length = mem::size_of_val(&actual) as libc::socklen_t;
    // SAFETY: The socket is live; both options use a native int and valid lengths.
    unsafe {
        assert_eq!(
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&size as *const libc::c_int).cast(),
                length
            ),
            0,
            "setsockopt: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&mut actual as *mut libc::c_int).cast(),
                &mut length
            ),
            0,
            "getsockopt: {}",
            io::Error::last_os_error()
        );
    }
    // Linux accounts for socket bookkeeping by doubling the requested capacity.
    assert!(
        actual > 0 && actual <= size * 4,
        "socket buffer option {option}: requested {size}, actual {actual}"
    );
}
