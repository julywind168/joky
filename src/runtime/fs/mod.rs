//! Optional file capability and compiler-checked asynchronous resource operations.
pub mod abi;
mod service;
mod testing;
mod worker;
use super::{
    arena::{Arena, ExternalCharge},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    executor::current_waker,
    resource::{OwnedResource, ResourceLease},
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
use service::{FileCell, Ticket};
pub use service::{FileHandle, FileLimits, FileService, FileSystem};
use std::{cell::RefCell, ffi::c_void, ptr::NonNull, sync::Arc};
use worker::{Data, Input, Packet};
pub use worker::{DirectoryNames, FileKind, Metadata};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsError {
    code: u32,
    message: String,
}
impl FsError {
    pub fn code(&self) -> u32 {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        let mut message = message.into();
        let mut n = message.len().min(1024);
        while !message.is_char_boundary(n) {
            n -= 1;
        }
        message.truncate(n);
        message.shrink_to_fit();
        Self {
            code: if (1..=10).contains(&code) { code } else { 10 },
            message,
        }
    }
}
impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        Self::new(
            match e.kind() {
                PermissionDenied => 2,
                NotFound => 3,
                AlreadyExists => 4,
                InvalidInput => 5,
                Unsupported => 1,
                WriteZero => 9,
                _ => 10,
            },
            e.to_string(),
        )
    }
}
fn failure(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::HostError, message)
}
thread_local! {static CURRENT:RefCell<Option<FileService>>=const{RefCell::new(None)};}
pub(crate) struct Scope(Option<FileService>);
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|s| s.replace(self.0.take()));
    }
}
pub(crate) fn enter(service: Option<FileService>) -> Scope {
    Scope(CURRENT.with(|s| s.replace(service)))
}

pub(crate) fn write_output(
    stderr: bool,
    text: &str,
    emit: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let service = CURRENT.with(|s| s.borrow().clone());
    match service {
        Some(service) => service.write_output(stderr, text, emit),
        None => emit(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Operation {
    Open = 1,
    Read,
    Write,
    Seek,
    Sync,
    Close,
    Code,
    Message,
    TakeFile,
    TakeBytes,
    Count,
    Process,
    Stdin,
    TestInput,
    Rename,
    RemoveFile,
    ReadDir,
    CreateDir,
    RemoveDir,
    Metadata,
    Kind,
}
pub(crate) fn operation(name: &str) -> Option<Operation> {
    Some(match name {
        "__fs_open" => Operation::Open,
        "__fs_rename" => Operation::Rename,
        "__fs_remove_file" => Operation::RemoveFile,
        "__fs_create_dir" => Operation::CreateDir,
        "__fs_remove_dir" => Operation::RemoveDir,
        "__fs_metadata" => Operation::Metadata,
        "__fs_kind" => Operation::Kind,
        "__fs_read_dir" => Operation::ReadDir,
        "__fs_read" => Operation::Read,
        "__fs_write" => Operation::Write,
        "__fs_seek" => Operation::Seek,
        "__fs_sync" => Operation::Sync,
        "__fs_close" => Operation::Close,
        "__fs_code" => Operation::Code,
        "__fs_message" => Operation::Message,
        "__fs_file" => Operation::TakeFile,
        "__fs_bytes" => Operation::TakeBytes,
        "__fs_count" => Operation::Count,
        "__process_read" => Operation::Process,
        "__io_stdin" => Operation::Stdin,
        "__test_input" => Operation::TestInput,
        _ => return None,
    })
}
impl Operation {
    pub(crate) fn returns_bytes(self) -> bool {
        self == Self::TakeBytes
    }
    pub(crate) fn scalar(self) -> bool {
        matches!(self, Self::Code | Self::Kind | Self::Count)
    }
    pub(crate) fn validate(self, sig: &FunctionSignature, suspends: bool) -> Result<(), Error> {
        let owner = Type::Resource("fs.FileOwner".into());
        let outcome = Type::Resource("fs.OutcomeOwner".into());
        let file = Type::reference(owner.clone(), true);
        let result = Type::reference(outcome.clone(), true);
        let bytes = |t: &Type| matches!(t,Type::Record(r)if r.name=="joky/array.Array"&&r.arguments==[Type::UInt8]&&r.fields.len()==1&&matches!(&r.fields[0].ty,Type::Buffer(t)if **t==Type::UInt8));
        let (params, ret) = match self {
            Self::Open => (vec![Type::String, Type::UInt64], outcome),
            Self::Rename => (vec![Type::String, Type::String], outcome),
            Self::CreateDir
            | Self::RemoveDir
            | Self::Metadata
            | Self::RemoveFile
            | Self::ReadDir => (vec![Type::String], outcome),
            Self::Stdin => (vec![], outcome),
            Self::Process => (
                vec![
                    Type::UInt64,
                    Type::UInt64,
                    Type::reference(Type::String, false),
                ],
                outcome,
            ),
            Self::TestInput => (vec![Type::UInt64, Type::String, Type::String], outcome),
            Self::Read => (vec![file, Type::UInt64], outcome),
            Self::Write => (
                vec![
                    file,
                    Type::reference(Type::BufferSlice(Arc::new(Type::UInt8)), false),
                ],
                outcome,
            ),
            Self::Seek => (vec![file, Type::UInt64, Type::UInt64], outcome),
            Self::Sync | Self::Close => (vec![file], outcome),
            Self::Code | Self::Kind | Self::Count => (vec![result], Type::UInt64),
            Self::Message => (vec![result], Type::String),
            Self::TakeFile => (vec![result], owner),
            Self::TakeBytes => (vec![result], sig.ret.clone()),
        };
        if !suspends
            || sig.params != params
            || sig.ret != ret
            || self == Self::TakeBytes && !bytes(&sig.ret)
        {
            return Err(failure(
                "File operation requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}
fn validate_path(path: &str) -> Result<(), FsError> {
    if path.len() > 4096 {
        Err(FsError::new(8, "File path exceeds 4096 bytes"))
    } else if path.is_empty() || path.contains('\0') {
        Err(FsError::new(5, "Invalid file path"))
    } else {
        Ok(())
    }
}
pub fn valid_open_flags(flags: u64) -> bool {
    let read = flags & 1 != 0;
    let write = flags & 2 != 0;
    let append = flags & 4 != 0;
    let create = flags & 8 != 0;
    let new = flags & 16 != 0;
    let truncate = flags & 32 != 0;
    flags & !63 == 0
        && (read || write || append)
        && !(write && append)
        && !(create && new)
        && (!(create || new || truncate) || write || append)
        && (!truncate || write && !new)
}
struct FileOwner {
    _charge: Option<ExternalCharge>,
    file: Arc<FileCell>,
    service: FileService,
}
impl Drop for FileOwner {
    fn drop(&mut self) {
        self.file.close(None);
    }
}
struct Outcome {
    packet: Packet,
    service: Option<FileService>,
    _charge: ExternalCharge,
}
unsafe extern "C" fn release_file(p: *mut c_void) {
    drop(Box::from_raw(p.cast::<FileOwner>()));
}
unsafe extern "C" fn release_outcome(p: *mut c_void) {
    drop(Box::from_raw(p.cast::<Outcome>()));
}
fn adopt<T>(
    arena: &mut Arena,
    value: T,
    release: unsafe extern "C" fn(*mut c_void),
) -> Result<NonNull<Buffer>, Error> {
    let pointer = Box::into_raw(Box::new(value)).cast();
    let resource = match OwnedResource::from_token(NonNull::new(pointer).unwrap(), release) {
        Ok(r) => r,
        Err(e) => {
            unsafe { release(pointer) };
            return Err(e);
        }
    };
    Buffer::external(arena, resource)
}
enum Pending {
    Request(Arc<Ticket>),
    Close(Arc<FileCell>),
}
struct Request {
    reason: u32,
    pending: Pending,
    service: FileService,
    _lease: Option<ResourceLease>,
    _charge: ExternalCharge,
}
impl Drop for Request {
    fn drop(&mut self) {
        match &self.pending {
            Pending::Request(t) => t.stop(self.reason),
            Pending::Close(f) => f.stop_close(self.reason),
        }
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    let p = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !p.is_null() {
        let mut request = Box::from_raw(p.cast::<Request>());
        request.reason = reason;
        drop(request);
    }
}
unsafe fn lease(slot: u64) -> Result<ResourceLease, Error> {
    (slot as usize as *mut Buffer)
        .as_mut()
        .ok_or_else(|| failure("Null file resource"))?
        .resource_mut()?
        .lease()
}
fn outcome(arena: &mut Arena, packet: Packet, service: Option<FileService>) -> Result<u64, Error> {
    let size = 4096
        + std::mem::size_of::<Outcome>()
        + match &packet.data {
            Some(Data::Bytes(b)) => b.capacity(),
            _ => 0,
        }
        + packet.error.as_ref().map_or(0, |e| e.message.capacity());
    let charge = arena.reserve_external(size)?;
    Ok(adopt(
        arena,
        Outcome {
            packet,
            service,
            _charge: charge,
        },
        release_outcome,
    )?
    .as_ptr() as usize as u64)
}
unsafe fn extract(arena: &mut Arena, frame: &CpsFrame, op: Operation) -> Result<u64, Error> {
    let loan = lease(frame.slots[0])?;
    let result = &mut *loan.token().cast::<Outcome>();
    if op == Operation::Code {
        return Ok(result
            .packet
            .error
            .as_ref()
            .map_or(0, |e| u64::from(e.code)));
    }
    if op == Operation::Message {
        let message = result
            .packet
            .error
            .as_ref()
            .ok_or_else(|| failure("Successful file result has no error message"))?;
        return Ok(Buffer::copy_bytes(arena, message.message.as_bytes())?.as_ptr() as usize as u64);
    }
    if result.packet.error.is_some() {
        return Err(failure("Cannot extract a failed file result"));
    }
    if op == Operation::Kind {
        return match &result.packet.data {
            Some(Data::Metadata(m)) => Ok(m.kind as u64),
            _ => Err(failure("Expected file metadata")),
        };
    }
    match (op, result.packet.data.take()) {
        (Operation::Count, Some(Data::Metadata(m))) => Ok(m.size),
        (Operation::TakeFile, Some(Data::File(file))) => {
            let service = result
                .service
                .clone()
                .ok_or_else(|| failure("File result lost its service"))?;
            let mut owner = FileOwner {
                file,
                service,
                _charge: None,
            };
            owner._charge = Some(arena.reserve_external(std::mem::size_of::<FileOwner>())?);
            Ok(adopt(arena, owner, release_file)?.as_ptr() as usize as u64)
        }
        (Operation::TakeBytes, Some(Data::Bytes(bytes))) => {
            Ok(Buffer::copy_bytes(arena, &bytes)?.as_ptr() as usize as u64)
        }
        (Operation::Count, Some(Data::Count(n))) => Ok(n),
        (_, data) => {
            result.packet.data = data;
            Err(failure("File result was consumed or has a different type"))
        }
    }
}
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if super::parallel::pool::active() {
        return super::parallel::control::poll(
            arena,
            frame,
            super::parallel::control::Operation::Fs(op),
        );
    }
    if matches!(frame.state, DONE | FAILED) {
        return Err(failure("File operation has terminated"));
    }
    let result = (|| {
        if op == Operation::TestInput {
            let service = CURRENT.with(|s| s.borrow().clone());
            let key = super::owned_text::borrow(frame.slots[1] as usize as *const Buffer)?;
            let value = super::owned_text::borrow(frame.slots[2] as usize as *const Buffer)?;
            let result = service
                .as_ref()
                .ok_or_else(|| FsError::new(1, "Host does not provide a test context"))
                .and_then(|service| service.test_input(frame.slots[0], key, value));
            let packet = match result {
                Ok(Some(text)) => Packet::ok(Data::Bytes(text.into_bytes())),
                Ok(None) => Packet::ok(Data::Unit),
                Err(error) => Packet::error(error),
            };
            return outcome(arena, packet, service).map(Some);
        }
        if op == Operation::Process {
            let service = CURRENT.with(|s| s.borrow().clone());
            let key = super::owned_text::borrow(frame.slots[2] as usize as *const Buffer)?;
            let value = service
                .as_ref()
                .ok_or_else(|| FsError::new(1, "Host does not provide process inputs"))
                .and_then(|service| service.process_read(frame.slots[0], frame.slots[1], key));
            let packet = match value {
                Ok(super::process::Value::Count(n)) => Packet::ok(Data::Count(n)),
                Ok(super::process::Value::Text(text)) => Packet::ok(Data::Bytes(text.into_bytes())),
                Err(error) => Packet::error(error),
            };
            return outcome(arena, packet, service).map(Some);
        }
        if matches!(
            op,
            Operation::Code
                | Operation::Message
                | Operation::TakeFile
                | Operation::TakeBytes
                | Operation::Kind
                | Operation::Count
        ) {
            return extract(arena, frame, op).map(Some);
        }
        if frame.host_context.is_null() {
            let mut loan = None;
            let (service, file) = if matches!(
                op,
                Operation::Open
                    | Operation::Stdin
                    | Operation::Rename
                    | Operation::RemoveFile
                    | Operation::ReadDir
                    | Operation::CreateDir
                    | Operation::RemoveDir
                    | Operation::Metadata
            ) {
                (CURRENT.with(|s| s.borrow().clone()), None)
            } else {
                let l = lease(frame.slots[0])?;
                let owner = &*l.token().cast::<FileOwner>();
                let pair = (Some(owner.service.clone()), Some(owner.file.clone()));
                loan = Some(l);
                pair
            };
            let mut immediate = None;
            let action = match op {
                Operation::Stdin => Input::Stdin,
                Operation::Open
                | Operation::Rename
                | Operation::RemoveFile
                | Operation::ReadDir
                | Operation::CreateDir
                | Operation::RemoveDir
                | Operation::Metadata => {
                    let path = super::owned_text::borrow(frame.slots[0] as usize as *const Buffer)?;
                    immediate = validate_path(path).err();
                    match op {
                        Operation::Open => {
                            if immediate.is_none() && !valid_open_flags(frame.slots[1]) {
                                immediate = Some(FsError::new(5, "Invalid open options"));
                            }
                            Input::Open(path, frame.slots[1])
                        }
                        Operation::Rename => {
                            let to = super::owned_text::borrow(
                                frame.slots[1] as usize as *const Buffer,
                            )?;
                            immediate = immediate.or_else(|| validate_path(to).err());
                            Input::Rename(path, to)
                        }
                        Operation::RemoveFile => Input::RemoveFile(path),
                        Operation::CreateDir => Input::CreateDir(path),
                        Operation::RemoveDir => Input::RemoveDir(path),
                        Operation::Metadata => Input::Metadata(path),
                        Operation::ReadDir => Input::ReadDir(path),
                        _ => unreachable!(),
                    }
                }
                Operation::Read => {
                    let n = frame.slots[1];
                    if n == 0 {
                        immediate = Some(FsError::new(5, "Read limit must be positive"));
                    } else if n > 1048576 {
                        immediate = Some(FsError::new(8, "Read limit exceeds 1 MiB"));
                    }
                    Input::Read(n)
                }
                Operation::Write => {
                    let bytes = (&*(frame.slots[1] as usize as *const Buffer)).bytes()?;
                    if bytes.len() > 1048576 {
                        immediate = Some(FsError::new(8, "Write exceeds 1 MiB"));
                    }
                    Input::Write(bytes)
                }
                Operation::Seek => {
                    if frame.slots[1] > 2 {
                        immediate = Some(FsError::new(5, "Invalid seek origin"));
                    }
                    Input::Seek(
                        u32::try_from(frame.slots[1]).unwrap_or(u32::MAX),
                        frame.slots[2],
                    )
                }
                Operation::Sync | Operation::Close => Input::Sync,
                _ => unreachable!(),
            };
            if op != Operation::Close && file.as_ref().is_some_and(|f| f.is_closed()) {
                immediate = Some(FsError::new(7, "File is closed"));
            }
            // Foreign services own cancellation phase and logical close state.
            // They must check Closed before validating file-operation arguments;
            // do not hide that state behind local invalid-input/empty-write paths.
            let foreign_file = matches!(file.as_deref(), Some(FileCell::External(_)));
            if let Some(error) = immediate.filter(|error| !foreign_file || error.code == 7) {
                return outcome(arena, Packet::error(error), service).map(Some);
            }
            let Some(service) = service else {
                return outcome(
                    arena,
                    Packet::error(FsError::new(1, "Host does not provide a file service")),
                    None,
                )
                .map(Some);
            };
            if !foreign_file && matches!(&action, Input::Write(bytes) if bytes.is_empty()) {
                return outcome(arena, Packet::ok(Data::Count(0)), Some(service)).map(Some);
            }
            let waker =
                current_waker().ok_or_else(|| failure("File I/O requires a task executor"))?;
            let size = std::mem::size_of::<Request>() + 4096 + 2 * action.size();
            let charge = arena.reserve_external(size)?;
            let pending = if op == Operation::Close {
                let file = file.unwrap();
                file.close(Some(waker));
                Pending::Close(file)
            } else {
                match service.submit(action, file, waker) {
                    Ok(ticket) => Pending::Request(ticket),
                    Err(error) => {
                        return outcome(arena, Packet::error(error), Some(service)).map(Some)
                    }
                }
            };
            frame.host_context = Box::into_raw(Box::new(Request {
                reason: super::borrowed_host::STOP_CANCELLED,
                pending,
                service,
                _lease: loan,
                _charge: charge,
            }))
            .cast();
            frame.host_stop = Some(stop);
        }
        let request = &*frame.host_context.cast::<Request>();
        let packet = match &request.pending {
            Pending::Request(ticket) => ticket.take(),
            Pending::Close(file) => file.close_result().map(|r| match r {
                Ok(()) => Packet::ok(Data::Unit),
                Err(e) => Packet::error(e),
            }),
        };
        packet
            .map(|packet| outcome(arena, packet, Some(request.service.clone())))
            .transpose()
    })();
    match &result {
        Ok(None) => {}
        Ok(Some(value)) => {
            frame.stop_host(super::borrowed_host::STOP_COMPLETED);
            frame.result = *value;
            frame.state = DONE;
        }
        Err(_) => {
            frame.stop_host(super::borrowed_host::STOP_FAILED);
            frame.state = FAILED;
        }
    }
    result
}
#[export_name = "__joky_fs_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame, kind: u32) -> i32 {
    let op = match kind {
        1 => Operation::Open,
        2 => Operation::Read,
        3 => Operation::Write,
        4 => Operation::Seek,
        5 => Operation::Sync,
        6 => Operation::Close,
        7 => Operation::Code,
        8 => Operation::Message,
        9 => Operation::TakeFile,
        10 => Operation::TakeBytes,
        11 => Operation::Count,
        12 => Operation::Process,
        13 => Operation::Stdin,
        14 => Operation::TestInput,
        15 => Operation::Rename,
        16 => Operation::RemoveFile,
        17 => Operation::ReadDir,
        18 => Operation::CreateDir,
        19 => Operation::RemoveDir,
        20 => Operation::Metadata,
        21 => Operation::Kind,
        _ => return super::cps::INVALID_STATE,
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(value)) => {
            if op.returns_bytes() {
                (&mut (*frame).slots)[1] = value;
            }
            0
        }
        Ok(None) => 1,
        Err(e) => {
            super::error::record_error(e);
            super::cps::RUNTIME_ERROR
        }
    }
}

/// Drive a standalone CLI entry with an explicitly installed native file service.
///
/// # Safety
/// The frame and entry must satisfy `joky_cps_block_on`'s safety requirements.
#[no_mangle]
pub unsafe extern "C" fn joky_fs_block_on_native(
    entry: super::cps::CpsEntry,
    frame: *mut CpsFrame,
) -> i32 {
    let service = match FileService::standalone() {
        Ok(service) => service,
        Err(error) => {
            (*frame).release();
            super::error::record_error(error);
            return super::cps::RUNTIME_ERROR;
        }
    };
    let _scope = enter(Some(service));
    super::driver::joky_cps_block_on(entry, frame)
}

/// Drive an AOT root with an explicitly supplied host descriptor.
/// # Safety
/// `descriptor` is readable and obeys `FileServiceV1`'s contract. Entry and frame
/// obey `joky_cps_block_on`'s requirements. This does not transfer the caller's
/// descriptor reference; the runtime retains context during the execution.
#[no_mangle]
pub unsafe extern "C" fn joky_fs_block_on_v1(
    entry: super::cps::CpsEntry,
    frame: *mut CpsFrame,
    descriptor: *const abi::FileServiceV1,
) -> i32 {
    let service = descriptor
        .as_ref()
        .ok_or_else(|| failure("Null file service descriptor"))
        .and_then(|d| FileService::from_descriptor(*d));
    match service {
        Ok(service) => {
            let _scope = enter(Some(service));
            super::driver::joky_cps_block_on(entry, frame)
        }
        Err(error) => {
            (*frame).release();
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
