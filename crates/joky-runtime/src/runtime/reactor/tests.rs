use super::*;
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

#[test]
fn timer_callback_runs_on_the_single_reactor() {
    let fired = Arc::new(AtomicBool::new(false));
    let callback_fired = Arc::clone(&fired);
    register_timer(
        Instant::now() + Duration::from_millis(1),
        Box::new(move || callback_fired.store(true, Ordering::Release)),
    );
    for _ in 0..100 {
        if fired.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(fired.load(Ordering::Acquire));
}

#[test]
fn cancelled_timer_still_runs_its_cleanup_callback() {
    let fired = Arc::new(AtomicBool::new(false));
    let callback_fired = Arc::clone(&fired);
    let id = register_timer(
        Instant::now() + Duration::from_secs(60),
        Box::new(move || callback_fired.store(true, Ordering::Release)),
    );
    cancel_timer(id);
    for _ in 0..100 {
        if fired.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(fired.load(Ordering::Acquire));
}

#[test]
fn cancellation_before_timer_registration_runs_the_cancel_hook() {
    let reactor = reactor();
    let id = reactor.reserve(true);
    cancel_timer(id);
    let cancelled = Arc::new(AtomicBool::new(false));
    let callback_ran = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::clone(&cancelled);
    let callback_flag = Arc::clone(&callback_ran);
    {
        let mut commands = reactor.commands.lock().expect("reactor command mutex");
        commands.push_back(ReactorCommand::Register {
            id,
            deadline: Instant::now() + Duration::from_secs(60),
            callback: Box::new(move || callback_flag.store(true, Ordering::Release)),
            on_cancel: Some(Box::new(move || cancel_flag.store(true, Ordering::Release))),
        });
    }
    reactor.wake.wake().expect("failed to wake Joky reactor");
    for _ in 0..100 {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(cancelled.load(Ordering::Acquire));
    assert!(!callback_ran.load(Ordering::Acquire));
}

#[test]
fn tcp_readiness_callback_runs_on_reactor_thread() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let address = listener.local_addr().expect("listener address");
    let client = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(address).expect("connect test stream");
        stream.write_all(b"ready").expect("write test bytes");
    });
    let (server, _) = listener.accept().expect("accept test stream");
    server
        .set_nonblocking(true)
        .expect("set server stream nonblocking");
    let stream = Arc::new(Mutex::new(TcpStream::from_std(server)));
    let (ready_sender, ready_receiver) = mpsc::channel();
    register_tcp(
        Arc::clone(&stream),
        Interest::READABLE,
        Box::new(move |_event, stream| {
            let mut bytes = [0_u8; 5];
            let read = stream
                .lock()
                .expect("stream mutex")
                .read(&mut bytes)
                .expect("read test bytes");
            ready_sender
                .send(bytes[..read].to_vec())
                .expect("send readiness result");
            true
        }),
    );
    let bytes = ready_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("readiness callback should run");
    assert_eq!(bytes, b"ready");
    client.join().expect("client should finish");
}

#[test]
fn failed_tcp_registration_reports_an_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let address = listener.local_addr().expect("listener address");
    let client = std::thread::spawn(move || {
        let _stream = std::net::TcpStream::connect(address).expect("connect test stream");
        thread::sleep(Duration::from_millis(50));
    });
    let (server, _) = listener.accept().expect("accept test stream");
    server
        .set_nonblocking(true)
        .expect("set server stream nonblocking");
    let stream = Arc::new(Mutex::new(TcpStream::from_std(server)));
    let first = register_tcp(
        Arc::clone(&stream),
        Interest::READABLE,
        Box::new(|_event, _stream| false),
    );
    let (error_sender, error_receiver) = mpsc::channel();
    let _second = register_tcp_with_error(
        Arc::clone(&stream),
        Interest::READABLE,
        Box::new(|_event, _stream| true),
        Some(Box::new(move |error| {
            error_sender
                .send(error.kind())
                .expect("send registration error");
        })),
    );
    let error = error_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("duplicate registration should report an error");
    assert_eq!(error, std::io::ErrorKind::AlreadyExists);
    cancel_io(first);
    client.join().expect("client should finish");
}
