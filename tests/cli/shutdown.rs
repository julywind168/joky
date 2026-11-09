//! Signals run only in child processes, never in the test runner.
use super::tcp_server::{connect, Running};
use super::*;
use std::os::unix::process::ExitStatusExt;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};
#[test]
fn shutdown_accept_until_cancellation_preserves_listener_in_all_backends() {
    let work = Workspace::new();
    work.write(
        "accept_until.jk",
        include_str!("../fixtures/tcp_accept_until.jk"),
    );
    work.assert_all_backends("accept_until.jk", &[], b"", 0);
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
    success(&work.cli(&["build", "server.jk"]));
    // Cover signal routing in both VM modes and a source-independent native binary.
    // Native uses the management request, which shares the same notification.
    for backend in ["jit", "interpreter", "aot"] {
        let command = if backend == "aot" {
            Command::new(work.path().join("server"))
        } else {
            let mut c = work.command();
            c.args(["run", "server.jk"]);
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
fn subscribed_wait() @suspends -> Int32 {
    let signals = shutdown.with_signals().unwrap()
    signals.controller().wait();
    0
}
fn failed_scope() @suspends {
    let signals = shutdown.with_signals().unwrap()
    assert(!signals.controller().requested(), "new scope is not stopped")
    assert(false, "intentional signal scope failure")
}
fn main() @suspends {
    let stop = shutdown.create()
    var i = 0
    while i < 10 {
        race { subscribed_wait(), { task.yield_now(); 0 } };
        let failed = isolate { failed_scope() }
        assert(failed.is_err(), "failed signal scope is cleaned up")
        i += 1
    }
    var subscriptions: Array<shutdown.Signals> = []
    i = 0
    while i < 8 { subscriptions.push(stop.subscribe_unix().unwrap()); i += 1 }
    assert(match stop.subscribe_unix() { Err(Busy) => true, _ => false }, "subscription limit")
    assert(match shutdown.with_signals() { Err(Busy) => true, _ => false }, "owning scope forwards subscription failure")
    mem.drop(subscriptions)
    { let old = stop.subscribe_unix().unwrap(); mem.drop(old) }
    let first = stop.subscribe_unix().unwrap()
    let second = shutdown.with_signals().unwrap()
    let shared = second.controller().share()
    mem.drop(first)
    println("ready")
    match second.controller().wait() { Interrupt => println("interrupt"), Terminate => println("terminate"), _ => println("wrong") }
    assert(!shared.request(), "signal reason remains first")
    mem.drop(second)
    assert(shared.requested(), "shared notification survives without retaining signal subscription")
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
