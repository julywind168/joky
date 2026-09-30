//! Versioned host operations that may retain checked object addresses while pending.
//! This is separate from the legacy CPS callback ABI.
use super::cps::{CpsEntry, CpsFrame, DONE, FAILED, INVALID_STATE, RUNTIME_ERROR};
use crate::{Error, ErrorCode, ErrorPhase};

pub const BORROWING_HOST_ABI_V1: u32 = 1;
pub const STOP_COMPLETED: u32 = 0;
pub const STOP_FAILED: u32 = 1;
pub const STOP_CANCELLED: u32 = 2;

/// Stop all access and dispose of per-call state before returning.
pub type BorrowingHostStop = unsafe extern "C" fn(*mut CpsFrame, u32);

/// Descriptor imported by generated code as `__joky_cps_borrow_v1_<symbol>`.
///
/// `poll` may retain the referenced objects, but never the frame or slot address.
/// `stop` is mandatory and runs exactly once after the first poll, on completion,
/// failure (including partial startup), or cancellation. Before it returns all
/// external access and callbacks must have stopped and per-call state must be
/// disposed. It must be synchronous, idempotent, non-unwinding, and must not
/// reenter this operation or access conflicting Joky state. Neither callback may
/// free/reallocate borrowed objects, exceed their layout, or violate permissions.
/// Callbacks run on the task's thread; no Joky frame/arena is shared with workers.
/// An external worker may access only the contracted objects until stop joins it.
///
/// The descriptor and callback code must outlive every retaining task. VM
/// registration copies the descriptor and retains its supplied owner. C/AOT
/// callers must keep their descriptor and dependencies alive until frame release.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BorrowingHostV1 {
    pub abi_version: u32,
    pub poll: Option<CpsEntry>,
    pub stop: Option<BorrowingHostStop>,
}

impl BorrowingHostV1 {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.abi_version != BORROWING_HOST_ABI_V1 || self.poll.is_none() || self.stop.is_none() {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Borrowing host ABI v1 requires poll and synchronous stop callbacks",
            ));
        }
        Ok(())
    }
}

/// Poll a v1 operation. Terminal polls stop access before returning to Joky.
/// # Safety
/// `frame` is live and exclusively borrowed for this call; `host` is a live,
/// immutable descriptor obeying its safety contract and the argument signature.
/// A frame must use the same descriptor for all polls and must not be reentered.
#[no_mangle]
pub unsafe extern "C" fn joky_cps_poll_borrowing_v1(
    frame: *mut CpsFrame,
    host: *const BorrowingHostV1,
) -> i32 {
    if matches!((*frame).state, DONE | FAILED) {
        return INVALID_STATE;
    }
    let host = &*host;
    if let Err(error) = host.validate() {
        (*frame).stop_host(STOP_FAILED);
        (*frame).state = FAILED;
        super::error::record_error(error);
        return RUNTIME_ERROR;
    }
    // Arm before entering user code: startup may fail after acquiring resources.
    if (*frame).host_stop.is_none() {
        (*frame).host_stop = host.stop;
    }
    let result = super::abi::poll_cps(host.poll.unwrap(), frame);
    let status = match result {
        Ok(1) if matches!((*frame).state, DONE | FAILED) => INVALID_STATE,
        Ok(status) => status,
        Err(error) => {
            super::error::record_error(error);
            RUNTIME_ERROR
        }
    };
    if status != 1 {
        (*frame).stop_host(if status == 0 {
            STOP_COMPLETED
        } else {
            STOP_FAILED
        });
        (*frame).state = if status == 0 { DONE } else { FAILED };
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    thread_local! { static STOPS: Cell<u32> = const { Cell::new(0) }; }

    unsafe extern "C" fn pending(frame: *mut CpsFrame) -> i32 {
        if (*frame).host_context.is_null() {
            (*frame).host_context = Box::into_raw(Box::new(42u32)).cast();
        }
        1
    }
    unsafe extern "C" fn malformed(frame: *mut CpsFrame) -> i32 {
        pending(frame);
        (*frame).state = DONE;
        1
    }
    unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
        assert!(reason == STOP_CANCELLED || reason == STOP_FAILED);
        assert_eq!(*Box::from_raw((*frame).host_context.cast::<u32>()), 42);
        STOPS.with(|n| n.set(n.get() + 1));
    }
    #[test]
    fn frame_release_stops_once_even_after_moving_a_pending_frame() {
        STOPS.with(|n| n.set(0));
        let host = BorrowingHostV1 {
            abi_version: 1,
            poll: Some(pending),
            stop: Some(stop),
        };
        let mut frame = CpsFrame::new(0);
        assert_eq!(unsafe { joky_cps_poll_borrowing_v1(&mut frame, &host) }, 1);
        let mut moved = Box::new(frame);
        assert_eq!(
            unsafe { joky_cps_poll_borrowing_v1(moved.as_mut(), &host) },
            1
        );
        moved.release();
        moved.release();
        drop(moved);
        STOPS.with(|n| assert_eq!(n.get(), 1));
    }
    #[test]
    fn malformed_pending_and_invalid_protocol_never_leave_active_operations() {
        STOPS.with(|n| n.set(0));
        let host = BorrowingHostV1 {
            abi_version: 1,
            poll: Some(malformed),
            stop: Some(stop),
        };
        let mut frame = CpsFrame::new(0);
        assert_eq!(
            unsafe { joky_cps_poll_borrowing_v1(&mut frame, &host) },
            INVALID_STATE
        );
        assert_eq!(frame.state, FAILED);
        STOPS.with(|n| assert_eq!(n.get(), 1));
        drop(frame);
        for host in [
            BorrowingHostV1 {
                abi_version: 2,
                ..host
            },
            BorrowingHostV1 { poll: None, ..host },
            BorrowingHostV1 { stop: None, ..host },
        ] {
            let mut frame = CpsFrame::new(0);
            let result = crate::runtime::error::checked(|| unsafe {
                joky_cps_poll_borrowing_v1(&mut frame, &host)
            });
            assert_eq!(result.unwrap_err().code(), ErrorCode::AbiError);
            assert_eq!(frame.state, FAILED);
            assert!(frame.host_context.is_null());
        }
        STOPS.with(|n| assert_eq!(n.get(), 1));
    }
}
