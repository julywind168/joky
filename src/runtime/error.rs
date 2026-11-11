//! Thread-local failure transport for generated native code with a C value ABI.
//!
//! Generated callers test this channel before consuming a callee's result.
//! Checked Rust boundaries isolate nested invocations and restore the outer error.
use crate::core::integer::IntegerTrap;
use crate::{Error, ErrorCode, ErrorPhase, Span};
use std::cell::RefCell;

thread_local! {
    static ERROR: RefCell<Option<Failure>> = const { RefCell::new(None) };
}

// The emergency variant stays inline until a checked Rust boundary or an
// explicit diagnostic query. Generated failure propagation only reads/writes
// these fixed fields; it never grows an error message or call-site vector.
#[derive(Clone)]
enum Failure {
    Reported(Error),
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Deferred(DeferredNative),
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy)]
struct DeferredNative {
    target_rejected: bool,
    span: Span,
    caller: Option<Span>,
    omitted: usize,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl DeferredNative {
    fn materialize(&self) -> Error {
        let mut error = Error::new_with_code(
            crate::ErrorPhase::Runtime,
            if self.target_rejected {
                ErrorCode::InvalidCall
            } else {
                ErrorCode::ResourceLimit
            },
            if self.target_rejected {
                "Native indirect target is outside strict stack admission"
            } else {
                "Native stack limit exceeded"
            },
        )
        .with_span(self.span);
        if let Some(caller) = self.caller {
            error.push_native_call_site(caller);
        }
        if self.omitted != 0 {
            error = error.with_help(format!(
                "{} further native call sites omitted during native failure propagation",
                self.omitted,
            ));
        }
        error
    }
}

impl Failure {
    fn code(&self) -> crate::ErrorCode {
        match self {
            Self::Reported(error) => error.code(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            Self::Deferred(error) => {
                if error.target_rejected {
                    ErrorCode::InvalidCall
                } else {
                    ErrorCode::ResourceLimit
                }
            }
        }
    }
    fn span(&self) -> Option<Span> {
        match self {
            Self::Reported(error) => error.span(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            Self::Deferred(error) => Some(error.span),
        }
    }
    fn into_error(self) -> Error {
        match self {
            Self::Reported(error) => error,
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            Self::Deferred(error) => error.materialize(),
        }
    }
}

struct Boundary(Option<Failure>);

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
        Some(error) => Err(error.into_error()),
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
#[export_name = "__joky_runtime_assert_failed_v2"]
pub(crate) unsafe extern "C" fn assert_failed_v2(
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

/// Preserve the selected failure while a task drains its descendants.
/// This explicit diagnostic read may allocate; synchronous emergency cleanup
/// uses the primitive queries instead. CPS draining has a separate stack budget.
pub(crate) fn current_error() -> Option<Error> {
    ERROR.with(|error| error.borrow().clone().map(Failure::into_error))
}

/// Report a structured failure from a synchronous or suspending Rust host callback.
/// Preserves the first error in the current thread-local invocation. Return
/// promptly afterwards; generated callers will discard the ordinary result.
/// Do not clear this channel inside a host callback.
pub fn record_error(value: Error) {
    record(Failure::Reported(value));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn record_stack_limit(span: Span) {
    record(Failure::Deferred(DeferredNative {
        target_rejected: false,
        span,
        caller: None,
        omitted: 0,
    }));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn record_unadmitted_target(span: Span) {
    record(Failure::Deferred(DeferredNative {
        target_rejected: true,
        span,
        caller: None,
        omitted: 0,
    }));
}

fn record(value: Failure) {
    ERROR.with(|error| {
        let mut error = error.borrow_mut();
        let previous = error.as_ref().map(Failure::code);
        let replace = if super::isolate::active() {
            super::isolate::replaces_failure(previous, value.code())
        } else {
            previous.is_none()
                || previous == Some(ErrorCode::Cancelled) && value.code() != ErrorCode::Cancelled
        };
        if replace {
            *error = Some(value);
        }
    });
}

/// Attach a generated synchronous caller before unwinding its owned values.
#[export_name = "__joky_runtime_call_failed"]
pub extern "C" fn call_failed(source: u64, start: usize, end: usize) {
    let span = Span::new(start, end.max(start)).with_source(crate::SourceId::from_raw(source));
    ERROR.with(|error| match error.borrow_mut().as_mut() {
        Some(Failure::Reported(error)) => error.push_native_call_site(span),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Some(Failure::Deferred(error)) => {
            if error.caller.is_none() {
                error.caller = Some(span);
            } else {
                error.omitted = error.omitted.saturating_add(1);
            }
        }
        None => {}
    });
}

fn record_failure(span: Span, message: &str) {
    record_error(
        Error::new_with_code(ErrorPhase::Runtime, ErrorCode::RuntimeError, message).with_span(span),
    );
}

/// Clear the native error before a new top-level C invocation.
#[no_mangle]
pub extern "C" fn joky_runtime_error_clear() {
    ERROR.with(|error| *error.borrow_mut() = None);
}

/// Compiler checkpoints run only where ownership registrations are consistent.
#[export_name = "__joky_task_checkpoint_v1"]
pub extern "C" fn checkpoint() {
    if let Err(error) = super::parallel::pool::check_cancelled() {
        record_error(error);
    }
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
            .and_then(Failure::span)
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
            .and_then(Failure::span)
            .map_or(0, Span::end)
    })
}

/// Print the native failure without clearing it. Never treats a failed value as zero.
#[no_mangle]
pub extern "C" fn joky_runtime_print_error() {
    fn print(error: &Error) {
        use std::io::Write;
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
        for help in error.help() {
            let _ = writeln!(stderr, "  note: {help}");
        }
    }
    ERROR.with(|error| match error.borrow().as_ref() {
        Some(Failure::Reported(error)) => print(error),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Some(Failure::Deferred(error)) => print(&error.materialize()),
        None => {}
    });
}

pub(crate) fn symbols() -> impl IntoIterator<Item = (&'static str, *const u8)> {
    [
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        (
            "__joky_native_stack_room_v1",
            super::native_stack::room as *const u8,
        ),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        (
            "__joky_native_stack_target_v1",
            super::native_stack::target as *const u8,
        ),
        ("__joky_runtime_call_failed", call_failed as *const u8),
        (
            "__joky_runtime_assert_failed_v2",
            assert_failed_v2 as *const u8,
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
        ("__joky_task_checkpoint_v1", checkpoint as *const u8),
        (
            "joky_runtime_error_code",
            joky_runtime_error_code as *const u8,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn stack_limit_defers_bounded_diagnostics_and_restores_nested_state() {
        let outer = Span::new(1, 3);
        record_stack_limit(outer);
        let failure = checked(|| {
            record_stack_limit(Span::new(10, 20));
            assert_eq!(checked(|| 42), Ok(42));
            assert!(std::panic::catch_unwind(|| checked(|| panic!("boundary test"))).is_err());
            for site in 0..10_000 {
                call_failed(7, site, site + 1);
            }
            assert_eq!(joky_runtime_error_code(), 1);
            assert_eq!(
                (joky_runtime_error_start(), joky_runtime_error_end()),
                (10, 20)
            );
            // Neither nested boundaries nor primitive queries materialize it.
            ERROR.with(|error| assert!(matches!(*error.borrow(), Some(Failure::Deferred(_)))));
        })
        .unwrap_err();
        assert_eq!(failure.code(), ErrorCode::ResourceLimit);
        assert_eq!(failure.span(), Some(Span::new(10, 20)));
        assert_eq!(
            failure.native_call_sites(),
            &[Span::new(0, 1).with_source(crate::SourceId::from_raw(7))]
        );
        assert!(failure.help()[0].contains("9999"));
        ERROR.with(|error| match error.borrow().as_ref().unwrap() {
            Failure::Deferred(error) => assert_eq!(error.span, outer),
            Failure::Reported(_) => panic!("outer failure was materialized"),
        });
        joky_runtime_error_clear();
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn stack_limit_preserves_first_error_and_isolate_fatal_priority() {
        let reported = |code| Error::new_with_code(ErrorPhase::Runtime, code, "reported");
        for isolated in [false, true] {
            let _guard = isolated.then(super::super::isolate::Guard::enter);
            for first in [
                ErrorCode::RuntimeError,
                ErrorCode::Cancelled,
                ErrorCode::AbiError,
            ] {
                let failure = checked(|| {
                    record_error(reported(first));
                    record_stack_limit(Span::new(3, 9));
                })
                .unwrap_err();
                assert_eq!(
                    failure.code(),
                    if first == ErrorCode::Cancelled {
                        ErrorCode::ResourceLimit
                    } else {
                        first
                    }
                );
            }
            for next in [
                ErrorCode::RuntimeError,
                ErrorCode::Cancelled,
                ErrorCode::AbiError,
            ] {
                let failure = checked(|| {
                    record_stack_limit(Span::new(3, 9));
                    record_error(reported(next));
                })
                .unwrap_err();
                assert_eq!(
                    failure.code(),
                    if isolated && next != ErrorCode::RuntimeError {
                        next
                    } else {
                        ErrorCode::ResourceLimit
                    }
                );
            }
        }
    }

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
