//! Per-run environment snapshots, exposed through cancellable provider calls.
//!
//! Before admission we validate layouts and retain the immutable name (get).
//! Queued jobs own that share, the scope and continuation; cancellation drops
//! these captures. Results own one managed root. Completion transfers that
//! root only on acceptance; rejected/late completion drops it here.

use super::blocking;
use super::continuation::Continuation;
use super::managed::{jk_drop, jk_dup, valid_header, RuntimeValueKind};
use super::provider::ProviderScope;
use super::scope::{current_or_default, EnvSnapshot};
use super::string::jk_string_from_utf8;
use std::ffi::OsStr;
use std::path::PathBuf;

const WORD: usize = std::mem::size_of::<usize>();

fn hooks() -> &'static [(&'static str, super::provider::ProviderStart)] {
    &[
        ("get", get_start),
        ("vars", vars_start),
        ("args", args_start),
        ("current_dir", current_dir_start),
        ("temp_dir", temp_dir_start),
    ]
}

pub(crate) fn register_operations(
    entries: &[super::provider::ProviderOperationEntry],
) -> Option<ProviderScope> {
    super::provider::register_named(hooks(), cancel, entries)
}

unsafe extern "C" fn cancel(handle: *mut Continuation, operation: u64) -> u8 {
    blocking::cancel((handle as usize, operation));
    1
}

// Immutable managed Strings can be retained across blocking workers. The
// integer address is Send; this descriptor owns exactly one managed share.
struct SharedName(usize, usize);
impl Drop for SharedName {
    fn drop(&mut self) {
        jk_drop(self.0 as *mut u8);
    }
}
impl SharedName {
    fn as_str(&self) -> Result<&str, String> {
        let bytes = unsafe { std::slice::from_raw_parts(self.0 as *const u8, self.1) };
        std::str::from_utf8(bytes).map_err(|_| "environment name is not valid UTF-8".into())
    }
}

unsafe extern "C" fn get_start(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    _result: *mut u8,
    result_size: usize,
) -> u8 {
    if arguments.is_null() || size != 2 * WORD || result_size != 6 * WORD {
        return 0;
    }
    let pointer = std::ptr::read_unaligned(arguments.cast::<*mut u8>());
    let length = std::ptr::read_unaligned(arguments.add(WORD).cast::<usize>());
    let Some(header) = valid_header(pointer) else {
        return 0;
    };
    if header.kind != RuntimeValueKind::String as u8 || header.payload_size != length {
        return 0;
    }
    if let Some(accepted) = dispatch_handler(handle, operation, arguments, size, result_size) {
        return accepted;
    }
    let pointer = jk_dup(pointer);
    if pointer.is_null() {
        return 0;
    }
    let name = SharedName(pointer as usize, length);
    start(handle, operation, move |snapshot| {
        lookup(snapshot, name.as_str()?)
    })
}

fn lookup(snapshot: &EnvSnapshot, name: &str) -> Result<Option<String>, String> {
    if name.is_empty() || name.contains(['=', '\0']) {
        return Err("invalid environment variable name".into());
    }
    snapshot
        .vars
        .iter()
        .find(|(key, _)| names_equal(key, OsStr::new(name)))
        .map(|(_, value)| {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "environment value is not valid UTF-8".into())
        })
        .transpose()
}

#[cfg(not(windows))]
pub(super) fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

#[cfg(windows)]
pub(super) fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn CompareStringOrdinal(
            a: *const u16,
            a_len: i32,
            b: *const u16,
            b_len: i32,
            ignore_case: i32,
        ) -> i32;
    }
    let left: Vec<_> = left.encode_wide().collect();
    let right: Vec<_> = right.encode_wide().collect();
    let (Ok(a_len), Ok(b_len)) = (i32::try_from(left.len()), i32::try_from(right.len())) else {
        return false;
    };
    unsafe { CompareStringOrdinal(left.as_ptr(), a_len, right.as_ptr(), b_len, 1) == 2 }
}

macro_rules! no_arg_start {
    ($name:ident, $words:literal, $body:expr) => {
        unsafe extern "C" fn $name(
            h: *mut Continuation,
            op: u64,
            _a: *const u8,
            size: usize,
            _r: *mut u8,
            result_size: usize,
        ) -> u8 {
            if size != 0 || result_size != $words * WORD {
                return 0;
            }
            if let Some(accepted) = dispatch_handler(h, op, _a, size, result_size) {
                return accepted;
            }
            start(h, op, $body)
        }
    };
}
no_arg_start!(vars_start, 4, make_vars);
no_arg_start!(args_start, 4, make_args);
no_arg_start!(current_dir_start, 5, |_| std::env::current_dir()
    .map_err(|e| e.to_string())
    .and_then(path_string));
no_arg_start!(temp_dir_start, 5, |_| path_string(std::env::temp_dir()));

unsafe fn dispatch_handler(
    h: *mut Continuation,
    op: u64,
    arguments: *const u8,
    size: usize,
    result_size: usize,
) -> Option<u8> {
    super::provider::dispatch_handler(
        h,
        op,
        arguments,
        size,
        result_size,
        if size == 0 { &[] } else { &[0] },
    )
}

fn start<T: Payload>(
    h: *mut Continuation,
    op: u64,
    f: impl FnOnce(&EnvSnapshot) -> Result<T, String> + Send + 'static,
) -> u8 {
    let Some(c) = (unsafe { Continuation::retain_registered(h) }) else {
        return 0;
    };
    let pending = c.clone();
    let scope = current_or_default();
    let address = h as usize;
    blocking::enqueue(
        (address, op),
        || c.is_cancelled(),
        move || {
            if pending.is_cancelled() {
                return;
            }
            // Managed results must be charged to the requesting program, including
            // when a late result is rejected after that program was cancelled.
            let _guard = scope.enter();
            complete(
                &pending,
                address as *mut Continuation,
                op,
                f(&scope.env_snapshot),
            );
        },
    );
    1
}

trait Payload {
    const WORDS: usize;
    /// Writes the Ok fields and returns the single owned managed root.
    fn write(self, words: &mut [usize; 6]) -> *mut u8;
}
impl Payload for String {
    const WORDS: usize = 5;
    fn write(self, words: &mut [usize; 6]) -> *mut u8 {
        let pointer = make_string(&self);
        words[1] = pointer as usize;
        words[2] = self.len();
        pointer
    }
}
impl Payload for Option<String> {
    const WORDS: usize = 6;
    fn write(self, words: &mut [usize; 6]) -> *mut u8 {
        match self {
            Some(value) => {
                let pointer = make_string(&value);
                words[2] = pointer as usize;
                words[3] = value.len();
                pointer
            }
            None => {
                words[1] = 1;
                std::ptr::null_mut()
            }
        }
    }
}
struct Collection(*mut u8);
impl Payload for Collection {
    const WORDS: usize = 4;
    fn write(self, words: &mut [usize; 6]) -> *mut u8 {
        words[1] = self.0 as usize;
        self.0
    }
}

fn complete<T: Payload>(
    c: &Continuation,
    h: *mut Continuation,
    op: u64,
    result: Result<T, String>,
) {
    let mut words = [0usize; 6];
    let owned = match result {
        Ok(value) => value.write(&mut words),
        Err(error) => {
            let pointer = make_string(&error);
            words[0] = 1;
            words[T::WORDS - 2] = pointer as usize;
            words[T::WORDS - 1] = error.len();
            pointer
        }
    };
    if !c.complete_suspend_with_payload(h, op, words.as_ptr().cast(), T::WORDS * WORD) {
        jk_drop(owned);
    }
}

fn make_string(value: &str) -> *mut u8 {
    let pointer = jk_string_from_utf8(value.as_ptr(), value.len());
    if pointer.is_null() {
        // As with Rust String allocation, OOM must terminate rather than leave
        // an accepted provider request suspended forever without a result.
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>());
    }
    pointer
}
fn path_string(path: PathBuf) -> Result<String, String> {
    path.into_os_string()
        .into_string()
        .map_err(|_| "path is not valid UTF-8".into())
}

fn make_args(snapshot: &EnvSnapshot) -> Result<Collection, String> {
    // Validate every input before allocating managed nodes. A bad item cannot
    // leak an already constructed prefix/tail.
    let items: Vec<&str> = snapshot
        .args
        .iter()
        .map(|item| {
            item.to_str()
                .ok_or_else(|| "argument is not valid UTF-8".to_owned())
        })
        .collect::<Result<_, _>>()?;
    let mut tail = std::ptr::null_mut();
    for item in items.into_iter().rev() {
        let words = [make_string(item) as u64, item.len() as u64];
        // Cons consumes both the element and tail, including allocation failure.
        tail = super::list::jk_list_cons(words.as_ptr(), 2, &1, 1, tail);
        if tail.is_null() {
            return Err("could not allocate argument list".into());
        }
    }
    Ok(Collection(tail))
}

fn make_vars(snapshot: &EnvSnapshot) -> Result<Collection, String> {
    let items: Vec<(&str, &str)> = snapshot
        .vars
        .iter()
        .map(|(key, value)| {
            Ok((
                key.to_str()
                    .ok_or_else(|| "environment name is not valid UTF-8".to_owned())?,
                value
                    .to_str()
                    .ok_or_else(|| "environment value is not valid UTF-8".to_owned())?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let mut map = std::ptr::null_mut();
    for (key, value) in items {
        let words = [
            make_string(key) as u64,
            key.len() as u64,
            make_string(value) as u64,
            value.len() as u64,
        ];
        // Legacy String comparison interoperates with generated String key
        // descriptors; insert consumes the old map and both new strings.
        map = super::map::jk_map_insert(map, words.as_ptr(), 4, &5, 1, 2, 1);
        if map.is_null() {
            return Err("could not allocate environment snapshot".into());
        }
    }
    Ok(Collection(map))
}

#[cfg(test)]
mod tests;
