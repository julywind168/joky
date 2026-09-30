//! Frame-owned wake sources; cancellation needs no timer registry or worker thread.
use crate::runtime::{
    cps::{CpsFrame, DONE, RUNTIME_ERROR},
    driver::Wake,
    error,
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::time::{Duration, Instant};

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
    if frame.state == 0 {
        let Some(deadline) = Instant::now().checked_add(Duration::from_millis(frame.slots[0]))
        else {
            return fail("sleep_ms duration exceeds the monotonic clock range");
        };
        frame.wake = Some(Wake::Deadline(deadline));
        frame.state = 1;
    }
    match (frame.state, frame.wake) {
        (1, Some(Wake::Deadline(deadline))) if Instant::now() >= deadline => complete(frame),
        (1, Some(Wake::Deadline(_))) => 1,
        _ => fail("Invalid sleep_ms frame state"),
    }
}
