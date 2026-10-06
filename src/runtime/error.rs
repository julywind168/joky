//! Thread-local failure transport for generated native code with a C value ABI.
//!
//! Generated callers test this channel before consuming a callee's result.
//! Checked Rust boundaries isolate nested invocations and restore the outer error.
use crate::core::integer::IntegerTrap;
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::cell::RefCell;

thread_local! {
    static ERROR: RefCell<Option<Error>> = const { RefCell::new(None) };
}

struct Boundary(Option<Error>);

impl Drop for Boundary {
    fn drop(&mut self) {
        ERROR.with(|error| *error.borrow_mut() = self.0.take());
    }
}

pub(crate) fn checked<T>(call: impl FnOnce() -> T) -> Result<T, Error> {
    let _float_env = super::float_env::Guard::enter();
    let boundary = Boundary(ERROR.with(|error| error.borrow_mut().take()));
    let value = call();
    let error = ERROR.with(|error| error.borrow_mut().take());
    drop(boundary);
    match error {
        Some(error) => Err(error),
        None => Ok(value),
    }
}

/// Record the first failure. Offsets are UTF-8 bytes in the failing module.
#[export_name = "__joky_runtime_division_by_zero"]
pub extern "C" fn division_by_zero(start: usize, end: usize) {
    record_failure(
        Span::new(start, end.max(start)),
        IntegerTrap::message(IntegerTrap::DivisionByZero),
    );
}

/// Record an out-of-range integer shift without executing a masked machine shift.
#[export_name = "__joky_runtime_invalid_shift"]
pub extern "C" fn invalid_shift(start: usize, end: usize) {
    record_failure(
        Span::new(start, end.max(start)),
        IntegerTrap::message(IntegerTrap::ShiftOutOfRange),
    );
}

/// File-aware variants preserve the original two-offset C entry points.
#[export_name = "__joky_runtime_division_by_zero_in"]
pub extern "C" fn division_by_zero_in(source: u64, start: usize, end: usize) {
    record_failure(
        Span::new(start, end.max(start)).with_source(crate::SourceId::from_raw(source)),
        IntegerTrap::message(IntegerTrap::DivisionByZero),
    );
}

#[export_name = "__joky_runtime_invalid_shift_in"]
pub extern "C" fn invalid_shift_in(source: u64, start: usize, end: usize) {
    record_failure(
        Span::new(start, end.max(start)).with_source(crate::SourceId::from_raw(source)),
        IntegerTrap::message(IntegerTrap::ShiftOutOfRange),
    );
}

/// Copy a v2 message into the first-error channel independently of task storage.
/// # Safety
/// `message` must be a live immutable UTF-8 byte Buffer on the task thread.
#[export_name = "__joky_runtime_require_failed_v2"]
pub(crate) unsafe extern "C" fn require_failed_v2(
    message: *const super::buffer::Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    let span = Span::new(start, end.max(start)).with_source(crate::SourceId::from_raw(source));
    match super::owned_text::borrow(message) {
        Ok(message) => record_failure(span, message),
        Err(error) => record_error(error.with_span(span)),
    }
}

/// Report a structured failure from a synchronous or suspending Rust host callback.
/// Preserves the first error in the current thread-local invocation. Return
/// promptly afterwards; generated callers will discard the ordinary result.
/// Do not clear this channel inside a host callback.
pub fn record_error(value: Error) {
    ERROR.with(|error| {
        error.borrow_mut().get_or_insert(value);
    });
}

/// Attach a generated synchronous caller before unwinding its owned values.
#[export_name = "__joky_runtime_call_failed"]
pub extern "C" fn call_failed(source: u64, start: usize, end: usize) {
    ERROR.with(|error| {
        if let Some(error) = error.borrow_mut().as_mut() {
            error.push_native_call_site(
                Span::new(start, end.max(start)).with_source(crate::SourceId::from_raw(source)),
            );
        }
    });
}

fn record_failure(span: Span, message: &str) {
    ERROR.with(|error| {
        error.borrow_mut().get_or_insert_with(|| {
            Error::new_with_code(ErrorPhase::Runtime, ErrorCode::RuntimeError, message)
                .with_span(span)
        });
    });
}

/// Clear the native error before a new top-level C invocation.
#[no_mangle]
pub extern "C" fn joky_runtime_error_clear() {
    ERROR.with(|error| *error.borrow_mut() = None);
}

/// Zero means success; nonzero means the native result must not be consumed.
#[no_mangle]
pub extern "C" fn joky_runtime_error_code() -> i32 {
    ERROR.with(|error| i32::from(error.borrow().is_some()))
}

/// Start byte of the failing expression, or zero when no error is recorded.
#[no_mangle]
pub extern "C" fn joky_runtime_error_start() -> usize {
    ERROR.with(|error| {
        error
            .borrow()
            .as_ref()
            .and_then(Error::span)
            .map_or(0, Span::start)
    })
}

/// Exclusive end byte of the failing expression, or zero when no error is recorded.
#[no_mangle]
pub extern "C" fn joky_runtime_error_end() -> usize {
    ERROR.with(|error| {
        error
            .borrow()
            .as_ref()
            .and_then(Error::span)
            .map_or(0, Span::end)
    })
}

/// Print the native failure without clearing it. Never treats a failed value as zero.
#[no_mangle]
pub extern "C" fn joky_runtime_print_error() {
    use std::io::Write;
    ERROR.with(|error| {
        if let Some(error) = error.borrow().as_ref() {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "error: {} (bytes {}..{})",
                error,
                error.span().map_or(0, Span::start),
                error.span().map_or(0, Span::end)
            );
            for span in error.native_call_sites() {
                let _ = writeln!(
                    stderr,
                    "  called at source {} (bytes {}..{})",
                    span.source_id().raw(),
                    span.start(),
                    span.end()
                );
            }
        }
    });
}

pub(crate) fn symbols() -> [(&'static str, *const u8); 7] {
    [
        ("__joky_runtime_call_failed", call_failed as *const u8),
        (
            "__joky_runtime_require_failed_v2",
            require_failed_v2 as *const u8,
        ),
        (
            "__joky_runtime_division_by_zero_in",
            division_by_zero_in as *const u8,
        ),
        (
            "__joky_runtime_invalid_shift_in",
            invalid_shift_in as *const u8,
        ),
        ("__joky_runtime_invalid_shift", invalid_shift as *const u8),
        (
            "__joky_runtime_division_by_zero",
            division_by_zero as *const u8,
        ),
        (
            "joky_runtime_error_code",
            joky_runtime_error_code as *const u8,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_and_division_errors_keep_the_first_failure_and_restore_outer_state() {
        invalid_shift(1, 4);
        let error = checked(|| {
            division_by_zero(5, 8);
            invalid_shift(9, 12);
        })
        .unwrap_err();
        assert_eq!(error.message(), "Division by zero");
        assert_eq!(error.span(), Some(Span::new(5, 8)));
        assert_eq!(joky_runtime_error_start(), 1);
        let error = checked(|| {
            invalid_shift(13, 20);
            division_by_zero(21, 24);
        })
        .unwrap_err();
        assert!(error.message().contains("Shift count"));
        assert_eq!(error.span(), Some(Span::new(13, 20)));
        assert_eq!(joky_runtime_error_start(), 1);
        joky_runtime_error_clear();
    }

    #[test]
    fn checked_calls_isolate_nested_errors_and_restore_on_unwind() {
        division_by_zero(1, 3);
        assert_eq!(checked(|| 42), Ok(42));
        assert_eq!(joky_runtime_error_start(), 1);
        let error = checked(|| {
            division_by_zero(4, 9);
            assert_eq!(
                checked(|| division_by_zero(20, 30)).unwrap_err().span(),
                Some(Span::new(20, 30))
            );
            division_by_zero(99, 100);
        })
        .unwrap_err();
        assert_eq!(error.span(), Some(Span::new(4, 9)));
        assert_eq!(joky_runtime_error_start(), 1);
        assert!(std::panic::catch_unwind(|| checked(|| panic!("boundary test"))).is_err());
        assert_eq!(joky_runtime_error_start(), 1);
        joky_runtime_error_clear();
        assert_eq!(joky_runtime_error_code(), 0);
        assert_eq!(checked(|| 7), Ok(7));
    }

    #[test]
    fn native_errors_do_not_cross_threads() {
        division_by_zero(1, 2);
        std::thread::spawn(|| {
            assert_eq!(joky_runtime_error_code(), 0);
            division_by_zero(30, 40);
            assert_eq!(joky_runtime_error_start(), 30);
        })
        .join()
        .unwrap();
        assert_eq!(joky_runtime_error_start(), 1);
        joky_runtime_error_clear();
    }
}
