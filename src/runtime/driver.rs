//! Blocking owner-thread driver for timers, I/O readiness and external wakes.
use super::{
    cps::{CpsEntry, CpsFrame, RUNTIME_ERROR},
    error,
    executor::{BlockingContext, HostExecutor, TaskGroup},
    suspension::{poll_root, Continuation, RootPoll},
};
use crate::Error;
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

    #[cfg(test)]
    fn wait(self) -> Result<(), Error> {
        BlockingContext::new()?.wait(self)
    }
}

/// Drive a scalar continuation to completion. Prefer VmEngine::prepare_async
/// so host operations capture this driver's wake/clock context on their first poll.
///
/// Blocks this thread for timers, registered I/O and retained external wake tokens.
/// Returns raw scalar result bits. Pending external host operations without a
/// supported wake source fail instead of busy polling. Consumes the task, so
/// every failure also cancels its children before returning. Manual resume and
/// host-managed scheduling remain available without using this driver.
pub fn block_on(task: Continuation) -> Result<u64, Error> {
    super::dns::initialize()?;
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor.spawn(&group, task)?;
    loop {
        let report = executor.poll(1)?;
        if let Some(completion) = report.completed.into_iter().next() {
            return completion.result;
        }
        if report.ready {
            std::thread::yield_now();
        }
        executor.wait()?;
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
    let context = match BlockingContext::new() {
        Ok(context) => context,
        Err(failure) => {
            frame.release();
            error::record_error(failure);
            return RUNTIME_ERROR;
        }
    };
    loop {
        if let Err(failure) = context.drive() {
            frame.release();
            error::record_error(failure);
            return RUNTIME_ERROR;
        }
        match context.poll(|| poll_root(entry, frame)) {
            Ok(RootPoll::Completed) => return 0,
            Ok(RootPoll::Pending) => {
                if let Err(failure) = context.wait(frame.wait_plan()) {
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
