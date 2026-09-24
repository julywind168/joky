use super::*;
use std::ffi::OsString;
use std::process::Command;
use std::sync::atomic::Ordering;

#[cfg(unix)]
fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[test]
fn environment_replacement_and_validation_do_not_modify_parent() {
    let inherited = vec![
        (OsString::from("KEEP"), OsString::from("parent")),
        ("REMOVE".into(), "value".into()),
    ];
    let mut spec = CommandSpec {
        program: std::env::current_exe()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        env: vec![
            ("KEEP".into(), Some("child".into())),
            ("REMOVE".into(), None),
        ],
        ..Default::default()
    };
    let command = runner::configure(&spec, &inherited).unwrap();
    let entries: Vec<_> = command
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned())))
        .collect();
    assert!(entries.contains(&("KEEP".into(), Some("child".into()))));
    assert!(!entries.iter().any(|(k, _)| k == "REMOVE"));
    assert_eq!(inherited[0].1, "parent");
    spec.replace = true;
    spec.env.clear();
    assert_eq!(
        runner::configure(&spec, &inherited)
            .unwrap()
            .get_envs()
            .count(),
        0
    );
    spec.env.push(("BAD=NAME".into(), Some("x".into())));
    assert!(runner::configure(&spec, &inherited).is_err());
}

#[cfg(unix)]
#[test]
fn status_nonzero_signal_and_missing_program_are_distinct() {
    let cancelled = AtomicBool::new(false);
    let result = runner::run(shell("exit 7"), None, &cancelled).unwrap();
    assert_eq!(result.status.code(), Some(7));
    use std::os::unix::process::ExitStatusExt;
    let result = runner::run(shell("kill -TERM $$"), None, &cancelled).unwrap();
    assert_eq!(result.status.signal(), Some(libc::SIGTERM));
    assert!(runner::run(
        Command::new("/definitely-missing/joky-command"),
        None,
        &cancelled
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn capture_drains_both_pipes_while_writing_large_stdin() {
    let cancelled = AtomicBool::new(false);
    // Each stream exceeds the OS pipe capacity. A sequential implementation
    // would block writing stdin before it ever drains stdout/stderr.
    let script = "dd if=/dev/zero bs=65536 count=2 2>/dev/null; dd if=/dev/zero bs=65536 count=2 1>&2 2>/dev/null; cat";
    let input = vec![b'x'; 256 * 1024];
    let result = runner::run(
        shell(script),
        Some((input.clone(), 1024 * 1024)),
        &cancelled,
    )
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout.len(), 128 * 1024 + input.len());
    assert_eq!(&result.stdout[128 * 1024..], &input);
    assert_eq!(result.stderr, vec![0; 128 * 1024]);
}

#[cfg(unix)]
#[test]
fn output_limit_and_empty_input_are_enforced() {
    let cancelled = AtomicBool::new(false);
    let error = runner::run(
        shell("printf 1234; printf 5678 >&2"),
        Some((Vec::new(), 7)),
        &cancelled,
    )
    .err()
    .unwrap();
    assert!(error.contains("output limit"), "{error}");
    let result = runner::run(shell("cat; printf done"), Some((Vec::new(), 4)), &cancelled).unwrap();
    assert_eq!(result.stdout, b"done");
    assert!(runner::run(shell("printf x"), Some((Vec::new(), 0)), &cancelled).is_err());
}

#[cfg(unix)]
#[test]
fn cancellation_reaps_child_and_closes_pipe_workers() {
    use std::io::Read;
    // A separate inherited fd is unnecessary: publish pid to a test file and
    // wait for that observable child action before requesting cancellation.
    let path = std::env::temp_dir().join(format!("joky-child-cancel-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let mut command = shell("printf '%s' $$ > \"$1\"; exec /bin/sleep 60");
    command.arg("sh").arg(&path);
    // A large input keeps all pipe machinery busy until cancellation.
    let worker = std::thread::spawn(move || {
        runner::run(
            command,
            Some((vec![0; 128 * 1024], 64 * 1024 * 1024)),
            &flag,
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let pid = loop {
        let mut value = String::new();
        if let Ok(mut file) = std::fs::File::open(&path) {
            file.read_to_string(&mut value).unwrap();
            if let Ok(pid) = value.parse::<i32>() {
                break pid;
            }
        }
        if std::time::Instant::now() >= deadline {
            cancelled.store(true, Ordering::Release);
            let _ = worker.join();
            panic!("child did not publish pid");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    cancelled.store(true, Ordering::Release);
    assert!(worker.join().unwrap().is_err());
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cancelled_request_never_spawns_and_malformed_shapes_are_rejected() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("--list");
    let error = runner::run(command, None, &AtomicBool::new(true))
        .err()
        .unwrap();
    assert!(error.contains("before spawn"));
    for capture in [false, true] {
        unsafe {
            assert_eq!(
                start_request(
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                    0,
                    0,
                    capture,
                    false
                ),
                0
            );
            // Null class payload must fail decoding before reading any fields.
            let arguments = [0usize; 3];
            assert!(protocol::decode(arguments.as_ptr().cast(), capture).is_err());
        }
    }
}

#[cfg(unix)]
#[test]
fn refused_completions_release_output_buffers_and_errors() {
    use crate::runtime::continuation::{
        jk_continuation_cancel, jk_continuation_free, jk_continuation_new,
    };
    use crate::runtime::scope::RuntimeScope;
    use std::os::unix::process::ExitStatusExt;
    let scope = RuntimeScope::new();
    let _guard = scope.enter();
    let handle = jk_continuation_new(2);
    let continuation = unsafe { Continuation::retain_registered(handle) }.unwrap();
    unsafe {
        assert_eq!(jk_continuation_cancel(handle), 1);
        jk_continuation_free(handle);
    }
    for capture in [false, true] {
        protocol::complete(
            &continuation,
            handle,
            0,
            capture,
            Ok(runner::Outcome {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![0xff, 0],
                stderr: b"error".to_vec(),
            }),
        );
        protocol::complete(&continuation, handle, 0, capture, Err("spawn error".into()));
    }
    for capture in [false, true] {
        protocol::complete_pipeline(
            &continuation,
            handle,
            0,
            capture,
            Ok(runner::PipelineOutcome {
                statuses: vec![
                    std::process::ExitStatus::from_raw(0),
                    std::process::ExitStatus::from_raw(7 << 8),
                ],
                stdout: vec![0xff, 0],
                stderrs: vec![vec![], b"error".to_vec()],
            }),
        );
        protocol::complete_pipeline(
            &continuation,
            handle,
            0,
            capture,
            Err("pipeline error".into()),
        );
    }
    assert_eq!(scope.managed_objects.load(Ordering::Acquire), 0);
}

#[cfg(unix)]
#[test]
fn pipeline_streams_large_input_and_preserves_stage_order() {
    let cancelled = AtomicBool::new(false);
    let input = vec![0xa5; 512 * 1024];
    let commands = vec![
        shell("printf first >&2; cat; exit 7"),
        shell("dd if=/dev/zero bs=65536 count=2 1>&2 2>/dev/null; cat"),
        shell("printf last >&2; cat"),
    ];
    let output =
        runner::run_pipeline(commands, Some((input.clone(), 1024 * 1024)), &cancelled).unwrap();
    assert_eq!(output.stdout, input);
    assert_eq!(
        output.stderrs,
        vec![b"first".to_vec(), vec![0; 128 * 1024], b"last".to_vec()]
    );
    assert_eq!(
        output.statuses.iter().map(|s| s.code()).collect::<Vec<_>>(),
        vec![Some(7), Some(0), Some(0)]
    );
}

#[cfg(unix)]
#[test]
fn pipeline_limit_excludes_intermediate_streams_but_includes_every_stderr() {
    let cancelled = AtomicBool::new(false);
    let commands = vec![
        shell("dd if=/dev/zero bs=65536 count=4 2>/dev/null"),
        shell("cat >/dev/null; printf ok"),
    ];
    let output = runner::run_pipeline(commands, Some((vec![], 2)), &cancelled).unwrap();
    assert_eq!(output.stdout, b"ok");
    let commands = vec![
        shell("printf 123 >&2"),
        shell("cat; printf 456 >&2; printf x"),
    ];
    let error = runner::run_pipeline(commands, Some((vec![], 6)), &cancelled)
        .err()
        .unwrap();
    assert!(error.contains("output limit"), "{error}");
    assert!(runner::run_pipeline(vec![], None, &cancelled).is_err());
}

#[cfg(unix)]
#[test]
fn downstream_early_exit_closes_parent_pipe_copies() {
    let output = runner::run_pipeline(
        vec![Command::new("/usr/bin/yes"), shell("head -c 1")],
        Some((vec![], 1024)),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(output.stdout.len(), 1);
    assert!(output.statuses[1].success());
    assert!(!output.statuses[0].success());
}

#[cfg(unix)]
fn wait_pid(path: &std::path::Path) -> i32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(value) = std::fs::read_to_string(path) {
            if let Ok(pid) = value.parse() {
                return pid;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stage did not publish pid"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(unix)]
fn assert_reaped(pid: i32) {
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "stage {pid} survived cleanup"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(unix)]
#[test]
fn cancellation_reaps_all_pipeline_stages() {
    let root = std::env::temp_dir().join(format!("joky-pipeline-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let paths = [root.join("first"), root.join("last")];
    let commands = paths
        .iter()
        .map(|path| {
            let mut command = shell("printf '%s' $$ > \"$1\"; exec /bin/sleep 60");
            command.arg("sh").arg(path);
            command
        })
        .collect();
    let flag = Arc::new(AtomicBool::new(false));
    let cancellation = flag.clone();
    let worker = std::thread::spawn(move || {
        runner::run_pipeline(commands, Some((vec![0; 128 * 1024], 1024)), &cancellation)
    });
    let pids = paths.each_ref().map(|path| wait_pid(path));
    flag.store(true, Ordering::Release);
    assert!(worker.join().unwrap().is_err());
    for pid in pids {
        assert_reaped(pid);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn partial_startup_failure_terminates_already_started_stages() {
    let root = std::env::temp_dir().join(format!("joky-pipeline-startup-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let pidfile = root.join("pid");
    let invalid = root.join("invalid");
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(&invalid, "#!/definitely-missing/interpreter\n").unwrap();
    std::fs::set_permissions(&invalid, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Block spawn of the second stage until the first has observably started.
    let mut first = shell("printf '%s' $$ > \"$1\"; exec /bin/sleep 60");
    first.arg("sh").arg(&pidfile);
    let mut last = Command::new(invalid);
    let handshake = pidfile.clone();
    use std::os::unix::process::CommandExt;
    // pre_exec must be async-signal-safe. Use only libc operations in the child.
    let name = std::ffi::CString::new(handshake.as_os_str().as_encoded_bytes()).unwrap();
    unsafe {
        last.pre_exec(move || {
            for _ in 0..10000 {
                let fd = libc::open(name.as_ptr(), libc::O_RDONLY);
                if fd >= 0 {
                    let mut byte = 0u8;
                    let count = libc::read(fd, (&mut byte as *mut u8).cast(), 1);
                    libc::close(fd);
                    if count == 1 {
                        return Ok(());
                    }
                }
                libc::usleep(1000);
            }
            Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT))
        });
    }
    let error = runner::run_pipeline(
        vec![first, last],
        Some((vec![], 1024)),
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    assert!(error.contains("stage 2 spawn failed"), "{error}");
    assert_reaped(wait_pid(&pidfile));
    std::fs::remove_dir_all(root).unwrap();
}
