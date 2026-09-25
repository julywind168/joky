//! OS randomness on the blocking pool; no user-space PRNG or weak fallback.
//!
//! Ownership by path:
//! - Bad ABI / unknown continuation: return 0, no owned input or queued work.
//! - Admission: copy only the scalar length, retain the continuation and scope.
//! - Queued cancellation: blocking::cancel releases all captures, no buffer yet.
//! - Running cancellation: OS calls may finish; late payloads are dropped here.
//! - Completion: one Bytes or String root transfers only on acceptance.
//!
//! No result-slot pointer is retained; all result allocation uses the caller's scope.

use super::blocking;
use super::bytes::jk_bytes_from_data;
use super::continuation::Continuation;
use super::managed::jk_drop;
use super::provider::ProviderScope;
use super::scope::current_or_default;
use super::string::jk_string_from_utf8;

const WORD: usize = std::mem::size_of::<usize>();
const MAX_LENGTH: u64 = 1024 * 1024;

pub(crate) fn register_operations(
    entries: &[super::provider::ProviderOperationEntry],
) -> Option<ProviderScope> {
    super::provider::register_named(&[("bytes", bytes_start)], cancel, entries)
}

unsafe extern "C" fn cancel(handle: *mut Continuation, operation: u64) -> u8 {
    blocking::cancel((handle as usize, operation));
    1
}

unsafe extern "C" fn bytes_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments.is_null() || size != std::mem::size_of::<u64>() || result_size != 4 * WORD {
        return 0;
    }
    let length = std::ptr::read_unaligned(arguments.cast::<u64>());
    if let Some(accepted) =
        super::provider::dispatch_handler(handle, operation, arguments, size, result_size, &[])
    {
        return accepted;
    }
    start(handle, operation, move || generate(length, getrandom::fill))
}

fn start(
    handle: *mut Continuation,
    operation: u64,
    generate: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
) -> u8 {
    let Some(continuation) = (unsafe { Continuation::retain_registered(handle) }) else {
        return 0;
    };
    let pending = continuation.clone();
    let scope = current_or_default();
    let work = scope.begin_cleanup();
    let address = handle as usize;
    blocking::enqueue(
        (address, operation),
        || continuation.is_cancelled(),
        move || {
            let _work = work;
            if pending.is_cancelled() {
                return;
            }
            let _guard = scope.enter();
            complete(
                &pending,
                address as *mut Continuation,
                operation,
                generate(),
            );
        },
    );
    1
}

fn generate<E: std::fmt::Display>(
    length: u64,
    fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<Vec<u8>, String> {
    if length > MAX_LENGTH {
        return Err("random: length exceeds the 1048576-byte limit".into());
    }
    let mut data = vec![0u8; length as usize];
    if !data.is_empty() {
        // On failure discard the entire buffer, including any partial output.
        fill(&mut data).map_err(|error| format!("random: OS random source failed: {error}"))?;
    }
    Ok(data)
}

fn complete(
    continuation: &Continuation,
    handle: *mut Continuation,
    operation: u64,
    result: Result<Vec<u8>, String>,
) {
    // Result(Bytes, String): tag, Ok Bytes pointer, Err String pointer + length.
    let mut words = [0usize; 4];
    let owned = match result {
        Ok(data) => {
            let pointer = jk_bytes_from_data(data.as_ptr(), data.len());
            words[1] = pointer as usize;
            pointer
        }
        Err(error) => {
            let pointer = jk_string_from_utf8(error.as_ptr(), error.len());
            words[0] = 1;
            words[2] = pointer as usize;
            words[3] = error.len();
            pointer
        }
    };
    if owned.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>());
    }
    if !continuation.complete_suspend_with_payload(
        handle,
        operation,
        words.as_ptr().cast(),
        4 * WORD,
    ) {
        jk_drop(owned);
    }
}

#[cfg(test)]
mod tests;
