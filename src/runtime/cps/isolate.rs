//! The generated isolate child stays local so borrowed captures never cross workers.
use super::{protocol, task, CpsFrame};
use crate::runtime::{error, isolate};

/// # Safety
/// Both frames are live and exclusive; child belongs to parent's isolate group.
#[export_name = "__joky_isolate_poll_v1"]
pub(crate) unsafe extern "C" fn poll(parent: *mut CpsFrame, child: *mut CpsFrame) -> i32 {
    let _guard = isolate::Guard::enter();
    match error::checked(|| {
        let status = task::poll(child);
        if status == 1 {
            // The outer driver's task round cannot cross this failure boundary.
            // Advance the isolated tree here while its error channel is active.
            let round = super::task_round(child);
            if round != 0 {
                return round;
            }
        }
        status
    }) {
        Ok(status @ (0 | 1)) => status,
        // Legacy CPS hosts may report failure only through their return status.
        Ok(_) => {
            (*parent).isolated_failure = 3;
            2
        }
        Err(failure) => match isolate::code(&failure) {
            Some(code) => {
                (*parent).isolated_failure = code;
                2
            }
            None => {
                error::record_error(failure);
                super::RUNTIME_ERROR
            }
        },
    }
}

/// Drain before recovery. No generated continuation sees a partial task tree.
/// # Safety
/// Parent owns the current, single-child isolate scope and no child is running.
#[export_name = "__joky_isolate_drain_v1"]
pub(crate) unsafe extern "C" fn drain(parent: *mut CpsFrame) -> u64 {
    let _guard = isolate::Guard::enter();
    let failed = (*parent).isolated_failure != 0;
    if failed {
        (*parent).concurrent.as_mut().unwrap().stop(None);
    }
    protocol::context(parent);
    let scope = (*parent).task_protocol.last().unwrap().id;
    if let Err(failure) = error::checked(|| protocol::join(scope)) {
        match isolate::code(&failure) {
            Some(code) => {
                if (*parent).isolated_failure == 0 {
                    (*parent).isolated_failure = code;
                }
            }
            None => error::record_error(failure),
        }
    }
    if (*parent).isolated_failure != 0 {
        let scope = (*parent).task_protocol.last_mut().unwrap();
        scope.closed = true;
        scope.drained = true;
        scope.terminal = Some(0);
        scope.failure = None;
    }
    (*parent).isolated_failure
}
