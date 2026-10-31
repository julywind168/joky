//! Bounded blocking service. Language storage never enters a worker.
use super::service::FileCell;
use super::{failure, FsError};
use crate::runtime::executor::TaskWaker;
use crate::Error;
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard, Weak,
    },
    thread,
};

pub trait FileHandle: Send {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, bytes: &[u8]) -> Result<usize, FsError>;
    fn seek(&mut self, origin: u32, offset: u64) -> Result<u64, FsError>;
    fn sync_all(&mut self) -> Result<(), FsError>;
    /// Runs on a service worker, including cancellation and automatic cleanup.
    fn close(self: Box<Self>) -> Result<(), FsError>;
}
/// Blocking implementations run exclusively on service workers. Implementations
/// must not access language storage or retain byte views. Panics are converted to
/// SystemFailure and terminate the affected file. Engine thread rules still apply.
pub trait FileSystem: Send + Sync + 'static {
    fn open(&self, path: &str, flags: u64) -> Result<Box<dyn FileHandle>, FsError>;
    /// Optional exclusive standard input capability. The default grants no access.
    fn stdin(&self) -> Result<Box<dyn FileHandle>, FsError> {
        Err(FsError::new(1, "Host does not provide standard input"))
    }
}
struct Native(PathBuf, bool);
impl FileSystem for Native {
    fn open(&self, path: &str, flags: u64) -> Result<Box<dyn FileHandle>, FsError> {
        let file = std::fs::OpenOptions::new()
            .read(flags & 1 != 0)
            .write(flags & 2 != 0)
            .append(flags & 4 != 0)
            .create(flags & 8 != 0)
            .create_new(flags & 16 != 0)
            .truncate(flags & 32 != 0)
            .open(self.0.join(path))
            .map_err(FsError::from)?;
        Ok(Box::new(file))
    }
    fn stdin(&self) -> Result<Box<dyn FileHandle>, FsError> {
        if !self.1 {
            return Err(FsError::new(1, "Host does not provide standard input"));
        }
        if STDIN_OWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(FsError::new(6, "Standard input already has a reader"));
        }
        Ok(Box::new(StandardInput))
    }
}
static STDIN_OWNED: AtomicBool = AtomicBool::new(false);
struct StandardInput;
impl Drop for StandardInput {
    fn drop(&mut self) {
        STDIN_OWNED.store(false, Ordering::Release);
    }
}
impl FileHandle for StandardInput {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        std::io::stdin().read(bytes).map_err(Into::into)
    }
    fn write(&mut self, _: &[u8]) -> Result<usize, FsError> {
        Err(FsError::new(1, "Standard input is read-only"))
    }
    fn seek(&mut self, _: u32, _: u64) -> Result<u64, FsError> {
        Err(FsError::new(1, "Standard input cannot seek"))
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        Ok(())
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        Ok(())
    }
}
impl FileHandle for std::fs::File {
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, FsError> {
        Read::read(self, bytes).map_err(Into::into)
    }
    fn write(&mut self, bytes: &[u8]) -> Result<usize, FsError> {
        Write::write(self, bytes).map_err(Into::into)
    }
    fn seek(&mut self, origin: u32, offset: u64) -> Result<u64, FsError> {
        let from = match origin {
            0 => SeekFrom::Start(offset),
            1 => SeekFrom::Current(offset as i64),
            2 => SeekFrom::End(offset as i64),
            _ => return Err(FsError::new(5, "Invalid seek origin")),
        };
        Seek::seek(self, from).map_err(Into::into)
    }
    fn sync_all(&mut self) -> Result<(), FsError> {
        std::fs::File::sync_all(self).map_err(Into::into)
    }
    fn close(self: Box<Self>) -> Result<(), FsError> {
        #[cfg(unix)]
        {
            use std::os::fd::IntoRawFd;
            unsafe extern "C" {
                #[link_name = "close"]
                fn close_fd(fd: std::ffi::c_int) -> std::ffi::c_int;
            }
            let fd = (*self).into_raw_fd();
            // Never retry close: even EINTR may have consumed the descriptor.
            if unsafe { close_fd(fd) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error().into())
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::IntoRawHandle;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            }
            if unsafe { CloseHandle((*self).into_raw_handle()) } != 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error().into())
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            drop(self);
            Ok(())
        }
    }
}
#[derive(Clone, Debug)]
pub struct FileLimits {
    pub workers: usize,
    pub queued: usize,
    pub requests: usize,
    pub files: usize,
    pub bytes: usize,
}
impl Default for FileLimits {
    fn default() -> Self {
        Self {
            workers: 2,
            queued: 64,
            requests: 128,
            files: 256,
            bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Default)]
struct Ledger {
    files: AtomicUsize,
    requests: AtomicUsize,
    bytes: AtomicUsize,
    workers: AtomicUsize,
}
struct Permit {
    ledger: Arc<Ledger>,
    bytes: usize,
    file: bool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.ledger.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        if self.file {
            self.ledger.files.fetch_sub(1, Ordering::AcqRel);
        } else {
            self.ledger.requests.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
fn reserve(counter: &AtomicUsize, count: usize, limit: usize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(count).filter(|n| *n <= limit)
        })
        .is_ok()
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
struct Queue {
    jobs: VecDeque<Arc<NativeTicket>>,
    cleanup: VecDeque<Arc<NativeFile>>,
    files: Vec<Weak<NativeFile>>,
    shutdown: bool,
    close_turn: bool,
}
struct Core {
    queue: Mutex<Queue>,
    signal: Condvar,
    backend: Arc<dyn FileSystem>,
    limits: FileLimits,
    ledger: Arc<Ledger>,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
}
struct Handle(Arc<Core>);
impl Drop for Handle {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
/// Explicitly shared service. Dropping it never joins a blocked worker.
#[derive(Clone)]
pub struct NativeService(Arc<Handle>);
impl NativeService {
    pub fn new(backend: Arc<dyn FileSystem>, limits: FileLimits) -> Result<Self, Error> {
        if limits.workers == 0
            || limits.queued == 0
            || limits.requests < limits.queued
            || limits.files == 0
            || limits.bytes < 4096
            || limits.files > 1_000_000
            || limits.queued > 1_000_000
            || limits.workers > 256
        {
            return Err(failure("Invalid file service limits"));
        }
        let workers = Mutex::new(Vec::with_capacity(limits.workers));
        let core = Arc::new(Core {
            queue: Mutex::new(Queue {
                jobs: VecDeque::with_capacity(limits.queued),
                cleanup: VecDeque::with_capacity(limits.files),
                files: Vec::with_capacity(limits.files),
                shutdown: false,
                close_turn: true,
            }),
            signal: Condvar::new(),
            backend,
            limits,
            ledger: Arc::default(),
            workers,
        });
        for index in 0..core.limits.workers {
            let worker = core.clone();
            core.ledger.workers.fetch_add(1, Ordering::AcqRel);
            match thread::Builder::new()
                .name(format!("joky-file-{index}"))
                .spawn(move || work(worker))
            {
                Ok(handle) => lock(&core.workers).push(handle),
                Err(error) => {
                    core.ledger.workers.fetch_sub(1, Ordering::AcqRel);
                    core.shutdown();
                    return Err(failure(format!("Cannot start file worker: {error}")));
                }
            }
        }
        Ok(Self(Arc::new(Handle(core))))
    }
    pub fn native(base: impl AsRef<Path>, limits: FileLimits) -> Result<Self, Error> {
        Self::native_with_stdio(base, limits, false)
    }
    pub(crate) fn native_with_stdio(
        base: impl AsRef<Path>,
        limits: FileLimits,
        stdio: bool,
    ) -> Result<Self, Error> {
        let base = base
            .as_ref()
            .canonicalize()
            .map_err(|e| failure(format!("Invalid file base: {e}")))?;
        Self::new(Arc::new(Native(base, stdio)), limits)
    }
    pub fn shutdown(&self) {
        self.0 .0.shutdown();
    }
    pub fn is_quiescent(&self) -> bool {
        // Only join handles already known to have exited, including thread-local
        // destructors. A counter set before return cannot authorize code unload.
        let mut workers = lock(&self.0 .0.workers);
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                let _ = workers.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        workers.is_empty()
            && self.0 .0.ledger.requests.load(Ordering::Acquire) == 0
            && self.0 .0.ledger.files.load(Ordering::Acquire) == 0
    }
    pub fn usage(&self) -> (usize, usize, usize) {
        let l = &self.0 .0.ledger;
        (
            l.files.load(Ordering::Acquire),
            l.requests.load(Ordering::Acquire),
            l.bytes.load(Ordering::Acquire),
        )
    }
    pub(crate) fn submit(
        &self,
        input: Input<'_>,
        file: Option<Arc<NativeFile>>,
        waker: TaskWaker,
    ) -> Result<Arc<NativeTicket>, FsError> {
        let core = &self.0 .0;
        let size = 4096 + input.size();
        let mut queue = lock(&core.queue);
        if queue.shutdown {
            return Err(FsError::new(7, "File service is closed"));
        }
        if queue.jobs.len() >= core.limits.queued
            || !reserve(&core.ledger.requests, 1, core.limits.requests)
        {
            return Err(FsError::new(6, "File request limit exceeded"));
        }
        if !reserve(&core.ledger.bytes, size, core.limits.bytes) {
            core.ledger.requests.fetch_sub(1, Ordering::AcqRel);
            return Err(FsError::new(6, "File storage limit exceeded"));
        }
        let permit = Permit {
            ledger: core.ledger.clone(),
            bytes: size,
            file: false,
        };
        let file = if matches!(input, Input::Open(..) | Input::Stdin) {
            if !reserve(&core.ledger.files, 1, core.limits.files) {
                return Err(FsError::new(6, "Open file limit exceeded"));
            }
            let file = Arc::new(NativeFile {
                core: core.clone(),
                state: Mutex::new(FileState {
                    handle: None,
                    active: true,
                    executing: false,
                    closing: false,
                    queued: false,
                    closed: false,
                    error: None,
                    waiter: None,
                }),
                _permit: Permit {
                    ledger: core.ledger.clone(),
                    bytes: 0,
                    file: true,
                },
            });
            queue.files.retain(|f| f.strong_count() > 0);
            queue.files.push(Arc::downgrade(&file));
            file
        } else {
            let file = file.expect("file operation receiver");
            let mut state = lock(&file.state);
            if state.closing {
                return Err(FsError::new(7, "File is closed"));
            }
            if state.active {
                return Err(FsError::new(6, "File has an active operation"));
            }
            state.active = true;
            drop(state);
            file
        };
        let action = input.snapshot();
        let ticket = Arc::new(NativeTicket {
            state: Mutex::new(NativeTicketState {
                action: Some(action),
                running: false,
                cancelled: false,
                result: None,
                waker: Some(waker),
                delivered: false,
            }),
            file,
            _permit: Arc::new(permit),
        });
        queue.jobs.push_back(ticket.clone());
        core.signal.notify_one();
        Ok(ticket)
    }
}
struct FileState {
    handle: Option<Box<dyn FileHandle>>,
    // Admission remains exclusive until the result is claimed or cancelled.
    active: bool,
    // Physical close only waits for actual I/O, not for result consumption.
    executing: bool,
    closing: bool,
    queued: bool,
    closed: bool,
    error: Option<FsError>,
    waiter: Option<TaskWaker>,
}
pub(crate) struct NativeFile {
    core: Arc<Core>,
    state: Mutex<FileState>,
    _permit: Permit,
}
impl NativeFile {
    pub(crate) fn close(self: &Arc<Self>, waiter: Option<TaskWaker>) {
        {
            let mut state = lock(&self.state);
            state.closing = true;
            if waiter.is_some() {
                state.waiter = waiter;
            }
        }
        self.enqueue_close();
    }
    fn enqueue_close(self: &Arc<Self>) {
        let mut queue = lock(&self.core.queue);
        let mut state = lock(&self.state);
        if !state.executing && !state.closed && !state.queued {
            state.queued = true;
            queue.cleanup.push_back(self.clone());
            self.core.signal.notify_one();
        }
    }
    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.state).closing
    }
    pub(crate) fn close_result(&self) -> Option<Result<(), FsError>> {
        let s = lock(&self.state);
        s.closed.then(|| s.error.clone().map_or(Ok(()), Err))
    }
    pub(crate) fn stop_close(&self) {
        lock(&self.state).waiter = None;
    }
}
pub(crate) enum Input<'a> {
    Stdin,
    Open(&'a str, u64),
    Read(u64),
    Write(&'a [u8]),
    Seek(u32, u64),
    Sync,
}
impl Input<'_> {
    pub(crate) fn size(&self) -> usize {
        match self {
            Self::Open(p, _) => p.len(),
            Self::Read(n) => (*n).min(1048576) as usize,
            Self::Write(b) => b.len().min(1048576),
            _ => 0,
        }
    }
    fn snapshot(self) -> Action {
        match self {
            Self::Open(p, f) => Action::Open(p.to_owned(), f),
            Self::Stdin => Action::Stdin,
            Self::Read(n) => Action::Read(n as usize),
            Self::Write(b) => Action::Write(b.to_vec()),
            Self::Seek(o, n) => Action::Seek(o, n),
            Self::Sync => Action::Sync,
        }
    }
}
pub(crate) enum Action {
    Stdin,
    Open(String, u64),
    Read(usize),
    Write(Vec<u8>),
    Seek(u32, u64),
    Sync,
}
pub(crate) enum Data {
    File(Arc<FileCell>),
    Bytes(Vec<u8>),
    Count(u64),
    Unit,
}
pub(crate) struct Packet {
    pub data: Option<Data>,
    pub error: Option<FsError>,
    _permit: Option<Arc<Permit>>,
}
impl Packet {
    pub(crate) fn error(error: FsError) -> Self {
        Self {
            data: None,
            error: Some(error),
            _permit: None,
        }
    }
    pub(crate) fn ok(data: Data) -> Self {
        Self {
            data: Some(data),
            error: None,
            _permit: None,
        }
    }
}
impl Drop for Packet {
    fn drop(&mut self) {
        if let Some(Data::File(file)) = &self.data {
            file.close(None);
        }
    }
}
struct NativeTicketState {
    action: Option<Action>,
    running: bool,
    cancelled: bool,
    result: Option<Packet>,
    waker: Option<TaskWaker>,
    delivered: bool,
}
pub(crate) struct NativeTicket {
    state: Mutex<NativeTicketState>,
    file: Arc<NativeFile>,
    _permit: Arc<Permit>,
}
impl NativeTicket {
    pub(crate) fn fail(self: &Arc<Self>) {
        self.stop();
        self.file.close(None);
    }
    pub(crate) fn take(&self) -> Option<Packet> {
        let mut state = lock(&self.state);
        let packet = state.result.take()?;
        state.delivered = true;
        Some(packet)
    }
    pub(crate) fn stop(self: &Arc<Self>) {
        // Queue -> ticket -> file is the single admission/cancellation lock order.
        let mut queue = lock(&self.file.core.queue);
        let mut state = lock(&self.state);
        state.waker = None;
        if state.delivered {
            lock(&self.file.state).active = false;
            drop(state);
            drop(queue);
            if self.file.is_closed() {
                self.file.enqueue_close();
            }
            return;
        }
        state.cancelled = true;
        let queued = state.action.is_some();
        if queued {
            queue.jobs.retain(|t| !Arc::ptr_eq(t, self));
        }
        let open = matches!(state.action, Some(Action::Open(..) | Action::Stdin));
        let packet = state.result.take();
        {
            let mut f = lock(&self.file.state);
            if !queued || open {
                f.closing = true;
            }
            if queued || !state.running {
                f.active = false;
            }
        }
        drop(state);
        drop(queue);
        drop(packet);
        if self.file.is_closed() {
            self.file.enqueue_close();
        }
    }
}
impl Core {
    fn shutdown(&self) {
        let (jobs, files) = {
            let mut q = lock(&self.queue);
            q.shutdown = true;
            (
                q.jobs.drain(..).collect::<Vec<_>>(),
                q.files.iter().filter_map(Weak::upgrade).collect::<Vec<_>>(),
            )
        };
        for ticket in jobs {
            let mut s = lock(&ticket.state);
            s.action.take();
            s.result = Some(Packet::error(FsError::new(7, "File service is closed")));
            let w = s.waker.clone();
            drop(s);
            lock(&ticket.file.state).active = false;
            if let Some(w) = w {
                w.wake();
            }
        }
        for file in files {
            file.close(None);
        }
        self.signal.notify_all();
    }
}
fn execute(backend: &dyn FileSystem, file: &Arc<NativeFile>, action: Action) -> Packet {
    if matches!(action, Action::Open(..) | Action::Stdin) {
        let opened = match action {
            Action::Open(path, flags) => backend.open(&path, flags),
            Action::Stdin => backend.stdin(),
            _ => unreachable!(),
        };
        return match opened {
            Ok(handle) => {
                lock(&file.state).handle = Some(handle);
                Packet::ok(Data::File(Arc::new(FileCell::Native(file.clone()))))
            }
            Err(error) => {
                file.close(None);
                Packet::error(error)
            }
        };
    }
    let mut handle = lock(&file.state).handle.take().expect("active file handle");
    let result = match action {
        Action::Read(limit) => {
            let mut bytes = vec![0; limit];
            handle.read(&mut bytes).and_then(|n| {
                if n > limit {
                    return Err(FsError::new(10, "Invalid read count"));
                }
                bytes.truncate(n);
                Ok(Data::Bytes(bytes))
            })
        }
        Action::Write(bytes) => handle.write(&bytes).and_then(|n| {
            if n > bytes.len() {
                Err(FsError::new(10, "Invalid write count"))
            } else if n == 0 && !bytes.is_empty() {
                Err(FsError::new(9, "Write made no progress"))
            } else {
                Ok(Data::Count(n as u64))
            }
        }),
        Action::Seek(origin, offset) => handle.seek(origin, offset).map(Data::Count),
        Action::Sync => handle.sync_all().map(|_| Data::Unit),
        Action::Open(..) | Action::Stdin => unreachable!(),
    };
    lock(&file.state).handle = Some(handle);
    match result {
        Ok(data) => Packet::ok(data),
        Err(error) => Packet::error(error),
    }
}
fn work(core: Arc<Core>) {
    loop {
        let mut q = lock(&core.queue);
        if !q.cleanup.is_empty() && (q.close_turn || q.jobs.is_empty()) {
            let file = q.cleanup.pop_front().unwrap();
            q.close_turn = false;
            drop(q);
            let handle = lock(&file.state).handle.take();
            let error = handle.and_then(|h| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.close()))
                    .unwrap_or_else(|_| Err(FsError::new(10, "File close panicked")))
                    .err()
            });
            let waker = {
                let mut s = lock(&file.state);
                s.closed = true;
                s.queued = false;
                s.error = error;
                s.waiter.take()
            };
            if let Some(w) = waker {
                w.wake();
            }
            continue;
        }
        if let Some(ticket) = q.jobs.pop_front() {
            q.close_turn = true;
            let action = {
                let mut s = lock(&ticket.state);
                s.running = true;
                s.action.take().unwrap()
            };
            lock(&ticket.file.state).executing = true;
            drop(q);
            let mut packet = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(&*core.backend, &ticket.file, action)
            }))
            .unwrap_or_else(|_| {
                ticket.file.close(None);
                Packet::error(FsError::new(10, "File operation panicked"))
            });
            packet._permit = Some(ticket._permit.clone());
            let mut s = lock(&ticket.state);
            s.running = false;
            let cancelled = s.cancelled;
            let waker = s.waker.clone();
            if !cancelled {
                s.result = Some(packet);
            } else {
                drop(s);
                drop(packet);
                s = lock(&ticket.state);
            }
            drop(s);
            lock(&ticket.file.state).executing = false;
            if cancelled || ticket.file.is_closed() {
                lock(&ticket.file.state).active = false;
                ticket.file.close(None);
            }
            if let Some(w) = waker {
                w.wake();
            }
            continue;
        }
        if q.shutdown && q.cleanup.is_empty() && core.ledger.files.load(Ordering::Acquire) == 0 {
            core.ledger.workers.fetch_sub(1, Ordering::AcqRel);
            core.signal.notify_all();
            return;
        }
        // Closed wrappers may outlive their physical file. Exit after every
        // registered file is physically closed; usage still tracks wrappers.
        if q.shutdown
            && q.files
                .iter()
                .all(|f| f.upgrade().is_none_or(|f| lock(&f.state).closed))
        {
            core.ledger.workers.fetch_sub(1, Ordering::AcqRel);
            core.signal.notify_all();
            return;
        }
        drop(core.signal.wait(q).unwrap_or_else(|e| e.into_inner()));
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
