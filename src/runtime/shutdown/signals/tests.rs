//! Process-wide signal state is changed only in an isolated test child.
use super::*;
use std::{
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn signal_pipe_reader_outlives_close_and_callback_unregistration() {
    const CHILD: &str = "JOKY_SIGNAL_PIPE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal pipe child did not finish");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }

    // Rust startup ignores SIGPIPE, unlike standalone AOT executables. Restore
    // the default in this child so an unsafe socket wake actually kills it.
    unsafe {
        assert_ne!(libc::signal(libc::SIGPIPE, libc::SIG_DFL), libc::SIG_ERR);
    }
    let (delivery, pipe) = SignalPipe::new().unwrap();
    let reader = pipe._read.as_raw_fd();
    // Force the adverse ordering instead of hoping to hit the scheduling window:
    // the worker's read descriptor is gone before close writes its wake byte.
    std::thread::spawn(move || drop(delivery)).join().unwrap();
    pipe.handle.close();
    pipe.handle.close();
    // close() leaves callbacks registered. They must also retain a live reader.
    for signal in [SIGINT, SIGTERM] {
        signal_hook::low_level::raise(signal).unwrap();
    }
    drop(pipe);
    assert_eq!(unsafe { libc::fcntl(reader, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));

    // Exercise actual Subscription teardown while another subscription prevents
    // default termination and signals can arrive during unregistration.
    let stop = Shutdown::new();
    let anchor = stop.subscribe_unix().unwrap();
    let sending = Arc::new(AtomicBool::new(true));
    let sender_flag = sending.clone();
    let sender = std::thread::spawn(move || {
        while sender_flag.load(Ordering::Relaxed) {
            signal_hook::low_level::raise(SIGINT).unwrap();
            std::thread::yield_now();
        }
    });
    for _ in 0..64 {
        drop(Shutdown::new().subscribe_unix().unwrap());
    }
    sending.store(false, Ordering::Relaxed);
    sender.join().unwrap();
    drop(anchor);
    // No global ignore was installed to hide the race.
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    unsafe {
        assert_eq!(
            libc::sigaction(libc::SIGPIPE, std::ptr::null(), action.as_mut_ptr()),
            0
        );
        assert_eq!(action.assume_init().sa_sigaction, libc::SIG_DFL);
    }
}
