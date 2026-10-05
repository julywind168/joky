//! Blocking, single-threaded driver for cooperative yields and monotonic timers.
//! External hosts without a supported wake source still use manual resume.
use super::{
    cps::{CpsEntry, CpsFrame, RUNTIME_ERROR},
    error,
    suspension::{poll_root, Continuation, ContinuationStatus, RootPoll},
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::time::Instant;

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(crate) enum Wake {
    Immediate,
    Deadline(Instant),
}

#[derive(Default)]
pub(crate) struct WaitPlan(Option<Wake>);

impl WaitPlan {
    pub(crate) fn wake(&self) -> Option<Wake> {
        self.0
    }
    pub(crate) fn add(&mut self, wake: Option<Wake>) {
        self.0 = match (self.0, wake) {
            (Some(Wake::Immediate), _) | (_, Some(Wake::Immediate)) => Some(Wake::Immediate),
            (Some(Wake::Deadline(a)), Some(Wake::Deadline(b))) => Some(Wake::Deadline(a.min(b))),
            (a, b) => a.or(b),
        };
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.add(other.0);
    }

    fn wait(self) -> Result<(), Error> {
        match self.0 {
            Some(Wake::Immediate) => std::thread::yield_now(),
            Some(Wake::Deadline(deadline)) => {
                while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    if remaining.is_zero() {
                        break;
                    }
                    std::thread::sleep(remaining);
                }
            }
            None => {
                return Err(driver_error(
                    "Task is pending without a supported wake source; use the embedding API to resume external host operations",
                ));
            }
        }
        Ok(())
    }
}

fn driver_error(message: impl Into<String>) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}

/// Drive a continuation returned by VmEngine::call_async to completion.
///
/// Blocks this thread for timers and cooperatively yields for runnable tasks.
/// Returns raw scalar result bits. Pending external host operations without a
/// supported wake source fail instead of busy polling. Consumes the task, so
/// every failure also cancels its children before returning. Manual resume and
/// host-managed scheduling remain available without using this driver.
pub fn block_on(mut task: Continuation) -> Result<u64, Error> {
    loop {
        match task.status_ref() {
            ContinuationStatus::Completed => return Ok(task.cps_result().unwrap()),
            ContinuationStatus::Failed(error) => return Err(Error::from(error.clone())),
            ContinuationStatus::Pending => {
                task.wait_plan().wait()?;
                task.resume()?;
            }
        }
    }
}

/// Poll a fresh native root and block until completion or failure.
/// Returns zero only after DONE; failures populate the native error channel.
/// Terminal cleanup preserves the scalar result and leaves the frame allocated.
///
/// # Safety
/// The frame must be live and exclusively borrowed, with the complete slot layout
/// required by entry, initialized arguments and state zero. Keep all reachable
/// code and host data alive until return, on the frame's original thread. The
/// entry must obey the CPS ABI and must not unwind. The caller must eventually
/// free the frame, and must not resume it after this call.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_block_on(entry: CpsEntry, frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    loop {
        match poll_root(entry, frame) {
            Ok(RootPoll::Completed) => return 0,
            Ok(RootPoll::Pending) => {
                if let Err(failure) = frame.wait_plan().wait() {
                    frame.release();
                    error::record_error(failure);
                    return RUNTIME_ERROR;
                }
            }
            Err(failure) => {
                let status = failure.cps_status();
                error::record_error(failure.into());
                return status;
            }
        }
    }
}
