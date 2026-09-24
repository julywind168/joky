//! Blocking file-system providers.
//!
//! Regular files have no readiness event suitable for the reactor. Providers
//! retain shared arguments, await bounded blocking-pool admission, and publish
//! typed Result payloads through the ordinary continuation completion path.

use super::blocking;
use super::continuation::Continuation;
use super::managed::{jk_drop, jk_dup};
use super::provider::ProviderScope;
use super::string::{jk_string_from_utf8, jk_string_len};

mod handles;

const STRING_RESULT_WORDS: usize = 5;
const BYTES_RESULT_WORDS: usize = 4;

/// Name-keyed hook table: the runtime's side of the registration contract.
/// A new file operation lands here and in the standard module's `eff`
/// declaration; no compiler change is involved.
fn operation_hooks() -> &'static [(&'static str, crate::runtime::provider::ProviderStart)] {
    &[
        ("read", file_read_start),
        ("read_bytes", file_read_bytes_start),
        ("write", file_write_start),
        ("write_bytes", file_write_bytes_start),
        ("open", handles::open_start),
        ("read_chunk", handles::read_start),
        ("write_chunk", handles::write_start),
        ("position", handles::position_start),
        ("seek", handles::seek_start),
        ("flush", handles::flush_start),
        ("sync", handles::sync_start),
        ("close", handles::close_start),
    ]
}

/// Register file hooks for `(name, operation_id)` entries derived from the
/// checked effect declarations. Unknown names are ignored.
pub(crate) fn register_operations(
    entries: &[crate::runtime::provider::ProviderOperationEntry],
) -> Option<ProviderScope> {
    crate::runtime::provider::register_named(operation_hooks(), file_read_cancel, entries)
}

unsafe extern "C" fn file_write_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    file_write_start_impl(
        handle,
        operation,
        arguments,
        arguments_size,
        result_size,
        false,
    )
}

unsafe extern "C" fn file_write_bytes_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    file_write_start_impl(
        handle,
        operation,
        arguments,
        arguments_size,
        result_size,
        true,
    )
}

unsafe fn file_write_start_impl(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    result_size: usize,
    bytes_mode: bool,
) -> u8 {
    let word = std::mem::size_of::<usize>();
    if arguments.is_null()
        || arguments_size != (if bytes_mode { 3 } else { 4 }) * word
        || result_size != 4 * word
    {
        return 0;
    }
    let path_pointer = std::ptr::read_unaligned(arguments.cast::<*mut u8>());
    let path_length = std::ptr::read_unaligned(arguments.add(word).cast::<usize>());
    if jk_string_len(path_pointer) != path_length {
        return 0;
    }
    let Some(path) = SharedPath::retain(path_pointer, path_length) else {
        return 0;
    };
    let pointer = std::ptr::read_unaligned(arguments.add(2 * word).cast::<*mut u8>());
    let length = if bytes_mode {
        if super::bytes::jk_bytes_data(pointer).is_null() {
            return 0;
        }
        super::bytes::jk_bytes_length(pointer)
    } else {
        let length = std::ptr::read_unaligned(arguments.add(3 * word).cast::<usize>());
        if jk_string_len(pointer) != length {
            return 0;
        }
        length
    };
    let retained = jk_dup(pointer);
    if retained.is_null() {
        return 0;
    }
    let data = SharedData::new(retained, length);
    let Some(continuation) = Continuation::retain_registered(handle) else {
        return 0;
    };
    let address = handle as usize;
    let pending = continuation.clone();
    blocking::enqueue(
        (address, operation),
        || continuation.is_cancelled(),
        move || {
            if pending.is_cancelled() {
                return;
            }
            let result = path.as_str().and_then(|path| {
                let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
                write_fully(&mut file, data.as_bytes(), || pending.is_cancelled())
            });
            complete_count(&pending, address as *mut Continuation, operation, result);
        },
    );
    1
}

struct SharedData(usize, usize, #[allow(dead_code)] super::resources::Lease);

impl SharedData {
    fn new(pointer: *mut u8, length: usize) -> Self {
        Self(
            pointer as usize,
            length,
            super::resources::Lease::new(
                &crate::runtime::scope::current_or_default(),
                super::resources::Kind::FileArgumentBytes,
                length,
            ),
        )
    }
    fn as_bytes(&self) -> &[u8] {
        // The immutable managed object is retained until this descriptor drops.
        unsafe { std::slice::from_raw_parts(self.0 as *const u8, self.1) }
    }
}

fn write_fully(
    writer: &mut impl std::io::Write,
    bytes: &[u8],
    cancelled: impl Fn() -> bool,
) -> Result<usize, String> {
    let mut written = 0;
    while written < bytes.len() {
        if cancelled() {
            return Err(format!("write cancelled after {written} bytes"));
        }
        match writer.write(&bytes[written..]) {
            Ok(0) => {
                return Err(format!(
                    "write failed after {written} bytes: write returned zero"
                ))
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("write failed after {written} bytes: {error}")),
        }
    }
    Ok(written)
}

impl Drop for SharedData {
    fn drop(&mut self) {
        jk_drop(self.0 as *mut u8);
    }
}

fn complete_count(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    result: Result<usize, String>,
) {
    if continuation.is_cancelled() {
        return;
    }
    let mut payload = [0usize; 4];
    match result {
        Ok(count) => payload[1] = count,
        Err(error) => {
            payload[0] = 1;
            payload[2] = jk_string_from_utf8(error.as_ptr(), error.len()) as usize;
            payload[3] = error.len();
        }
    }
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        std::mem::size_of_val(&payload),
    ) && payload[0] == 1
    {
        jk_drop(payload[2] as *mut u8);
    }
}

unsafe extern "C" fn file_read_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    file_read_start_impl(
        handle,
        operation,
        arguments,
        arguments_size,
        result_size,
        true,
    )
}

unsafe extern "C" fn file_read_bytes_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    file_read_start_impl(
        handle,
        operation,
        arguments,
        arguments_size,
        result_size,
        false,
    )
}

unsafe fn file_read_start_impl(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    arguments_size: usize,
    result_size: usize,
    validate_utf8: bool,
) -> u8 {
    if arguments.is_null()
        || arguments_size != 2 * std::mem::size_of::<usize>()
        || result_size
            != (if validate_utf8 {
                STRING_RESULT_WORDS
            } else {
                BYTES_RESULT_WORDS
            }) * std::mem::size_of::<usize>()
    {
        return 0;
    }
    let path_pointer = unsafe { std::ptr::read_unaligned(arguments.cast::<*mut u8>()) };
    let path_length = unsafe {
        std::ptr::read_unaligned(arguments.add(std::mem::size_of::<usize>()).cast::<usize>())
    };
    if jk_string_len(path_pointer) != path_length {
        return 0;
    }
    // Keep a share of the immutable string rather than copying every waiting
    // caller's path. Only executing workers allocate filesystem read buffers.
    let Some(path) = SharedPath::retain(path_pointer, path_length) else {
        return 0;
    };
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let handle_address = handle as usize;
    let pending = continuation.clone();
    blocking::enqueue(
        (handle_address, operation),
        || continuation.is_cancelled(),
        move || {
            run_read(
                &pending,
                handle_address as *mut Continuation,
                operation,
                || std::fs::read(path.as_str()?).map_err(|error| error.to_string()),
                validate_utf8,
            );
        },
    );
    1
}

// A retained immutable managed string may be read and released by any worker.
// Store the address as an integer so the descriptor is Send; Drop owns the share.
struct SharedPath(
    usize,
    usize,
    #[allow(dead_code)] [super::resources::Lease; 2],
);

impl SharedPath {
    fn retain(pointer: *mut u8, length: usize) -> Option<Self> {
        let retained = jk_dup(pointer);
        if retained.is_null() {
            return None;
        }
        Some(Self(
            retained as usize,
            length,
            [
                super::resources::Lease::new(
                    &crate::runtime::scope::current_or_default(),
                    super::resources::Kind::FileRequests,
                    1,
                ),
                super::resources::Lease::new(
                    &crate::runtime::scope::current_or_default(),
                    super::resources::Kind::FileArgumentBytes,
                    length,
                ),
            ],
        ))
    }
    fn as_str(&self) -> Result<&str, String> {
        // SAFETY: the provider validated the string and retains it through the
        // entire read. Runtime strings contain UTF-8 and cannot be mutated.
        let bytes = unsafe { std::slice::from_raw_parts(self.0 as *const u8, self.1) };
        std::str::from_utf8(bytes).map_err(|error| error.to_string())
    }
}

impl Drop for SharedPath {
    fn drop(&mut self) {
        jk_drop(self.0 as *mut u8);
    }
}

unsafe extern "C" fn file_read_cancel(handle: *mut Continuation, operation: u64) -> u8 {
    blocking::cancel((handle as usize, operation));
    1
}

fn run_read(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    read: impl FnOnce() -> Result<Vec<u8>, String>,
    validate_utf8: bool,
) {
    // Cancellation while queued must not start a new blocking syscall. Once
    // started, the syscall cannot be interrupted; discard its late result.
    if !continuation.is_cancelled() {
        complete_read(continuation, handle, operation, read(), validate_utf8);
    }
}

fn complete_read(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    result: Result<Vec<u8>, String>,
    validate_utf8: bool,
) {
    if continuation.is_cancelled() {
        return;
    }
    let (tag, bytes) = match result {
        Ok(bytes) if !validate_utf8 || std::str::from_utf8(&bytes).is_ok() => (0_usize, bytes),
        Ok(_) => (1_usize, b"file contents are not valid UTF-8".to_vec()),
        Err(error) => (1_usize, error.into_bytes()),
    };
    let value = if validate_utf8 || tag != 0 {
        jk_string_from_utf8(bytes.as_ptr(), bytes.len())
    } else {
        super::bytes::jk_bytes_from_data(bytes.as_ptr(), bytes.len())
    };
    if value.is_null() {
        return;
    }
    let mut payload = [0_usize; STRING_RESULT_WORDS];
    payload[0] = tag;
    let value_offset = if tag == 0 {
        1
    } else {
        if validate_utf8 {
            3
        } else {
            2
        }
    };
    payload[value_offset] = value as usize;
    if validate_utf8 || tag != 0 {
        payload[value_offset + 1] = bytes.len();
    }
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        payload.as_ptr().cast(),
        (if validate_utf8 {
            STRING_RESULT_WORDS
        } else {
            BYTES_RESULT_WORDS
        }) * std::mem::size_of::<usize>(),
    ) {
        jk_drop(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::continuation::{
        jk_continuation_cancel, jk_continuation_free, jk_continuation_new,
    };
    use crate::runtime::managed::live_object_count;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn writes_retry_short_and_interrupted_calls_and_report_partial_failures() {
        struct Writer {
            bytes: Vec<u8>,
            calls: usize,
            failure: Option<std::io::ErrorKind>,
        }
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                if self.calls == 1 {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                if self.bytes.len() == 4 {
                    if let Some(kind) = self.failure {
                        return Err(kind.into());
                    }
                }
                let count = bytes.len().min(2);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer {
            bytes: Vec::new(),
            calls: 0,
            failure: None,
        };
        assert_eq!(write_fully(&mut writer, b"abcdef", || false).unwrap(), 6);
        assert_eq!(writer.bytes, b"abcdef");
        let mut writer = Writer {
            bytes: Vec::new(),
            calls: 0,
            failure: Some(std::io::ErrorKind::PermissionDenied),
        };
        assert!(write_fully(&mut writer, b"abcdef", || false)
            .unwrap_err()
            .starts_with("write failed after 4 bytes:"));
        assert_eq!(writer.bytes, b"abcd");
        assert!(write_fully(&mut writer, b"ignored", || true)
            .unwrap_err()
            .starts_with("write cancelled after 0 bytes"));
        assert_eq!(writer.bytes, b"abcd");
        assert!(write_fully(&mut &mut [0u8; 0][..], b"x", || false)
            .unwrap_err()
            .contains("write returned zero"));
    }

    #[test]
    fn queued_read_cancelled_before_execution_does_not_call_the_filesystem() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _scope_guard = scope.enter();
        let handle = jk_continuation_new(1);
        let request = unsafe { Continuation::retain_registered(handle) }.unwrap();
        assert_eq!(unsafe { jk_continuation_cancel(handle) }, 1);
        unsafe { jk_continuation_free(handle) };
        run_read(
            &request,
            handle,
            0,
            || panic!("cancelled read started a syscall"),
            true,
        );
        assert_eq!(live_object_count(), 0);
    }

    #[test]
    fn cancellation_does_not_wait_for_running_read_and_discards_its_result() {
        let scope = crate::runtime::scope::RuntimeScope::new();
        let _scope_guard = scope.enter();
        let handle = jk_continuation_new(2);
        let request = unsafe { Continuation::retain_registered(handle) }.unwrap();
        let address = handle as usize;
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_read(
                &request,
                address as *mut Continuation,
                0,
                || {
                    started.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(b"late file contents".to_vec())
                },
                true,
            );
            assert_eq!(live_object_count(), 0);
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(unsafe { jk_continuation_cancel(handle) }, 1);
        unsafe { jk_continuation_free(handle) };
        // Provider's retained state remains observable after token removal;
        // it must not be confused with a leaked public handle or ready task.
        assert_eq!(scope.resources.snapshot()[0], 1);
        assert_eq!(
            crate::runtime::continuation::resource_snapshot(Some(scope.id())).scope_handles,
            0
        );
        let replacement = jk_continuation_new(2);
        assert_ne!(replacement, handle);
        // The read can only return after cancellation and public-handle free.
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            unsafe { crate::runtime::continuation::jk_continuation_state(replacement) },
            0
        );
        unsafe { jk_continuation_free(replacement) };
        assert_eq!(scope.resources.snapshot()[0], 0);
        assert_eq!(live_object_count(), 0);
    }
}
