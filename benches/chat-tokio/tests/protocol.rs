use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Server {
    fn start(workers: usize, extra: &[&str]) -> (Self, u16, u16) {
        let child = Command::new(env!("CARGO_BIN_EXE_chat-tokio"))
            .args([
                "--chat-port",
                "0",
                "--admin-port",
                "0",
                "--max-clients",
                "2",
                "--task-workers",
                &workers.to_string(),
            ])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut server = Self(child);
        let stdout = server.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut lines = [String::new(), String::new()];
            for line in &mut lines {
                reader.read_line(line).unwrap();
            }
            let _ = tx.send(lines);
        });
        let [chat, admin] = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let port =
            |line: String, prefix: &str| line.trim().strip_prefix(prefix).unwrap().parse().unwrap();
        (server, port(chat, "chat "), port(admin, "admin "))
    }
    fn exited(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                let mut errors = String::new();
                self.0
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut errors)
                    .unwrap();
                assert!(status.success(), "{status}: {errors}");
                assert!(errors.is_empty(), "{errors}");
                return;
            }
            assert!(Instant::now() < deadline, "shutdown did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
struct Peer(BufReader<TcpStream>);
impl Peer {
    fn open(port: u16) -> Self {
        let socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self(BufReader::new(socket))
    }
    fn send(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.get_mut().write_all(bytes.as_ref()).unwrap();
    }
    fn read(&mut self) -> String {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        line
    }
    fn expect(&mut self, expected: &str) {
        assert_eq!(self.read(), expected);
    }
    fn closed(&mut self) {
        assert_eq!(self.0.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn real_tcp_protocol_admission_framing_and_shutdown_on_one_and_three_workers() {
    for workers in [1, 3] {
        let (mut server, port, admin_port) = Server::start(workers, &[]);
        let mut a = Peer::open(port);
        a.expect("welcome 1\n");
        let mut b = Peer::open(port);
        b.expect("welcome 2\n");
        Peer::open(port).closed();
        a.send("join 房");
        a.send("间\r\n");
        a.expect("joined 房间 1\n");
        b.send("join 房间\n");
        a.expect("joined 房间 2\n");
        b.expect("joined 房间 2\n");
        a.send("say  hello 中文\nbalance\n");
        a.expect("message 房间 1  hello 中文\n");
        b.expect("message 房间 1  hello 中文\n");
        a.expect("balance 4\n");
        a.send("join next\nleave\n");
        b.expect("left 房间 1\n");
        a.expect("left 房间 1\n");
        a.expect("joined next 1\n");
        a.expect("left next 1\n");
        a.send(b"\xff\n");
        a.closed();
        // Half closure may become visible just before the admission permit drops.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut c = loop {
            let mut candidate = Peer::open(port);
            let welcome = candidate.read();
            if !welcome.is_empty() {
                assert_eq!(welcome, "welcome 3\n");
                break candidate;
            }
            assert!(
                Instant::now() < deadline,
                "admission permit was not returned"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        c.send("say unfinished");
        c.0.get_mut().shutdown(Shutdown::Write).unwrap();
        c.closed();
        b.send(format!("{}\n", "x".repeat(4096)));
        b.closed();
        let mut partial = Peer::open(admin_port);
        partial.send("shutdown");
        partial.0.get_mut().shutdown(Shutdown::Write).unwrap();
        partial.closed();
        let mut admin = Peer::open(admin_port);
        admin.send("reload\njoin no\nshutdown\n");
        admin.expect("error reload unsupported in static Rust server\n");
        admin.expect("error use reload or shutdown\n");
        admin.expect("shutting down\n");
        server.exited();
    }
}

#[test]
fn complete_line_timeout_and_invalid_worker_count() {
    let (mut server, port, admin_port) = Server::start(1, &["--idle-ms", "50"]);
    let mut peer = Peer::open(port);
    peer.expect("welcome 1\n");
    peer.send("balance");
    peer.closed();
    let mut admin = Peer::open(admin_port);
    admin.send("shutdown\n");
    admin.expect("shutting down\n");
    server.exited();
    let output = Command::new(env!("CARGO_BIN_EXE_chat-tokio"))
        .args(["--task-workers", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
