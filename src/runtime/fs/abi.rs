//! Version 1 nonblocking host file protocol.
//!
//! Registration is unsafe: callbacks and context must remain valid through all
//! retained references, never unwind or reenter Joky, and synchronize their own
//! workers. The same context may receive concurrent callbacks from arbitrary
//! threads, including shutdown and release callbacks. Context, operation, file
//! and result tokens must not depend on thread affinity; the host synchronizes
//! shared state and retains storage used by background work after stop/release.
//! Submit copies input views before return. Poll transfers each result
//! once. Stop revokes delivery without joining workers. Release callbacks only
//! release storage or schedule cleanup; they never perform blocking file I/O.
//! Hosts enforce bounded admission, including reserved per-file close capacity.
//! File-operation inputs may have invalid limits/sizes. Hosts check logical
//! Closed first, then validate read 1..=1 MiB, write <=1 MiB and seek origin <=2
//! before copying or executing. Open paths/options are validated by the runtime.
//! Background work retains context and wake tokens independently of operations.
use super::{
    failure,
    service::FileCell,
    worker::{Data, Input, Packet},
    FsError,
};
use crate::{runtime::executor::TaskWaker, Error};
use std::{
    ffi::c_void,
    ptr,
    sync::{Arc, Mutex, MutexGuard},
};
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileWakeV1 {
    pub context: *mut c_void,
    pub retain: unsafe extern "C" fn(*mut c_void),
    pub release: unsafe extern "C" fn(*mut c_void),
    pub wake: unsafe extern "C" fn(*mut c_void),
}
unsafe extern "C" fn retain_wake(p: *mut c_void) {
    Arc::increment_strong_count(p.cast::<TaskWaker>());
}
unsafe extern "C" fn release_wake(p: *mut c_void) {
    Arc::decrement_strong_count(p.cast::<TaskWaker>());
}
unsafe extern "C" fn wake(p: *mut c_void) {
    (*p.cast::<TaskWaker>()).wake();
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileInputV1 {
    pub operation_code: u32,
    pub file_token: *mut c_void,
    pub path: *const u8,
    pub path_len: u64,
    pub flags: u64,
    pub bytes: *const u8,
    pub bytes_len: u64,
    pub limit: u64,
    pub origin: u32,
    pub offset_bits: u64,
}
impl Default for FileInputV1 {
    fn default() -> Self {
        Self {
            operation_code: 0,
            file_token: ptr::null_mut(),
            path: ptr::null(),
            path_len: 0,
            flags: 0,
            bytes: ptr::null(),
            bytes_len: 0,
            limit: 0,
            origin: 0,
            offset_bits: 0,
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileResultV1 {
    pub result_token: *mut c_void,
    pub error_code: u32,
    pub message: *const u8,
    pub message_len: u64,
    pub file_token: *mut c_void,
    pub bytes: *const u8,
    pub bytes_len: u64,
    pub count: u64,
}
impl Default for FileResultV1 {
    fn default() -> Self {
        Self {
            result_token: ptr::null_mut(),
            error_code: 0,
            message: ptr::null(),
            message_len: 0,
            file_token: ptr::null_mut(),
            bytes: ptr::null(),
            bytes_len: 0,
            count: 0,
        }
    }
}
/// Every callback is required. Version, size and reserved fields are checked
/// before retaining context; pointer validity remains the registrant's obligation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileServiceV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    pub reserved: u64,
    pub context: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*mut c_void)>,
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
    pub submit: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const FileInputV1,
            FileWakeV1,
            *mut *mut c_void,
            *mut FileResultV1,
        ) -> u32,
    >,
    pub poll: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut FileResultV1) -> u32>,
    pub stop: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32)>,
    pub release_result: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub release_file: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub shutdown: Option<unsafe extern "C" fn(*mut c_void)>,
    pub is_quiescent: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
pub(crate) struct External(FileServiceV1);
// SAFETY: from_descriptor requires thread-safe lifetime and callback synchronization.
unsafe impl Send for External {}
unsafe impl Sync for External {}
impl Drop for External {
    fn drop(&mut self) {
        unsafe {
            (self.0.shutdown.unwrap())(self.0.context);
            (self.0.release.unwrap())(self.0.context);
        }
    }
}
impl External {
    pub(super) unsafe fn new(d: FileServiceV1) -> Result<Arc<Self>, Error> {
        if d.abi_version != 1
            || d.struct_size as usize != std::mem::size_of::<FileServiceV1>()
            || d.reserved != 0
            || d.context.is_null()
            || d.retain.is_none()
            || d.release.is_none()
            || d.submit.is_none()
            || d.poll.is_none()
            || d.stop.is_none()
            || d.release_result.is_none()
            || d.release_file.is_none()
            || d.shutdown.is_none()
            || d.is_quiescent.is_none()
        {
            return Err(failure("Invalid FileServiceV1 descriptor"));
        }
        (d.retain.unwrap())(d.context);
        Ok(Arc::new(Self(d)))
    }
    pub(super) fn shutdown(&self) {
        unsafe { (self.0.shutdown.unwrap())(self.0.context) }
    }
    pub(super) fn is_quiescent(&self) -> bool {
        unsafe { (self.0.is_quiescent.unwrap())(self.0.context) == 1 }
    }
    pub(super) fn submit(
        self: &Arc<Self>,
        input: Input<'_>,
        file: Option<Arc<ExternalFile>>,
        wake: TaskWaker,
    ) -> Result<ExternalTicket, FsError> {
        let mut raw = FileInputV1::default();
        if let Some(f) = &file {
            raw.file_token = lock(&f.state).token;
        }
        match input {
            Input::Rename(..)
            | Input::RemoveFile(_)
            | Input::ReadDir(_)
            | Input::CreateDir(_)
            | Input::CreateDirAll(_)
            | Input::WriteAtomic(..)
            | Input::RemoveDir(_)
            | Input::Metadata(_) => {
                return Err(FsError::new(
                    1,
                    "File service V1 does not provide path operations",
                ))
            }
            Input::Stdin => {
                return Err(FsError::new(
                    1,
                    "File service V1 does not provide standard input",
                ))
            }
            Input::Open(path, flags) => {
                raw.operation_code = 1;
                raw.path = path.as_ptr();
                raw.path_len = path.len() as u64;
                raw.flags = flags;
            }
            Input::Read(n) => {
                raw.operation_code = 2;
                raw.limit = n;
            }
            Input::Write(bytes) => {
                raw.operation_code = 3;
                raw.bytes = bytes.as_ptr();
                raw.bytes_len = bytes.len() as u64;
            }
            Input::Seek(o, n) => {
                raw.operation_code = 4;
                raw.origin = o;
                raw.offset_bits = n;
            }
            Input::Sync => raw.operation_code = 5,
        }
        self.start(raw, wake)
    }
    fn start(
        self: &Arc<Self>,
        input: FileInputV1,
        waker: TaskWaker,
    ) -> Result<ExternalTicket, FsError> {
        let waker = Arc::new(waker);
        let wake = FileWakeV1 {
            context: Arc::as_ptr(&waker).cast_mut().cast(),
            retain: retain_wake,
            release: release_wake,
            wake,
        };
        let mut operation = ptr::null_mut();
        let mut result = FileResultV1::default();
        let status = unsafe {
            (self.0.submit.unwrap())(self.0.context, &input, wake, &mut operation, &mut result)
        };
        let empty = empty(&result);
        let mut guard = ResultGuard {
            service: self.clone(),
            raw: result,
        };
        if status == 0 && !operation.is_null() && empty {
            return Ok(ExternalTicket {
                service: self.clone(),
                state: Mutex::new(OperationState {
                    token: operation,
                    reason: 2,
                }),
                kind: input.operation_code,
                limit: if input.operation_code == 2 {
                    input.limit
                } else {
                    input.bytes_len
                },
                _wake: waker,
            });
        }
        if !operation.is_null() {
            unsafe {
                (self.0.stop.unwrap())(self.0.context, operation, 1);
            }
        }
        if status == 1 && operation.is_null() && result.error_code != 0 {
            return Err(guard
                .packet(input.operation_code, 0)
                .error
                .take()
                .unwrap_or_else(protocol));
        }
        Err(protocol())
    }
}
fn protocol() -> FsError {
    FsError::new(10, "Invalid file service protocol response")
}
fn empty(r: &FileResultV1) -> bool {
    r.result_token.is_null()
        && r.error_code == 0
        && r.message.is_null()
        && r.message_len == 0
        && r.file_token.is_null()
        && r.bytes.is_null()
        && r.bytes_len == 0
        && r.count == 0
}
struct ResultGuard {
    service: Arc<External>,
    raw: FileResultV1,
}
// SAFETY: Result storage and release callbacks obey the descriptor's thread contract.
unsafe impl Send for ResultGuard {}
impl Drop for ResultGuard {
    fn drop(&mut self) {
        unsafe {
            let d = &self.service.0;
            if !self.raw.file_token.is_null() {
                (d.release_file.unwrap())(d.context, self.raw.file_token);
            }
            if !self.raw.result_token.is_null() {
                (d.release_result.unwrap())(d.context, self.raw.result_token);
            }
        }
    }
}
impl ResultGuard {
    fn packet(&mut self, kind: u32, limit: u64) -> Packet {
        let r = &mut self.raw;
        let valid = r.message_len <= 1024
            && r.bytes_len <= 1048576
            && r.message_len <= usize::MAX as u64
            && r.bytes_len <= usize::MAX as u64
            && (r.message_len == 0 || !r.message.is_null())
            && (r.bytes_len == 0 || !r.bytes.is_null())
            && ((r.message_len == 0 && r.bytes_len == 0) || !r.result_token.is_null());
        if !valid {
            return Packet::error(protocol());
        }
        if r.error_code != 0 {
            if r.error_code > 10
                || !r.file_token.is_null()
                || !r.bytes.is_null()
                || r.bytes_len != 0
                || r.count != 0
            {
                return Packet::error(protocol());
            }
            let bytes = if r.message_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(r.message, r.message_len as usize) }
            };
            return Packet::error(match std::str::from_utf8(bytes) {
                Ok(text) => FsError::new(r.error_code, text),
                Err(_) => protocol(),
            });
        }
        if !r.message.is_null() || r.message_len != 0 {
            return Packet::error(protocol());
        }
        let has_bytes = !r.bytes.is_null() || r.bytes_len != 0;
        let data = match kind {
            1 if !r.file_token.is_null() && !has_bytes && r.count == 0 => {
                let file = ExternalFile {
                    service: self.service.clone(),
                    state: Mutex::new(FileState {
                        token: std::mem::replace(&mut r.file_token, ptr::null_mut()),
                        closing: false,
                        pending: None,
                        result: None,
                    }),
                };
                Data::File(Arc::new(FileCell::External(Arc::new(file))))
            }
            2 if r.file_token.is_null() && r.count == 0 && r.bytes_len <= limit => {
                Data::Bytes(if r.bytes_len == 0 {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(r.bytes, r.bytes_len as usize) }.to_vec()
                })
            }
            3 | 4 if r.file_token.is_null() && !has_bytes && (kind == 4 || r.count <= limit) => {
                if kind == 3 && r.count == 0 && limit > 0 {
                    return Packet::error(FsError::new(9, "Write made no progress"));
                }
                Data::Count(r.count)
            }
            5 | 6 if r.file_token.is_null() && !has_bytes && r.count == 0 => Data::Unit,
            _ => return Packet::error(protocol()),
        };
        Packet::ok(data)
    }
}
struct OperationState {
    token: *mut c_void,
    reason: u32,
}
pub(crate) struct ExternalTicket {
    service: Arc<External>,
    state: Mutex<OperationState>,
    kind: u32,
    limit: u64,
    _wake: Arc<TaskWaker>,
}
// SAFETY: tokens are opaque and only passed to synchronized host callbacks.
unsafe impl Send for ExternalTicket {}
unsafe impl Sync for ExternalTicket {}
impl ExternalTicket {
    pub(super) fn take(&self) -> Option<Packet> {
        let mut s = lock(&self.state);
        if s.token.is_null() || s.reason != 2 {
            return None;
        }
        let mut raw = FileResultV1::default();
        let status =
            unsafe { (self.service.0.poll.unwrap())(self.service.0.context, s.token, &mut raw) };
        let mut guard = ResultGuard {
            service: self.service.clone(),
            raw,
        };
        if status == 1 && empty(&raw) {
            return None;
        }
        s.reason = if status == 0 { 0 } else { 1 };
        let packet = if status == 0 {
            guard.packet(self.kind, self.limit)
        } else {
            Packet::error(protocol())
        };
        // Input/result copies are already covered by the task request's peak
        // reservation. Release host storage before publishing the runtime packet.
        drop(guard);
        Some(packet)
    }
    pub(super) fn stop(&self, reason: u32) {
        let mut s = lock(&self.state);
        if !s.token.is_null() {
            unsafe {
                (self.service.0.stop.unwrap())(
                    self.service.0.context,
                    s.token,
                    if reason == 1 { 1 } else { s.reason },
                );
            }
            s.token = ptr::null_mut();
        }
    }
}
impl Drop for ExternalTicket {
    fn drop(&mut self) {
        self.stop(2);
    }
}
struct FileState {
    token: *mut c_void,
    closing: bool,
    pending: Option<ExternalTicket>,
    result: Option<Result<(), FsError>>,
}
pub(crate) struct ExternalFile {
    service: Arc<External>,
    state: Mutex<FileState>,
}
// SAFETY: no dereference of the host token; callbacks synchronize file ownership.
unsafe impl Send for ExternalFile {}
unsafe impl Sync for ExternalFile {}
impl ExternalFile {
    pub(super) fn is_closed(&self) -> bool {
        lock(&self.state).closing
    }
    pub(super) fn close(&self, wake: Option<TaskWaker>) {
        let mut s = lock(&self.state);
        s.closing = true;
        if let Some(wake) = wake {
            if s.pending.is_none() && s.result.is_none() {
                match self.service.start(
                    FileInputV1 {
                        operation_code: 6,
                        file_token: s.token,
                        ..Default::default()
                    },
                    wake,
                ) {
                    Ok(t) => s.pending = Some(t),
                    Err(e) => s.result = Some(Err(e)),
                }
            }
        } else {
            s.pending.take();
            self.release(&mut s);
        }
    }
    fn release(&self, s: &mut FileState) {
        if !s.token.is_null() {
            unsafe {
                (self.service.0.release_file.unwrap())(self.service.0.context, s.token);
            }
            s.token = ptr::null_mut();
        }
    }
    pub(super) fn close_result(&self) -> Option<Result<(), FsError>> {
        let mut s = lock(&self.state);
        if let Some(p) = s.pending.as_ref().and_then(ExternalTicket::take) {
            s.result = Some(p.error.clone().map_or(Ok(()), Err));
            s.pending.take();
        }
        s.result.clone()
    }
    pub(super) fn stop_close(&self, reason: u32) {
        let mut s = lock(&self.state);
        if let Some(pending) = s.pending.take() {
            pending.stop(reason);
        }
        self.release(&mut s);
    }
}
impl Drop for ExternalFile {
    fn drop(&mut self) {
        self.close(None);
    }
}
