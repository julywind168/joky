//! Frame-owned wake sources; cancellation needs no timer registry or worker thread.
use crate::runtime::{
    cps::{CpsFrame, DONE, RUNTIME_ERROR},
    driver::Wake,
    error,
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::time::Duration;

fn fail(message: &str) -> i32 {
    error::record_error(Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::InvalidState,
        message,
    ));
    RUNTIME_ERROR
}

fn complete(frame: &mut CpsFrame) -> i32 {
    frame.wake = None;
    frame.result = 0;
    frame.state = DONE;
    0
}

/// Yield for one poll without blocking the caller.
/// # Safety
/// The frame must be live and exclusively borrowed on its owning thread; no
/// parameter slots are required. Start at state zero, resume only while pending, and keep
/// the same frame alive between polls. This function never retains its pointer.
#[export_name = "joky_task_yield_now"]
pub unsafe extern "C" fn yield_now(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    match frame.state {
        0 => {
            frame.state = 1;
            frame.wake = Some(Wake::Immediate);
            1
        }
        1 => complete(frame),
        _ => fail("Invalid yield_now frame state"),
    }
}

/// Wait for UInt64 milliseconds in slot zero, using a monotonic deadline.
/// # Safety
/// The frame must be live, exclusively borrowed on its owning thread and contain
/// at least one initialized slot. Start at state zero, resume only while pending,
/// and keep the same frame alive between polls. No frame pointer is retained.
#[export_name = "joky_task_sleep_ms"]
pub unsafe extern "C" fn sleep_ms(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    sleep(
        frame,
        Duration::from_millis(frame.slots[0]),
        "sleep_ms duration exceeds the monotonic clock range",
    )
}

/// Wait for UInt64 nanoseconds in slot zero, using the executor's monotonic clock.
/// # Safety
/// The frame must satisfy the same initialization, exclusive access and polling
/// contract as `sleep_ms`. It must contain one initialized parameter slot.
#[export_name = "joky_task_sleep_ns"]
pub unsafe extern "C" fn sleep_ns(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    sleep(
        frame,
        Duration::from_nanos(frame.slots[0]),
        "sleep duration exceeds the monotonic clock range",
    )
}

/// Wait for an absolute instant using its original clock domain.
/// # Safety
/// Same frame contract as sleep_ns, with clock identity and ticks in slots 0/1.
#[export_name = "joky_task_sleep_until"]
pub unsafe extern "C" fn sleep_until(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    let deadline = match crate::runtime::executor::time_deadline(frame.slots[0], frame.slots[1]) {
        Ok(value) => value,
        Err(error) => {
            error::record_error(error);
            return RUNTIME_ERROR;
        }
    };
    if frame.state == 0 {
        frame.wake = Some(Wake::Deadline(deadline));
        frame.state = 1;
    }
    poll_sleep(frame)
}

fn sleep(frame: &mut CpsFrame, duration: Duration, range_error: &str) -> i32 {
    if frame.state == 0 {
        let Some(deadline) = crate::runtime::executor::now().checked_add(duration) else {
            return fail(range_error);
        };
        frame.wake = Some(Wake::Deadline(deadline));
        frame.state = 1;
    }
    poll_sleep(frame)
}

fn poll_sleep(frame: &mut CpsFrame) -> i32 {
    match (frame.state, frame.wake) {
        (1, Some(Wake::Deadline(deadline))) if crate::runtime::executor::now() >= deadline => {
            complete(frame)
        }
        (1, Some(Wake::Deadline(deadline))) => {
            crate::runtime::parallel::pool::wait_until(deadline);
            1
        }
        _ => fail("Invalid sleep frame state"),
    }
}

// A distinct symbol lets embedded modules use the same primitive without
// importing a host-only module into the source-module dependency graph.
#[export_name = "__joky_task_yield_now"]
pub(crate) unsafe extern "C" fn yield_internal(frame: *mut CpsFrame) -> i32 {
    yield_now(frame)
}

// Source modules use a private name for the same frame-owned timer.
#[export_name = "__joky_task_sleep_ms"]
pub(crate) unsafe extern "C" fn sleep_internal(frame: *mut CpsFrame) -> i32 {
    sleep_ms(frame)
}
