use std::{
    io::Read,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

// Each client completes connect and sends a marker before the peer sends RST.
pub fn reset_endpoint(connections: usize) -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || {
        for _ in 0..connections {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "client did not connect");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut signal = [0];
            stream.read_exact(&mut signal).unwrap();
            assert_eq!(signal, [1]);
            unsafe extern "C" {
                fn setsockopt(
                    fd: i32,
                    level: i32,
                    option: i32,
                    value: *const std::ffi::c_void,
                    length: u32,
                ) -> i32;
            }
            #[cfg(target_os = "macos")]
            let (level, option) = (0xffff, 0x80);
            #[cfg(target_os = "linux")]
            let (level, option) = (1, 13);
            let linger = [1i32, 0];
            // SAFETY: A live socket fd and the native two-i32 linger layout.
            assert_eq!(
                unsafe {
                    setsockopt(
                        stream.as_raw_fd(),
                        level,
                        option,
                        linger.as_ptr().cast(),
                        std::mem::size_of_val(&linger) as u32,
                    )
                },
                0
            );
            // Dropping with linger=0 performs an abortive close.
        }
    });
    (port, peer)
}
