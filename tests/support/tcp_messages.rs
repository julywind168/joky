use std::{
    io::Read,
    net::{Ipv4Addr, TcpListener},
    os::fd::AsRawFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Received {
    pub complete: usize,
    pub partial: usize,
}
pub struct Peer {
    pub port: u16,
    pub accepted: Arc<AtomicBool>,
    gate: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<Received>>,
}
impl Peer {
    pub fn release(&mut self) {
        if let Some(gate) = self.gate.take() {
            let _ = gate.send(());
        }
    }
    pub fn finish(mut self) -> Received {
        self.release();
        self.thread.take().unwrap().join().unwrap()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.release();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn peer() -> Peer {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
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
    let (level, option) = (0xffff, 0x1002);
    #[cfg(target_os = "linux")]
    let (level, option) = (1, 8);
    let window: i32 = 4096;
    // SAFETY: The live listener fd and native i32 SO_RCVBUF value are valid.
    assert_eq!(
        unsafe {
            setsockopt(
                listener.as_raw_fd(),
                level,
                option,
                (&window as *const i32).cast(),
                4,
            )
        },
        0
    );
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicBool::new(false));
    let mark = accepted.clone();
    let (gate, ready) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        mark.store(true, Ordering::Release);
        ready.recv_timeout(Duration::from_secs(60)).unwrap();
        // Enlarge after observing backpressure; no receive before the gate opens.
        let window: i32 = 262144;
        // SAFETY: The accepted live stream has the same socket option ABI.
        assert_eq!(
            unsafe {
                setsockopt(
                    stream.as_raw_fd(),
                    level,
                    option,
                    (&window as *const i32).cast(),
                    4,
                )
            },
            0
        );
        let mut wire = Vec::new();
        let mut block = [0; 65536];
        loop {
            let n = stream.read(&mut block).unwrap();
            if n == 0 {
                break;
            }
            wire.extend_from_slice(&block[..n]);
            assert!(wire.len() <= 128 * (65536 + 4));
        }
        let mut offset = 0;
        let mut complete = 0;
        while wire.len() - offset >= 4 {
            assert_eq!(&wire[offset..offset + 4], &[0, 1, 0, 0]);
            offset += 4;
            let n = (wire.len() - offset).min(65536);
            if n != 0 {
                assert_eq!(wire[offset], complete as u8);
            }
            assert!(wire[offset + n.min(1)..offset + n].iter().all(|b| *b == 42));
            offset += n;
            if n < 65536 {
                return Received {
                    complete,
                    partial: n + 4,
                };
            }
            complete += 1;
        }
        Received {
            complete,
            partial: wire.len() - offset,
        }
    });
    Peer {
        port,
        accepted,
        gate: Some(gate),
        thread: Some(thread),
    }
}
