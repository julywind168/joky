use joky::runtime::fs::{abi::*, FileService};
use std::{
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
#[derive(Default)]
pub struct Stats {
    pub mode: usize,
    pub files: AtomicUsize,
    pub results: AtomicUsize,
    pub operations: AtomicUsize,
    pub stops: AtomicUsize,
}
struct File {
    closed: bool,
    bytes: Vec<u8>,
    position: usize,
}
struct ResultStorage {
    bytes: Vec<u8>,
}
struct Operation {
    file: *mut c_void,
    pending: bool,
    result: FileResultV1,
}
unsafe extern "C" fn retain(p: *mut c_void) {
    Arc::increment_strong_count(p.cast::<Stats>());
}
unsafe extern "C" fn release(p: *mut c_void) {
    Arc::decrement_strong_count(p.cast::<Stats>());
}
unsafe extern "C" fn release_file(p: *mut c_void, f: *mut c_void) {
    drop(Box::from_raw(f.cast::<File>()));
    (*p.cast::<Stats>()).files.fetch_sub(1, Ordering::SeqCst);
}
unsafe extern "C" fn release_result(p: *mut c_void, r: *mut c_void) {
    drop(Box::from_raw(r.cast::<ResultStorage>()));
    (*p.cast::<Stats>()).results.fetch_sub(1, Ordering::SeqCst);
}
unsafe fn storage(stats: &Stats, bytes: Vec<u8>, r: &mut FileResultV1) {
    let b = Box::new(ResultStorage { bytes });
    r.bytes = b.bytes.as_ptr();
    r.bytes_len = b.bytes.len() as u64;
    r.result_token = Box::into_raw(b).cast();
    stats.results.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn submit(
    p: *mut c_void,
    input: *const FileInputV1,
    wake: FileWakeV1,
    operation: *mut *mut c_void,
    out: *mut FileResultV1,
) -> u32 {
    let stats = &*p.cast::<Stats>();
    let input = &*input;
    let mut result = FileResultV1::default();
    if input.operation_code != 1 {
        let closed = (*input.file_token.cast::<File>()).closed;
        let code = if closed {
            7
        } else if input.operation_code == 2 && input.limit == 0 {
            5
        } else if input.operation_code == 2 && input.limit > 1048576
            || input.operation_code == 3 && input.bytes_len > 1048576
        {
            8
        } else if input.operation_code == 4 && input.origin > 2 {
            5
        } else {
            0
        };
        if code != 0 {
            out.write(FileResultV1 {
                error_code: code,
                ..Default::default()
            });
            return 1;
        }
    }
    let pending = stats.mode >= 5 && input.operation_code == 2;
    if pending {
        *operation = Box::into_raw(Box::new(Operation {
            file: input.file_token,
            pending: true,
            result,
        }))
        .cast();
        stats.operations.fetch_add(1, Ordering::SeqCst);
        (wake.wake)(wake.context);
        return 0;
    }
    match input.operation_code {
        1 => {
            result.file_token = Box::into_raw(Box::new(File {
                closed: false,
                bytes: Vec::new(),
                position: 0,
            }))
            .cast();
            stats.files.fetch_add(1, Ordering::SeqCst);
        }
        2 => {
            let file = &mut *input.file_token.cast::<File>();
            let end = (file.position + input.limit as usize).min(file.bytes.len());
            let start = file.position.min(end);
            storage(stats, file.bytes[start..end].to_vec(), &mut result);
            file.position = end;
        }
        3 => {
            let file = &mut *input.file_token.cast::<File>();
            let bytes = std::slice::from_raw_parts(input.bytes, input.bytes_len as usize);
            let end = file.position + bytes.len();
            file.bytes.resize(end, 0);
            file.bytes[file.position..end].copy_from_slice(bytes);
            file.position = end;
            result.count = bytes.len() as u64;
        }
        4 => {
            let file = &mut *input.file_token.cast::<File>();
            file.position = input.offset_bits as usize;
            result.count = file.position as u64;
        }
        5 | 6 => {}
        _ => panic!("invalid fixture operation"),
    }
    if input.operation_code == 1 && (1..=4).contains(&stats.mode) {
        storage(stats, vec![0xff], &mut result);
        // Malformed outputs must still return both owning tokens to their host.
        result.error_code = 10;
        if stats.mode == 2 {
            result.message = result.bytes;
            result.message_len = 1;
            result.bytes = ptr::null();
            result.bytes_len = 0;
        }
        if stats.mode == 3 {
            *out = result;
            return 1;
        }
    }
    *operation = Box::into_raw(Box::new(Operation {
        file: input.file_token,
        pending: false,
        result,
    }))
    .cast();
    stats.operations.fetch_add(1, Ordering::SeqCst);
    (wake.retain)(wake.context);
    (wake.wake)(wake.context);
    (wake.release)(wake.context);
    0
}
unsafe extern "C" fn poll(p: *mut c_void, operation: *mut c_void, out: *mut FileResultV1) -> u32 {
    if (*operation.cast::<Operation>()).pending {
        return 1;
    }
    *out = std::mem::take(&mut (*operation.cast::<Operation>()).result);
    if (*p.cast::<Stats>()).mode == 4 {
        1
    } else {
        0
    }
}
unsafe extern "C" fn stop(p: *mut c_void, operation: *mut c_void, reason: u32) {
    let operation = Box::from_raw(operation.cast::<Operation>());
    if operation.pending && reason == 2 && (*p.cast::<Stats>()).mode == 5 {
        (*operation.file.cast::<File>()).closed = true;
    }
    let r = operation.result;
    if !r.file_token.is_null() {
        release_file(p, r.file_token);
    }
    if !r.result_token.is_null() {
        release_result(p, r.result_token);
    }
    let stats = &*p.cast::<Stats>();
    stats.operations.fetch_sub(1, Ordering::SeqCst);
    stats.stops.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn shutdown(_: *mut c_void) {}
unsafe extern "C" fn quiet(p: *mut c_void) -> u32 {
    let s = &*p.cast::<Stats>();
    u32::from(
        s.files.load(Ordering::SeqCst) == 0
            && s.results.load(Ordering::SeqCst) == 0
            && s.operations.load(Ordering::SeqCst) == 0,
    )
}
pub fn descriptor(stats: &Arc<Stats>) -> FileServiceV1 {
    FileServiceV1 {
        abi_version: 1,
        struct_size: std::mem::size_of::<FileServiceV1>() as u32,
        reserved: 0,
        context: Arc::as_ptr(stats).cast_mut().cast(),
        retain: Some(retain),
        release: Some(release),
        submit: Some(submit),
        poll: Some(poll),
        stop: Some(stop),
        release_result: Some(release_result),
        release_file: Some(release_file),
        shutdown: Some(shutdown),
        is_quiescent: Some(quiet),
    }
}
pub fn suite(mut run: impl FnMut(FileService, &str) -> u64) {
    for mode in 0..=6 {
        let stats = Arc::new(Stats {
            mode,
            ..Default::default()
        });
        let service = unsafe { FileService::from_descriptor(descriptor(&stats)) }.unwrap();
        assert_eq!(
            run(
                service,
                match mode {
                    0 => "run",
                    5 => "cancel_started",
                    6 => "cancel_queued",
                    _ => "malformed",
                }
            ),
            42,
            "mode {mode}"
        );
        assert_eq!(stats.files.load(Ordering::SeqCst), 0);
        assert_eq!(stats.results.load(Ordering::SeqCst), 0);
        assert_eq!(stats.operations.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&stats), 1);
    }
}
