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
            let mut stdout = BufReader::new(stdout);
            let result = stdout.read_line(&mut port).map(|_| port);
            let _ = sender.send(result);
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("server must announce its bound port")
            .unwrap();
        let port = line
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("server did not announce a port: {line}"));
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

fn conversation(port: u16, members: u64, logic_path: &std::path::Path, logic: &str) {
    let mut alice = Peer::connect(port, 1);
    let mut bob = Peer::connect(port, 2);
    let mut carol = Peer::connect(port, 3);
    alice.send("balance\nsay before joining\nbalance\n");
    alice.expect("balance 5\n");
    alice.expect("error join a room first\n");
    alice.expect("balance 5\n");
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
    // Publish a business change without replacing either socket or membership.
    let patched = logic.replace(
        "event(\"message\", room, id, text)",
        "event(\"message-v2\", room, id, text)",
    );
    assert_ne!(patched, logic);
    std::fs::write(logic_path, &patched).unwrap();
    alice.send("reload\n");
    alice.expect("reloaded\n");
    bob.send("say updated\n");
    alice.expect("message-v2 123 2 updated\n");
    bob.expect("message-v2 123 2 updated\n");
    for (index, invalid) in [
        format!("{patched}\nfn broken("),
        patched.replace("balance_cents", "credit_cents"),
        patched.replace("fn dispatch(", "fn missing_dispatch("),
        patched.replace(
            "fn dispatch(request: Call) @suspends -> UInt64",
            "fn dispatch(request: Call) @suspends -> Int32",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(logic_path, invalid).unwrap();
        alice.send("reload\n");
        alice.expect("reload failed\n");
        if index < 2 {
            bob.send("say preserved\n");
            alice.expect("message-v2 123 2 preserved\n");
            bob.expect("message-v2 123 2 preserved\n");
        } else {
            bob.send("balance\n");
            bob.expect("balance 2\n");
        }
    }
    std::fs::remove_file(logic_path).unwrap();
    alice.send("reload\n");
    alice.expect("reload failed\n");
    std::fs::write(logic_path, logic).unwrap();
    alice.send("reload\n");
    alice.expect("reloaded\n");
    alice.quiet();
    bob.quiet();

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
        bob.expect("error use join <room>, leave, say <message>, or balance\n");
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
        } else if id == 6 {
            peer.send("crash\n");
            peer.closed();
        }
        drop(peer);
        alice.expect(&format!("left 123 {id}\n"));
    }
    carol.quiet();
    alice.send("say still running\n");
    alice.expect("message 123 1 still running\n");
    alice.send("balance\nsay too expensive\nbalance\n");
    alice.expect("balance 0\n");
    alice.expect("error insufficient balance\n");
    alice.expect("balance 0\n");
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
    witness.send("say concurrent\n");
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
            "message 123 16 concurrent\n"
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
    // Empty rooms retire their owned roots without parked initializer frames.
    // One connection crosses the former parked-owner frame limit by changing rooms.
    let mut previous = "房间".to_owned();
    for index in 0..224 {
        let room = format!("retained-{index}");
        carol.send(format!("join {room}\n"));
        carol.expect(&format!("left {previous} 3\n"));
        carol.expect(&format!("joined {room} 3\n"));
        previous = room;
    }
    carol.send("join retained-0\n");
    carol.expect(&format!("left {previous} 3\n"));
    carol.expect("joined retained-0 3\n");
    carol.send("balance\n");
    carol.expect("balance 4\n");
    // Native backends also cross the former 256-message batch boundary.
    // Interpreter sessions still have a 1024 live-frame budget; 40 TCP members
    // leave headroom, while verify_wide_broadcast covers 260 targets directly.
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
#[ignore = "manual: real TCP chat matrix; run with cargo test --test chat -- --ignored"]
fn chat_real_tcp_on_all_backends() {
    let work = scratch::ScratchDir::new("chat");
    let source = include_str!("../examples/chat.jk");
    std::fs::write(
        work.path().join("chat_contract.jk"),
        include_str!("../examples/chat_contract.jk"),
    )
    .unwrap();
    assert_eq!(source.matches("0x7f000001, 7777").count(), 1);
    assert_eq!(
        source
            .matches("    let directory = domain.directory(1)")
            .count(),
        1
    );
    let path = work.path().join("chat.jk");
    let source = source.replace("import joky/vm", "import joky/vm\nimport joky/check")
        .replace("0x7f000001, 7777", "0x7f000001, 0")
        .replace("examples/chat_logic.jk", "chat_logic.jk")
        .replace("examples/chat_contract.jk", "chat_contract.jk")
        .replace("    let directory = domain.directory(1)", "    engine.call::<Unit,Unit>(\"verify\", {});\n    verify_publication(&engine)\n    let directory = domain.directory(1)");
    let source = format!("{source}\n{}", include_str!("fixtures/chat_reload_host.jk"));
    for create in ["new", "interpreter"] {
        let source = source.replace("vm.Vm.new()", &format!("vm.Vm.{create}()"));
        std::fs::write(&path, &source).unwrap();
        let logic = format!(
        "import joky/check\nimport joky/string.{{type StringBuilder}}\n{}\n{}\n{}\nfn verify(_:Unit) @suspends {{verify_chat_queue_domain()}}",
        include_str!("../examples/chat_logic.jk").replace("type AccountInit}", "type AccountInit, defaults}"), include_str!("fixtures/chat_queue.jk"), include_str!("fixtures/chat_reload.jk")
    );
        assert_eq!(
            logic
                .matches("if !value.route.live { return Step.Done(false) }")
                .count(),
            1
        );
        let logic = logic.replace(
            "Action.Line(line) => if process(ctx, id, line) { 1 } else { 0 },",
            "Action.Line(line) => { if line.equals(\"crash\") { let bad = 1 / 0; }; if process(ctx, id, line) { 1 } else { 0 } },",
        );
        let logic = logic.replace(
            "if !value.route.live { return Step.Done(false) }",
            "if !value.route.live { return Step.Done(false) }\n        pause_send(ctx, &command)",
        );
        std::fs::write(work.path().join("chat_logic.jk"), &logic).unwrap();
        let next = logic
            .replace(
                "event(\"message\", room, id, text)",
                "event(\"probe-v2\", room, id, text)",
            )
            .replace("account.balance_cents < 100", "account.balance_cents < 200")
            .replace(
                "account.balance_cents -= 100",
                "account.balance_cents -= 200",
            );
        assert_ne!(next, logic);
        std::fs::write(work.path().join("chat_logic_next.jk"), next).unwrap();
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
            eprintln!("chat backend: {backend}, script: {create}");
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
            let (mut server, port) =
                Server::start(command, work.path().join(format!("{backend}.log")));
            conversation(
                port,
                if backend == "interpreter" { 40 } else { 260 },
                &work.path().join("chat_logic.jk"),
                &logic,
            );
            assert!(server.child.try_wait().unwrap().is_none());
        }
    }
}
