//! Exercise the shipped Joky server over real TCP on all three backends.
#![cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;
#[path = "support/scratch.rs"]
mod scratch;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

struct Server {
    child: Child,
    log: std::path::PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let errors = std::fs::read_to_string(&self.log).unwrap();
        if !errors.is_empty() {
            eprintln!("chat server stderr: {errors}");
        }
    }
}
impl Server {
    fn start(mut command: Command, log: std::path::PathBuf) -> (Self, u16) {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Self { child, log };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut port = String::new();
            let result = BufReader::new(stdout).read_line(&mut port).map(|_| port);
            let _ = sender.send(result);
        });
        let port = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("server must announce its bound port")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        (server, port)
    }
}
struct Peer(BufReader<TcpStream>);
impl Peer {
    fn connect(port: u16, id: u64) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut peer = Self(BufReader::new(stream));
        peer.expect(&format!("welcome {id}\n"));
        peer
    }
    fn send(&mut self, text: impl AsRef<[u8]>) {
        self.0.get_mut().write_all(text.as_ref()).unwrap();
    }
    #[track_caller]
    fn expect(&mut self, expected: &str) {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        assert_eq!(line, expected);
    }
    fn quiet(&mut self) {
        assert!(self.0.buffer().is_empty(), "unexpected buffered output");
        self.0
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let error = self.0.get_mut().peek(&mut [0]).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        self.0
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
    }
    fn closed(&mut self) {
        assert_eq!(self.0.read(&mut [0]).unwrap(), 0);
    }
}

fn conversation(port: u16, members: u64) {
    let mut alice = Peer::connect(port, 1);
    let mut bob = Peer::connect(port, 2);
    let mut carol = Peer::connect(port, 3);
    alice.send("say before joining\n");
    alice.expect("error join a room first\n");
    // A command split between writes must not run before its LF arrives.
    alice.send("join 12");
    alice.quiet();
    alice.send("3\r\n");
    alice.expect("joined 123 1\n");
    bob.send("join 123\n");
    alice.expect("joined 123 2\n");
    bob.expect("joined 123 2\n");
    carol.send("join 房间\n");
    carol.expect("joined 房间 3\n");
    // Coalesced commands, spaces and UTF-8 spanning the packet boundary.
    let message = format!("{}你好 🙂  hello world", "x".repeat(27));
    alice.send(format!("say {message}\nsay second\n"));
    for peer in [&mut alice, &mut bob] {
        peer.expect(&format!("message 123 1 {message}\n"));
        peer.expect("message 123 1 second\n");
    }
    carol.quiet();
    // The line limit includes LF; the largest accepted command still broadcasts whole.
    let largest = "x".repeat(4091);
    alice.send(format!("say {largest}\n"));
    alice.expect(&format!("message 123 1 {largest}\n"));
    bob.expect(&format!("message 123 1 {largest}\n"));
    // Inputs arriving together must have the same room-wide order.
    alice.send("say from alice\n");
    bob.send("say from bob\n");
    let mut delivered = Vec::new();
    for _ in 0..2 {
        let mut line = String::new();
        alice.0.read_line(&mut line).unwrap();
        bob.expect(&line);
        delivered.push(line);
    }
    delivered.sort();
    assert_eq!(
        delivered,
        ["message 123 1 from alice\n", "message 123 2 from bob\n"]
    );
    bob.send("join 房间\n");
    alice.expect("left 123 2\n");
    bob.expect("left 123 2\n");
    bob.expect("joined 房间 2\n");
    carol.expect("joined 房间 2\n");
    bob.send("join 房间\nleave\nsay outside\nleave\njoin\nsay\nwat hi\nleave extra\n");
    bob.expect("error already joined\n");
    bob.expect("left 房间 2\n");
    carol.expect("left 房间 2\n");
    for _ in 0..2 {
        bob.expect("error join a room first\n");
    }
    for _ in 0..4 {
        bob.expect("error use join <room>, leave, or say <message>\n");
    }
    carol.send("say alone\n");
    carol.expect("message 房间 3 alone\n");
    bob.quiet();
    alice.quiet();
    bob.send("join 123\n");
    bob.expect("joined 123 2\n");
    alice.expect("joined 123 2\n");
    // A truncated final line is not executed; half-close removes membership.
    bob.send("say unfinished");
    bob.0.get_mut().shutdown(Shutdown::Write).unwrap();
    bob.closed();
    alice.expect("left 123 2\n");
    alice.quiet();
    // Repeated connect/disconnect reclaims membership and never reuses IDs.
    for id in 4..16 {
        let mut peer = Peer::connect(port, id);
        peer.send("join 123\n");
        peer.expect(&format!("joined 123 {id}\n"));
        alice.expect(&format!("joined 123 {id}\n"));
        if id == 4 {
            peer.send(b"say \xff\n");
            peer.closed();
        } else if id == 5 {
            peer.send(format!("say {}\n", "x".repeat(4096)));
            peer.closed();
        }
        drop(peer);
        alice.expect(&format!("left 123 {id}\n"));
    }
    carol.quiet();
    alice.send("say still running\n");
    alice.expect("message 123 1 still running\n");
    // Concurrent join, move, send and disconnect agree at both stable observers.
    let mut witness = Peer::connect(port, 16);
    let mut newcomer = Peer::connect(port, 17);
    let mut mover = Peer::connect(port, 18);
    let mut quitter = Peer::connect(port, 19);
    witness.send("join 123\n");
    alice.expect("joined 123 16\n");
    witness.expect("joined 123 16\n");
    mover.send("join 123\n");
    for peer in [&mut alice, &mut witness, &mut mover] {
        peer.expect("joined 123 18\n");
    }
    quitter.send("join 123\n");
    for peer in [&mut alice, &mut witness, &mut mover, &mut quitter] {
        peer.expect("joined 123 19\n");
    }
    newcomer.send("join 123\n");
    mover.send("join other\n");
    alice.send("say concurrent\n");
    drop(quitter);
    let mut events = Vec::new();
    for _ in 0..4 {
        let mut line = String::new();
        alice.0.read_line(&mut line).unwrap();
        witness.expect(&line);
        events.push(line);
    }
    events.sort();
    assert_eq!(
        events,
        [
            "joined 123 17\n",
            "left 123 18\n",
            "left 123 19\n",
            "message 123 1 concurrent\n"
        ]
    );
    drop((witness, newcomer, mover));
    let mut departures = Vec::new();
    for _ in 0..2 {
        let mut line = String::new();
        alice.0.read_line(&mut line).unwrap();
        departures.push(line);
    }
    departures.sort();
    assert_eq!(departures, ["left 123 16\n", "left 123 17\n"]);
    // Native backends also cross the former 256-message batch boundary.
    // The interpreter's 1024 live-frame budget admits fewer simultaneous TCP sessions;
    // its 260-target publication is covered by verify_wide_broadcast before listening.
    let mut peers: Vec<_> = (20..20 + members)
        .map(|id| Peer::connect(port, id))
        .collect();
    for index in 0..peers.len() {
        peers[index].send("join wide\n");
        for peer in &mut peers[..=index] {
            peer.expect(&format!("joined wide {}\n", 20 + index));
        }
    }
    peers.last_mut().unwrap().send("say wide broadcast\n");
    for peer in &mut peers {
        peer.expect(&format!("message wide {} wide broadcast\n", 19 + members));
    }
}

#[test]
fn chat_real_tcp_on_all_backends() {
    let work = scratch::ScratchDir::new("chat");
    let source = include_str!("../examples/chat.jk");
    assert_eq!(source.matches("0x7f000001, 7777").count(), 1);
    let path = work.path().join("chat.jk");
    let source = source.replace("0x7f000001, 7777", "0x7f000001, 0").replace(
        "fn main() @suspends -> Int32 {",
        "fn main() @suspends -> Int32 {\n    verify_chat_queue_domain()",
    );
    std::fs::write(
        &path,
        format!(
            "import joky/check\nimport joky/string.{{type StringBuilder}}\n{source}\n{}",
            include_str!("fixtures/chat_queue.jk")
        ),
    )
    .unwrap();
    let binary = work.path().join("chat");
    let built = Command::new(env!("CARGO_BIN_EXE_joky"))
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&binary)
        .env("JOKY_RUNTIME", runtime::runtime_archive())
        .output()
        .unwrap();
    assert!(built.status.success(), "{built:?}");
    for backend in ["jit", "interpreter", "aot"] {
        eprintln!("chat backend: {backend}");
        let mut command = if backend == "aot" {
            Command::new(&binary)
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_joky"));
            command.arg("run");
            if backend == "interpreter" {
                command.arg("--interpreter");
            }
            command.arg(&path);
            command
        };
        command.current_dir(work.path());
        let (mut server, port) = Server::start(command, work.path().join(format!("{backend}.log")));
        conversation(port, if backend == "interpreter" { 70 } else { 260 });
        assert!(server.child.try_wait().unwrap().is_none());
    }
}
