//! Failure categories and local execution boundary shared by both engines.
use crate::{Error, ErrorCode};
use std::cell::Cell;
thread_local! { static DEPTH: Cell<usize> = const { Cell::new(0) }; }

pub(crate) struct Guard;
impl Guard {
    pub(crate) fn enter() -> Self {
        DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}
pub(crate) fn active() -> bool {
    DEPTH.with(|depth| depth.get() != 0)
}

// Stable language categories, independent of the Rust enum's discriminants.
// Cancellation and ABI violations must never become recoverable business values.
pub(crate) fn code(error: &Error) -> Option<u64> {
    Some(match error.code() {
        ErrorCode::Cancelled | ErrorCode::AbiError => return None,
        ErrorCode::RuntimeError | ErrorCode::Panic => 1,
        ErrorCode::ResourceLimit => 2,
        ErrorCode::HostError => 3,
        ErrorCode::IoError => 4,
        ErrorCode::InvalidState => 5,
        ErrorCode::InvalidArgument => 6,
        _ => 8,
    })
}

/// Preserve the first recoverable error, but never let it hide an ABI violation
/// or cancellation reported by a later cleanup in the same isolated subtree.
pub(crate) fn retain_failure(failure: &mut Option<Error>, error: Error) {
    if replaces_failure(failure.as_ref().map(Error::code), error.code()) {
        *failure = Some(error);
    }
}

// Shared with the native channel so an emergency error can be compared by
// category without materializing a heap-backed Error during cleanup.
pub(crate) fn replaces_failure(previous: Option<ErrorCode>, next: ErrorCode) -> bool {
    let fatal = |code| matches!(code, ErrorCode::Cancelled | ErrorCode::AbiError);
    previous.is_none()
        || previous == Some(ErrorCode::Cancelled) && next != ErrorCode::Cancelled
        || fatal(next) && previous.is_some_and(|old| !fatal(old))
}
