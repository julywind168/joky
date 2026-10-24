//! Local adapters for generated CPS static groups. Frames and result layouts are
//! still supplied by checked lowering; these are not the cross-thread task ABI.
use super::{CpsFrame, INVALID_STATE};
use crate::runtime::{
    error::record_error,
    task_scope::{CompletionPolicy, GroupPoll, LocalTaskGroup},
};
use crate::{Error, ErrorCode, ErrorPhase};

pub(crate) const PENDING: i64 = -1;
pub(crate) const SELECTED: i64 = -2;
const FAILED: i64 = -3;

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}

/// Publish one fully prepared static group before its first child can run.
/// # Safety
/// `frame` is a live, exclusively borrowed local parent; checked lowering has
/// initialized all `count` child frames and transferred their capture owners.
#[export_name = "__joky_cps_concurrent_open_v1"]
pub(crate) unsafe extern "C" fn open(frame: *mut CpsFrame, kind: u32, count: usize) -> i32 {
    let frame = &mut *frame;
    let prepare = || {
        if frame.concurrent.is_some()
            || count != frame.children.len()
            || frame.children.iter().any(Option::is_none)
            || count > i64::MAX as usize
        {
            return Err(invalid("Invalid prepared static task group"));
        }
        let policy = match kind {
            0 => CompletionPolicy::All,
            1 => CompletionPolicy::First,
            _ => return Err(invalid("Invalid static task group kind")),
        };
        LocalTaskGroup::new(policy, count)
    };
    match prepare() {
        Ok(group) => {
            frame.concurrent = Some(group);
            0
        }
        Err(error) => {
            record_error(error);
            INVALID_STATE
        }
    }
}

/// Select a source index, or return PENDING/SELECTED. Selection is not a cleanup
/// barrier: generated code must run ConcurrentFinish before continuing the body.
/// # Safety
/// `frame` is a live local parent with a published static group; no child is
/// executing while this helper holds its exclusive borrow.
#[export_name = "__joky_cps_concurrent_next_v1"]
pub(crate) unsafe extern "C" fn next(frame: *mut CpsFrame) -> i64 {
    let result = (*frame)
        .concurrent
        .as_mut()
        .ok_or_else(|| invalid("Missing static task group"))
        .and_then(LocalTaskGroup::next);
    match result {
        Ok(GroupPoll::Child(index)) => index as i64,
        Ok(GroupPoll::Pending) => PENDING,
        Ok(GroupPoll::Selected) => SELECTED,
        Err(error) => {
            record_error(error);
            FAILED
        }
    }
}

/// Record the selected child's CPS Complete/Pending status after result owners
/// have been registered. Failures take the caller's existing unwind path.
/// # Safety
/// `frame` is live and exclusive on its local task thread. `index` is the last
/// index returned by next; the child has returned and no longer accesses slots.
#[export_name = "__joky_cps_concurrent_polled_v1"]
pub(crate) unsafe extern "C" fn polled(frame: *mut CpsFrame, index: usize, status: i32) -> i32 {
    let result = (*frame)
        .concurrent
        .as_mut()
        .ok_or_else(|| invalid("Missing static task group"))
        .and_then(|group| match status {
            0 | 1 => group.polled(index, status == 0),
            _ => Err(invalid("Invalid task child poll report")),
        });
    match result {
        Ok(()) => 0,
        Err(error) => {
            record_error(error);
            INVALID_STATE
        }
    }
}
