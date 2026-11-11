//! Native-stack admission for the ordinary macOS AArch64 ABI.
//!
//! This protects generated synchronous frames, not arbitrary host code. Host
//! callbacks must return within the reserved runtime/cleanup stack budget.
use crate::{Error, ErrorCode, ErrorPhase, SourceId, Span};
use std::cell::Cell;

/// Space kept out of generated bodies for runtime calls and iterative cleanup.
/// This is an execution contract, not a bound on arbitrary host callbacks.
pub(crate) const RESERVE: usize = 256 * 1024;

thread_local! {
    static BOUNDS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

fn bounds(sp: usize) -> Option<(usize, usize)> {
    BOUNDS.with(|cache| {
        if let Some((low, high)) = cache.get().filter(|&(low, high)| low < sp && sp <= high) {
            return Some((low, high));
        }
        // Query the execution thread, never the compilation thread. The host
        // ABI requires this pthread's actual stack; out-of-range alternate
        // stacks fail closed rather than inheriting the cached boundary.
        let (low, high) = unsafe {
            let thread = libc::pthread_self();
            let high = libc::pthread_get_stackaddr_np(thread) as usize;
            let size = libc::pthread_get_stacksize_np(thread);
            (high.checked_sub(size)?, high)
        };
        if low >= sp || sp > high {
            return None;
        }
        cache.set(Some((low, high)));
        Some((low, high))
    })
}

/// Private generated-code ABI. `bytes` includes the final callee frame and
/// bounded generated adapters. Failure returns normally through the error ABI.
#[export_name = "__joky_native_stack_room_v1"]
pub extern "C" fn room(sp: usize, bytes: usize, source: u64, start: usize, end: usize) -> i32 {
    let fits = bounds(sp).is_some_and(|(low, _)| {
        bytes
            .checked_add(RESERVE)
            .is_some_and(|need| sp - low >= need)
    });
    if !fits {
        super::error::record_error(
            Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Native stack limit exceeded",
            )
            .with_span(Span::new(start, end.max(start)).with_source(SourceId::from_raw(source))),
        );
    }
    i32::from(fits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_stack_rejects_unknown_bounds_and_overflowing_requirements() {
        for (sp, bytes) in [(0, 0), (usize::MAX, 0)] {
            let error = super::super::error::checked(|| {
                assert_eq!(room(sp, bytes, 17, 3, 9), 0);
            })
            .unwrap_err();
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
            assert_eq!(
                error.span(),
                Some(Span::new(3, 9).with_source(SourceId::from_raw(17)))
            );
        }
        let sp: usize;
        unsafe {
            std::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));
        }
        assert!(super::super::error::checked(|| room(sp, usize::MAX, 0, 0, 1)).is_err());
        assert_eq!(super::super::error::checked(|| room(sp, 0, 0, 0, 1)), Ok(1));
    }
}
