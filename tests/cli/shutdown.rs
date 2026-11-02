//! Signals run only in child processes, never in the test runner.
use super::*;
use std::os::unix::process::ExitStatusExt;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Running {
    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
    fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(30))
            .expect("child output deadline")
    }
    fn signal(&self, signal: &str) {
        assert!(Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()
            .unwrap()
            .success());
    }
    fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "child exit deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
}
#[test]
fn shutdown_notification_fixture_executes_in_standalone_aot() {
    let work = Workspace::new();
    work.write("shutdown.jk", include_str!("../fixtures/shutdown.jk"));
    work.assert_native("shutdown.jk", &["--no-prelude"], b"", 0);
}
#[test]
fn shutdown_tcp_stops_accepting_drains_and_cancels_at_deadline() {
    let work = Workspace::new();
    work.write("server.jk", include_str!("../../examples/tcp_shutdown.jk"));
    success(&work.cli(&["build", "server.jk", "--no-prelude"]));
    // Cover signal routing in both VM modes and a source-independent native binary.
    // Native uses the management request, which shares the same notification.
    for backend in ["jit", "interpreter", "aot"] {
        let command = if backend == "aot" {
            Command::new(work.path().join("server"))
        } else {
            let mut c = work.command();
            c.args(["run", "server.jk", "--no-prelude"]);
            if backend == "interpreter" {
                c.arg("--interpreter");
            }
            c
        };
        let mut process = Running::start(command);
        let ready = process.line();
        let port: u16 = ready.strip_prefix("listening ").unwrap().parse().unwrap();
        let mut idle = connect(port);
        let mut active = connect(port);
        active
            .write_all(if backend == "interpreter" {
                b"slow\n"
            } else {
                b"echo\n"
            })
            .unwrap();
        assert_eq!(process.line(), "accepted");
        if backend == "aot" {
            let mut admin = connect(port);
            admin.write_all(b"shutdown\n").unwrap();
            let mut reply = String::new();
            admin.read_to_string(&mut reply).unwrap();
            assert_eq!(reply, "shutting down\n");
        } else {
            process.signal(if backend == "jit" { "-INT" } else { "-TERM" });
        }
        assert_eq!(process.line(), "stopped accepting");
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "listener remained open"
        );
        let mut byte = [0];
        assert_eq!(
            idle.read(&mut byte).unwrap(),
            0,
            "idle connection did not stop"
        );
        if backend == "interpreter" {
            assert_eq!(process.line(), "deadline reached");
            assert_eq!(process.finish().code(), Some(1));
            assert_eq!(
                active.read(&mut byte).unwrap(),
                0,
                "cancelled socket remained open"
            );
        } else {
            let mut reply = String::new();
            active.read_to_string(&mut reply).unwrap();
            assert_eq!(reply, "done\n");
            assert_eq!(process.line(), "drained");
            assert!(process.finish().success());
        }
        // The listening socket and all connection owners have left the task tree.
        drop(TcpListener::bind(("127.0.0.1", port)).unwrap());
    }
}
#[test]
fn shutdown_signal_subscription_is_explicit_repeatable_and_scoped() {
    let work = Workspace::new();
    work.write("signals.jk", r#"
import joky/shutdown
import joky/mem
import joky/task
import joky/array.{type Array}
import joky/result.{Err}
import joky/check.{assert}
import joky/io.{println}
fn subscribed_wait(stop: &shutdown.Controller) @suspends -> Int32 {
    let signals = stop.subscribe_unix().unwrap()
    stop.wait();
    mem.drop(signals)
    0
}
fn main() @suspends {
    let stop = shutdown.create()
    var i = 0
    while i < 10 {
        race { | subscribed_wait(&stop) | { task.yield_now(); 0 } };
        i += 1
    }
    var subscriptions: Array<shutdown.Signals> = []
    i = 0
    while i < 8 { subscriptions.push(stop.subscribe_unix().unwrap()); i += 1 }
    assert(match stop.subscribe_unix() { Err(Busy) => true, _ => false }, "subscription limit")
    mem.drop(subscriptions)
    { let old = stop.subscribe_unix().unwrap(); mem.drop(old) }
    let first = stop.subscribe_unix().unwrap()
    let second = stop.subscribe_unix().unwrap()
    mem.drop(first)
    println("ready")
    match stop.wait() { Interrupt => println("interrupt"), Terminate => println("terminate"), _ => println("wrong") }
    assert(!stop.request(), "signal reason remains first")
    mem.drop(second)
    println("unsubscribed")
    let never = shutdown.create()
    never.wait();
}
"#);
    success(&work.cli(&["build", "signals.jk", "--no-prelude"]));
    for signal in ["-INT", "-TERM"] {
        let mut process = Running::start(Command::new(work.path().join("signals")));
        assert_eq!(process.line(), "ready");
        process.signal(signal);
        assert_eq!(
            process.line(),
            if signal == "-INT" {
                "interrupt"
            } else {
                "terminate"
            }
        );
        assert_eq!(process.line(), "unsubscribed");
        process.signal("-TERM");
        assert_eq!(process.finish().signal(), Some(15));
    }
    work.write("plain.jk", "import joky/shutdown import joky/io.{println} fn main() @suspends { let stop = shutdown.create(); println(\"ready\"); stop.wait(); }");
    let mut command = work.command();
    command.args(["run", "plain.jk", "--interpreter", "--no-prelude"]);
    let mut process = Running::start(command);
    assert_eq!(process.line(), "ready");
    process.signal("-TERM");
    assert_eq!(process.finish().signal(), Some(15));
}
