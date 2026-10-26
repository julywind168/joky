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
