//! Uniquely owned files. Providers retain only the Rust resource across Pending;
//! borrowed language handles remain with the caller; close's owned handle is
//! released by the continuation, including on errors and cancellation.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::runtime::managed::{jk_alloc_native_handle, valid_header, RuntimeValueKind};
use crate::runtime::scope::ScopeId;

const MAGIC: u64 = 0x4a4b_4649_4c45_0001;
const WORD: usize = std::mem::size_of::<usize>();

struct Resource {
    file: Mutex<Option<File>>,
    busy: AtomicBool,
    scope: ScopeId,
    lease: Option<crate::runtime::resources::Lease>,
    #[cfg(test)]
    executing: AtomicBool,
}

impl Drop for Resource {
    fn drop(&mut self) {
        let lease = self.lease.take();
        if let Some(file) = self.file.get_mut().expect("file mutex").take() {
            blocking::enqueue_cleanup(move || {
                drop(file);
                drop(lease);
            });
        }
    }
}

#[repr(C)]
struct NativeFile {
    magic: u64,
    resource: Arc<Resource>,
}

unsafe extern "C" fn drop_file(pointer: *mut u8) {
    std::ptr::drop_in_place(pointer.cast::<NativeFile>());
}

fn allocate(file: File, scope: &crate::runtime::scope::RuntimeScope) -> *mut u8 {
    let resource = Arc::new(Resource {
        file: Mutex::new(Some(file)),
        busy: AtomicBool::new(false),
        scope: scope.id(),
        lease: Some(crate::runtime::resources::Lease::new(
            scope,
            crate::runtime::resources::Kind::FileHandles,
            1,
        )),
        #[cfg(test)]
        executing: AtomicBool::new(false),
    });
    let pointer = jk_alloc_native_handle(
        std::mem::size_of::<NativeFile>(),
        std::mem::align_of::<NativeFile>(),
        Some(drop_file),
    );
    if !pointer.is_null() {
        unsafe {
            pointer.cast::<NativeFile>().write(NativeFile {
                magic: MAGIC,
                resource,
            });
        }
    }
    pointer
}

unsafe fn resource(pointer: *mut u8, scope: ScopeId) -> Option<Arc<Resource>> {
    let header = valid_header(pointer)?;
    if header.kind != RuntimeValueKind::NativeHandle as u8
        || header.payload_size != std::mem::size_of::<NativeFile>()
    {
        return None;
    }
    let value = &*pointer.cast::<NativeFile>();
    (value.magic == MAGIC && value.resource.scope == scope).then(|| Arc::clone(&value.resource))
}

struct Reservation(Arc<Resource>);

impl Reservation {
    fn acquire(resource: Arc<Resource>) -> Option<Self> {
        resource
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self(resource))
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

fn error(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    words: usize,
    message: &str,
) {
    let pointer = jk_string_from_utf8(message.as_ptr(), message.len());
    let mut payload = [0usize; 4];
    payload[0] = 1;
    payload[words - 2] = pointer as usize;
    payload[words - 1] = message.len();
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        words * WORD,
    ) {
        jk_drop(pointer);
    }
}

pub(super) unsafe extern "C" fn open_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    _: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments.is_null() || size != 3 * WORD || result_size != 4 * WORD {
        return 0;
    }
    let pointer = std::ptr::read_unaligned(arguments.cast::<*mut u8>());
    let length = std::ptr::read_unaligned(arguments.add(WORD).cast::<usize>());
    if jk_string_len(pointer) != length {
        return 0;
    }
    let Some(path) = SharedPath::retain(pointer, length) else {
        return 0;
    };
    let mode = std::ptr::read_unaligned(arguments.add(2 * WORD).cast::<u32>());
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let pending = continuation.clone();
    let scope = crate::runtime::scope::current_or_default();
    let address = handle as usize;
    blocking::enqueue(
        (address, operation),
        || continuation.is_cancelled(),
        move || {
            let _scope_guard = pending.enter_scope();
            if pending.is_cancelled() {
                return;
            }
            let opened = path.as_str().and_then(|path| {
                let mut options = OpenOptions::new();
                match mode {
                    0 => {
                        options.read(true);
                    }
                    1 => {
                        options.write(true).create(true).truncate(true);
                    }
                    2 => {
                        options.append(true).create(true);
                    }
                    3 => {
                        options.read(true).write(true);
                    }
                    4 => {
                        options.read(true).write(true).create_new(true);
                    }
                    _ => return Err("invalid file mode".to_owned()),
                }
                options.open(path).map_err(|e| e.to_string())
            });
            match opened {
                Ok(file) => {
                    let pointer = allocate(file, &scope);
                    if pointer.is_null() {
                        error(
                            &pending,
                            address as *mut Continuation,
                            operation,
                            4,
                            "could not allocate file handle",
                        );
                        return;
                    }
                    let payload = [0usize, pointer as usize, 0, 0];
                    if !pending.complete_suspend_with_payload(
                        address as *mut Continuation,
                        operation,
                        payload.as_ptr().cast(),
                        4 * WORD,
                    ) {
                        jk_drop(pointer);
                    }
                }
                Err(message) => error(
                    &pending,
                    address as *mut Continuation,
                    operation,
                    4,
                    &message,
                ),
            }
        },
    );
    1
}

#[derive(Clone, Copy)]
enum Operation {
    Read,
    Write,
    Position,
    Seek,
    Flush,
    Sync,
    Close,
}

macro_rules! start {
    ($name:ident, $kind:ident) => {
        pub(super) unsafe extern "C" fn $name(
            handle: *mut Continuation,
            operation: u64,
            arguments: *const u8,
            size: usize,
            _: *mut u8,
            result_size: usize,
        ) -> u8 {
            start_operation(
                handle,
                operation,
                arguments,
                size,
                result_size,
                Operation::$kind,
            )
        }
    };
}
start!(read_start, Read);
start!(write_start, Write);
start!(position_start, Position);
start!(seek_start, Seek);
start!(flush_start, Flush);
start!(sync_start, Sync);
start!(close_start, Close);

unsafe fn start_operation(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    result_size: usize,
    kind: Operation,
) -> u8 {
    let argument_words = match kind {
        Operation::Read | Operation::Write => 2,
        Operation::Seek => 3,
        _ => 1,
    };
    let result_words = match kind {
        Operation::Flush | Operation::Sync | Operation::Close => 3,
        _ => 4,
    };
    if arguments.is_null() || size != argument_words * WORD || result_size != result_words * WORD {
        return 0;
    }
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let pointer = std::ptr::read_unaligned(arguments.cast::<*mut u8>());
    let Some(resource) = resource(pointer, continuation.scope_id()) else {
        error(
            &continuation,
            handle,
            operation,
            result_words,
            "invalid file handle",
        );
        return 1;
    };
    let Some(reservation) = Reservation::acquire(resource) else {
        error(
            &continuation,
            handle,
            operation,
            result_words,
            "file is busy",
        );
        return 1;
    };
    let value = if argument_words > 1 {
        std::ptr::read_unaligned(arguments.add(WORD).cast::<usize>())
    } else {
        0
    };
    let origin = if matches!(kind, Operation::Seek) {
        std::ptr::read_unaligned(arguments.add(2 * WORD).cast::<u32>())
    } else {
        0
    };
    let data = if matches!(kind, Operation::Write) {
        let pointer = value as *mut u8;
        if crate::runtime::bytes::jk_bytes_data(pointer).is_null() {
            return 0;
        }
        let length = crate::runtime::bytes::jk_bytes_length(pointer);
        let retained = jk_dup(pointer);
        if retained.is_null() {
            return 0;
        }
        Some(SharedData::new(retained, length))
    } else {
        None
    };
    let pending = continuation.clone();
    let address = handle as usize;
    blocking::enqueue(
        (address, operation),
        || continuation.is_cancelled(),
        move || {
            let _scope_guard = pending.enter_scope();
            if pending.is_cancelled() {
                return;
            }
            let result = execute(&reservation.0, kind, value, origin, data.as_ref(), || {
                pending.is_cancelled()
            });
            // Release the admission reservation before publishing: resumed code may
            // immediately begin another operation on the same file.
            drop(reservation);
            match result {
                Ok(Output::Bytes(bytes)) => complete_read(
                    &pending,
                    address as *mut Continuation,
                    operation,
                    Ok(bytes),
                    false,
                ),
                Ok(Output::Count(count)) => {
                    complete_count(&pending, address as *mut Continuation, operation, Ok(count))
                }
                Ok(Output::Unit) => {
                    let payload = [0usize; 3];
                    pending.complete_suspend_with_payload(
                        address as *mut Continuation,
                        operation,
                        payload.as_ptr().cast(),
                        3 * WORD,
                    );
                }
                Err(message) => error(
                    &pending,
                    address as *mut Continuation,
                    operation,
                    result_words,
                    &message,
                ),
            }
        },
    );
    1
}

enum Output {
    Bytes(Vec<u8>),
    Count(usize),
    Unit,
}

fn execute(
    resource: &Resource,
    kind: Operation,
    value: usize,
    origin: u32,
    data: Option<&SharedData>,
    cancelled: impl Fn() -> bool,
) -> Result<Output, String> {
    #[cfg(test)]
    resource.executing.store(true, Ordering::Release);
    let mut guard = resource.file.lock().expect("file mutex");
    if matches!(kind, Operation::Close) {
        drop(guard.take());
        return Ok(Output::Unit);
    }
    let file = guard.as_mut().ok_or("file is closed")?;
    match kind {
        Operation::Read => {
            if value == 0 {
                return Err("max_bytes must be positive".to_owned());
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(value).map_err(|e| e.to_string())?;
            bytes.resize(value, 0);
            let count = loop {
                match file.read(&mut bytes) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => break result.map_err(|e| e.to_string())?,
                }
            };
            bytes.truncate(count);
            Ok(Output::Bytes(bytes))
        }
        Operation::Write => {
            let bytes = data.expect("write data").as_bytes();
            write_fully(file, bytes, cancelled).map(Output::Count)
        }
        Operation::Position => file
            .stream_position()
            .map(|p| Output::Count(p as usize))
            .map_err(|e| e.to_string()),
        Operation::Seek => {
            let offset = value as i64;
            let from = match origin {
                0 if offset >= 0 => SeekFrom::Start(offset as u64),
                1 => SeekFrom::Current(offset),
                2 => SeekFrom::End(offset),
                _ => return Err("invalid seek origin or negative absolute offset".to_owned()),
            };
            file.seek(from)
                .map(|p| Output::Count(p as usize))
                .map_err(|e| e.to_string())
        }
        Operation::Flush => file
            .flush()
            .map(|_| Output::Unit)
            .map_err(|e| e.to_string()),
        Operation::Sync => file
            .sync_all()
            .map(|_| Output::Unit)
            .map_err(|e| e.to_string()),
        Operation::Close => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::continuation::*;
    use std::time::{Duration, Instant};

    fn wait_until(ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "file resource did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn explicitly_closed_file_releases_its_resource_lease() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _guard = scope.enter();
        let pointer = allocate(
            File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap(),
            &scope,
        );
        let retained = unsafe { resource(pointer, scope.id()) }.unwrap();

        assert!(matches!(
            execute(&retained, Operation::Close, 0, 0, None, || false),
            Ok(Output::Unit)
        ));
        jk_drop(pointer);
        drop(retained);

        wait_until(|| scope.resources.snapshot()[5] == 0);
    }

    #[test]
    fn cancellation_keeps_inflight_file_alive_and_close_rejects_busy_handle() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _guard = scope.enter();
        let entries = [
            crate::runtime::provider::ProviderOperationEntry {
                effect: "file",
                name: "read_chunk",
                operation: 5,
            },
            crate::runtime::provider::ProviderOperationEntry {
                effect: "file",
                name: "close",
                operation: 11,
            },
        ];
        let _provider = super::super::register_operations(&entries).unwrap();
        let operation = |name| match name {
            "read_chunk" => 5,
            "close" => 11,
            _ => unreachable!(),
        };
        let pointer = allocate(
            File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap(),
            &scope,
        );
        let retained = unsafe { resource(pointer, scope.id()) }.unwrap();
        assert!(
            unsafe { resource(pointer, crate::runtime::scope::RuntimeScope::new().id()) }.is_none()
        );
        let file_guard = retained.file.lock().unwrap();
        let read = jk_continuation_new(1);
        let arguments = [pointer as usize, 64];
        unsafe {
            jk_continuation_alloc_suspend_result(read, 4 * WORD);
            assert_eq!(
                jk_continuation_start_suspend_payload(
                    read,
                    operation("read_chunk"),
                    arguments.as_ptr().cast(),
                    2 * WORD
                ),
                1
            );
        }
        wait_until(|| retained.executing.load(Ordering::Acquire));
        let close = jk_continuation_new(1);
        unsafe {
            jk_continuation_alloc_suspend_result(close, 3 * WORD);
            jk_continuation_register_cleanup(
                close,
                joky_runtime_abi::CONTINUATION_SUSPEND_ARGUMENT_STORAGE,
                0,
                jk_drop as *mut std::ffi::c_void,
            );
            assert_eq!(
                jk_continuation_start_suspend_payload(
                    close,
                    operation("close"),
                    arguments.as_ptr().cast(),
                    WORD
                ),
                1
            );
            let result = jk_continuation_suspend_result_pointer(close).cast::<usize>();
            assert_eq!(result.read_unaligned(), 1);
            let message = result.add(1).read_unaligned() as *mut u8;
            let length = result.add(2).read_unaligned();
            assert_eq!(std::slice::from_raw_parts(message, length), b"file is busy");
            jk_drop(message);
            // Even a rejected close consumes its language handle. The running
            // read still owns a separate Arc, so its OS resource stays alive.
            assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
            jk_continuation_free(close);
            assert_eq!(jk_continuation_cancel(read), 1);
            jk_continuation_free(read);
        }
        assert_eq!(scope.managed_objects.load(Ordering::SeqCst), 0);
        assert_eq!(
            scope.resources.snapshot()[5],
            1,
            "inflight operation owns the OS resource"
        );
        drop(file_guard);
        wait_until(|| !retained.busy.load(Ordering::Acquire));
        drop(retained);
        wait_until(|| scope.resources.snapshot()[5] == 0);
        assert_eq!(scope.resources.snapshot()[4], 0);
    }
}
