//! Dedicated child supervisors; no process waits or pipe I/O occupy the file
//! blocking pool. Each capture drains both output pipes while writing stdin.
use super::{CaptureInput, CommandSpec};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(super) struct Outcome {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(super) struct PipelineOutcome {
    pub statuses: Vec<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderrs: Vec<Vec<u8>>,
}

fn environment(
    spec: &CommandSpec,
    inherited: &[(OsString, OsString)],
) -> BTreeMap<OsString, OsString> {
    let mut result = if spec.replace {
        BTreeMap::new()
    } else {
        inherited.iter().cloned().collect()
    };
    for (key, value) in &spec.env {
        #[cfg(windows)]
        if let Some(old) = result
            .keys()
            .find(|old| super::super::env::names_equal(old, OsStr::new(key)))
            .cloned()
        {
            result.remove(&old);
        }
        match value {
            Some(value) => {
                result.insert(key.into(), value.into());
            }
            None => {
                result.remove(OsStr::new(key));
            }
        }
    }
    result
}

pub(super) fn configure(
    spec: &CommandSpec,
    inherited: &[(OsString, OsString)],
) -> Result<Command, String> {
    if spec.program.is_empty()
        || spec.program.contains('\0')
        || spec.args.iter().any(|v| v.contains('\0'))
    {
        return Err("invalid process program or argument".into());
    }
    if spec.cwd.as_ref().is_some_and(|v| v.contains('\0')) {
        return Err("invalid process working directory".into());
    }
    for (key, value) in &spec.env {
        if key.is_empty()
            || key.contains(['=', '\0'])
            || value.as_ref().is_some_and(|v| v.contains('\0'))
        {
            return Err("invalid child environment variable".into());
        }
    }
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let cwd = spec
        .cwd
        .as_ref()
        .map(|value| current.join(value))
        .unwrap_or(current);
    let env = environment(spec, inherited);
    let program = resolve_program(&spec.program, &cwd, &env)?;
    #[cfg(windows)]
    if program
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        // Rust's Command would otherwise implicitly invoke cmd.exe.
        return Err("batch programs require an explicitly configured shell".into());
    }
    let mut command = Command::new(program);
    command
        .args(&spec.args)
        .current_dir(cwd)
        .env_clear()
        .envs(env);
    Ok(command)
}

fn resolve_program(
    program: &str,
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
) -> Result<PathBuf, String> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    if program.chars().any(std::path::is_separator) || path.components().count() != 1 {
        return Ok(cwd.join(path));
    }
    let search = env
        .iter()
        .find(|(key, _)| super::super::env::names_equal(key, OsStr::new("PATH")))
        .map(|(_, value)| value)
        .ok_or_else(|| "child PATH is unset; use an explicit program path".to_owned())?;
    for directory in std::env::split_paths(search) {
        let candidate = cwd.join(directory).join(program);
        if executable(&candidate) {
            return Ok(candidate);
        }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            let candidate = candidate.with_extension("exe");
            if executable(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(format!("program not found in child PATH: {program}"))
}
fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// Terminate all stages before waiting for any one of them. This guard also
// handles partial startup, pipe setup failures and errors while polling.
struct Children(Vec<Child>);
impl Children {
    fn terminate(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
        }
    }
}
impl Drop for Children {
    fn drop(&mut self) {
        self.terminate();
        for child in &mut self.0 {
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
fn prepare(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(unix))]
fn prepare<T>(_: &T) -> io::Result<()> {
    Ok(())
}

fn pause() {
    thread::sleep(Duration::from_millis(2));
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

struct Pipes {
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<io::Result<Vec<u8>>>>,
}
impl Pipes {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            workers: Vec::new(),
        }
    }
    fn reader(
        &mut self,
        mut reader: impl Read + Send + 'static,
        total: Arc<AtomicUsize>,
        limit: usize,
    ) -> io::Result<()> {
        let stop = self.stop.clone();
        self.workers.push(
            thread::Builder::new()
                .name("joky-process-read".into())
                .spawn(move || {
                    let mut output = Vec::new();
                    let mut buffer = [0u8; 8192];
                    let result = loop {
                        if stop.load(Ordering::Acquire) {
                            break Err(io::Error::other("process pipe cancelled"));
                        }
                        match reader.read(&mut buffer) {
                            Ok(0) => break Ok(output),
                            Ok(count) => {
                                if total
                                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                        n.checked_add(count).filter(|n| *n <= limit)
                                    })
                                    .is_err()
                                {
                                    break Err(io::Error::other("process output limit exceeded"));
                                }
                                output.extend_from_slice(&buffer[..count]);
                            }
                            Err(_) if stop.load(Ordering::Acquire) => {
                                break Err(io::Error::other("process pipe cancelled"));
                            }
                            Err(error) if retry(&error) => pause(),
                            Err(error) => break Err(error),
                        }
                    };
                    if result.is_err() {
                        stop.store(true, Ordering::Release);
                    }
                    result
                })?,
        );
        Ok(())
    }
    fn writer(
        &mut self,
        mut writer: impl Write + Send + 'static,
        input: Vec<u8>,
    ) -> io::Result<()> {
        let stop = self.stop.clone();
        self.workers.push(
            thread::Builder::new()
                .name("joky-process-write".into())
                .spawn(move || {
                    let mut offset = 0;
                    while offset < input.len() {
                        if stop.load(Ordering::Acquire) {
                            return Ok(Vec::new());
                        }
                        match writer.write(&input[offset..input.len().min(offset + 8192)]) {
                            Ok(0) => {
                                stop.store(true, Ordering::Release);
                                return Err(io::ErrorKind::WriteZero.into());
                            }
                            Ok(count) => offset += count,
                            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
                            Err(_) if stop.load(Ordering::Acquire) => break,
                            Err(error) if retry(&error) => pause(),
                            Err(error) => {
                                stop.store(true, Ordering::Release);
                                return Err(error);
                            }
                        }
                    }
                    // Dropping ChildStdin is the EOF signal, including empty input.
                    Ok(Vec::new())
                })?,
        );
        Ok(())
    }
    fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[link(name = "kernel32")]
            extern "system" {
                fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
            }
            // Retry while joining: a worker can cross its stop check just
            // before issuing ReadFile/WriteFile. Never close a live worker's fd.
            for worker in &self.workers {
                unsafe {
                    CancelSynchronousIo(worker.as_raw_handle());
                }
            }
        }
    }
}
impl Drop for Pipes {
    fn drop(&mut self) {
        self.cancel();
        while self.workers.iter().any(|worker| !worker.is_finished()) {
            self.cancel();
            pause();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
pub(super) fn run(
    command: Command,
    capture: CaptureInput,
    cancelled: &AtomicBool,
) -> Result<Outcome, String> {
    let mut outcome = run_pipeline(vec![command], capture, cancelled)?;
    Ok(Outcome {
        status: outcome.statuses.pop().expect("one command"),
        stdout: outcome.stdout,
        stderr: outcome.stderrs.pop().unwrap_or_default(),
    })
}

pub(super) fn run_pipeline(
    commands: Vec<Command>,
    capture: CaptureInput,
    cancelled: &AtomicBool,
) -> Result<PipelineOutcome, String> {
    let count = commands.len();
    if count == 0 {
        return Err("process pipeline must contain at least one command".into());
    }
    let capturing = capture.is_some();
    let (mut input, limit) = match capture {
        Some((input, limit)) => (Some(input), limit),
        None => (None, 0),
    };
    let total = Arc::new(AtomicUsize::new(0));
    let mut children = Children(Vec::with_capacity(count));
    let mut pipes = Pipes::new();
    let mut previous: Option<std::process::ChildStdout> = None;
    // Queue the stdin worker last so captured stream indices follow stage
    // order: stderr[0..count], final stdout, then the empty writer result.
    let mut stdin = None;
    for (index, mut command) in commands.into_iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Err("process cancelled before spawn".into());
        }
        let last = index + 1 == count;
        command.stdin(match previous.take() {
            Some(pipe) => Stdio::from(pipe),
            None if capturing => Stdio::piped(),
            None => Stdio::inherit(),
        });
        command.stdout(if last && !capturing {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        command.stderr(if capturing {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        let child = command
            .spawn()
            .map_err(|error| format!("process stage {} spawn failed: {error}", index + 1))?;
        // Command owns the moved upstream read end too. Close the parent's
        // copy now so an early downstream exit delivers EOF/SIGPIPE correctly.
        drop(command);
        children.0.push(child);
        let child = children.0.last_mut().unwrap();
        if capturing {
            if index == 0 {
                stdin = child.stdin.take();
            }
            let stderr = child.stderr.take().expect("piped stderr");
            prepare(&stderr).map_err(|error| error.to_string())?;
            pipes
                .reader(stderr, total.clone(), limit)
                .map_err(|error| error.to_string())?;
        }
        if last {
            if capturing {
                let stdout = child.stdout.take().expect("piped stdout");
                prepare(&stdout).map_err(|error| error.to_string())?;
                pipes
                    .reader(stdout, total.clone(), limit)
                    .map_err(|error| error.to_string())?;
            }
        } else {
            // Keep intermediate pipes blocking: only the two child processes
            // own them after the next spawn. No user-space forwarding buffers.
            previous = child.stdout.take();
        }
    }
    if let Some(stdin) = stdin {
        prepare(&stdin).map_err(|error| error.to_string())?;
        pipes
            .writer(stdin, input.take().unwrap())
            .map_err(|error| error.to_string())?;
    }
    let mut statuses = vec![None; count];
    let mut failure = None;
    loop {
        if cancelled.load(Ordering::Acquire) || pipes.stop.load(Ordering::Acquire) {
            failure = Some(
                if cancelled.load(Ordering::Acquire) {
                    "process cancelled"
                } else {
                    "process pipe failed"
                }
                .to_owned(),
            );
            children.terminate();
            for (child, status) in children.0.iter_mut().zip(&mut statuses) {
                *status = Some(child.wait().map_err(|error| error.to_string())?);
            }
            pipes.cancel();
            break;
        }
        for (child, status) in children.0.iter_mut().zip(&mut statuses) {
            if status.is_none() {
                *status = child.try_wait().map_err(|error| error.to_string())?;
            }
        }
        if statuses.iter().all(Option::is_some) && pipes.workers.iter().all(JoinHandle::is_finished)
        {
            break;
        }
        pause();
    }
    while pipes.workers.iter().any(|worker| !worker.is_finished()) {
        if cancelled.load(Ordering::Acquire) || failure.is_some() {
            pipes.cancel();
        }
        pause();
    }
    let mut streams = Vec::new();
    for worker in pipes.workers.drain(..) {
        match worker.join() {
            Ok(Ok(bytes)) => streams.push(bytes),
            Ok(Err(error)) => {
                if failure.as_deref() != Some("process cancelled")
                    && error.to_string() != "process pipe cancelled"
                {
                    failure = Some(error.to_string());
                }
                streams.push(Vec::new());
            }
            Err(_) => {
                failure = Some("process pipe worker panicked".into());
                streams.push(Vec::new());
            }
        }
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let stdout = if capturing {
        streams.remove(count)
    } else {
        Vec::new()
    };
    streams.truncate(count);
    Ok(PipelineOutcome {
        statuses: statuses
            .into_iter()
            .map(|status| status.expect("reaped stage"))
            .collect(),
        stdout,
        stderrs: streams,
    })
}
