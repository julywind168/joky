mod socket_buffer {
    include!("socket_buffer.rs");
}
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub fn source() -> String {
    let protocol = include_str!("../../examples/tcp_json.jk")
        .split("fn serve(")
        .next()
        .unwrap();
    format!(
        "{protocol}\n{}",
        include_str!("../fixtures/json_tcp_sessions.jk").replace("PRESSURE_TEXT", &"x".repeat(8192))
    )
}
fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut output = (bytes.len() as u32).to_be_bytes().to_vec();
    output.extend_from_slice(bytes);
    output
}
fn read_frame(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut header = [0; 4];
    if stream.read(&mut header[..1]).unwrap() == 0 {
        return None;
    }
    stream.read_exact(&mut header[1..]).unwrap();
    let size = u32::from_be_bytes(header) as usize;
    assert!(size <= 65536);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    Some(bytes)
}
pub struct Peer {
    pub port: u16,
    gate: Option<mpsc::Sender<bool>>,
    thread: Option<JoinHandle<usize>>,
}
impl Peer {
    pub fn release(&mut self) {
        if let Some(gate) = self.gate.take() {
            let _ = gate.send(false);
        }
    }
    pub fn finish(mut self, cancelled: bool) -> usize {
        if let Some(gate) = self.gate.take() {
            let _ = gate.send(cancelled);
        }
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
// 0: complete business errors followed by a valid request; 1/2: broken framing.
// 3: hold the receive window closed until the VM confirms pending write progress.
pub fn peer(case: u8) -> Peer {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    if case == 3 {
        // Configure before the handshake so the initial receive window is bounded.
        socket_buffer::set(&listener, libc::SO_RCVBUF, 4096);
    }
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let (gate, wait) = mpsc::channel();
    let thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "JSON client never connected");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{error}"),
            }
        };
        // Accepted sockets inherit the listener's nonblocking flag on macOS.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        if case == 3 {
            stream
                .write_all(&frame(br#"["push",99,{"alive":true}]"#))
                .unwrap();
            let cancelled = wait.recv_timeout(Duration::from_secs(60)).unwrap();
            // Once the gate opens, restore receive capacity for draining.
            socket_buffer::set(&stream, libc::SO_RCVBUF, 262144);
            let mut wire = Vec::new();
            if let Err(error) = stream.read_to_end(&mut wire) {
                // Cancelling with the incoming push unread may reset TCP on Linux.
                // Successful completion must still reach EOF; other errors are failures.
                assert!(
                    cancelled && error.kind() == std::io::ErrorKind::ConnectionReset,
                    "JSON peer drain failed: {error}"
                );
            }
            let expected = frame(format!("[\"reply\",7,\"{}\"]", "x".repeat(8192)).as_bytes());
            let count = wire.len() / expected.len();
            assert!(count <= 512);
            for complete in wire[..count * expected.len()].chunks_exact(expected.len()) {
                assert_eq!(complete, expected);
            }
            assert_eq!(
                &wire[count * expected.len()..],
                &expected[..wire.len() % expected.len()]
            );
            return count;
        }
        let push = read_frame(&mut stream).unwrap();
        assert!(push.starts_with(br#"["push",0,{"#));
        match case {
            0 => {
                let request = frame(
                    "[\"request\",9,[18446744073709551616,1e400,\"你🙂\\u0000\"]]".as_bytes(),
                );
                // Split the four-byte header and every UTF-8/token byte, then coalesce errors.
                for byte in &request {
                    stream.write_all(&[*byte]).unwrap();
                    thread::yield_now();
                }
                let mut errors = frame(b"[");
                errors.extend(frame(&[0xff]));
                errors.extend(frame(br#"["request",1.5,null]"#));
                errors.extend(frame(br#"["request",10,true]"#));
                stream.write_all(&errors).unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
                for expected in [
                    "[\"reply\",9,[18446744073709551616,1e400,\"你🙂\\u0000\"]]",
                    "[\"error\",0,\"invalid-json\"]",
                    "[\"error\",0,\"invalid-json\"]",
                    "[\"error\",0,\"invalid-request\"]",
                    "[\"reply\",10,true]",
                ] {
                    assert_eq!(read_frame(&mut stream).unwrap(), expected.as_bytes());
                }
                assert!(read_frame(&mut stream).is_none());
            }
            1 => {
                stream.write_all(&[0, 0, 0, 8, b'[', b'1']).unwrap();
                stream.shutdown(std::net::Shutdown::Write).unwrap();
                assert!(read_frame(&mut stream).is_none());
            }
            2 => {
                stream.write_all(&65537u32.to_be_bytes()).unwrap();
                assert!(read_frame(&mut stream).is_none());
            }
            _ => unreachable!(),
        }
        0
    });
    Peer {
        port,
        gate: Some(gate),
        thread: Some(thread),
    }
}
