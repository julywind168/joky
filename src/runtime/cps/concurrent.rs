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
            frame.concurrent_results = (0..count).map(|_| None).collect();
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
/// barrier: generated code must drain before taking results, then run
/// ConcurrentFinish before continuing the body.
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

/// Stop all children before allowing result delivery. Generated code has
/// exclusive local execution ownership; no metadata borrow spans a stop hook.
/// # Safety
/// `frame` is a live local parent whose selected child has returned. Child
/// callbacks and their borrowed resources remain live through this call.
#[export_name = "__joky_cps_concurrent_drain_v1"]
pub(crate) unsafe extern "C" fn drain(frame: *mut CpsFrame) -> i32 {
    let begin = (*frame)
        .concurrent
        .as_mut()
        .ok_or_else(|| invalid("Missing static task group"))
        .and_then(LocalTaskGroup::begin_drain);
    let outcome = match begin {
        Ok(false) => Ok(()),
        Ok(true) => {
            let children = std::mem::take(&mut (*frame).children);
            let outcome = crate::runtime::error::checked(|| drop(children));
            (*frame)
                .concurrent
                .as_mut()
                .expect("draining static group")
                .finish_drain(outcome)
        }
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            record_error(error);
            INVALID_STATE
        }
    }
}

/// Allocate or access one independent result packet before polling the child.
/// A packet uses the local frame's slots and cleanup machinery but never runs code.
/// # Safety
/// `frame` is exclusively borrowed; checked lowering supplies a fixed layout per
/// source index. No pointer into a packet may outlive the group's cleanup barrier.
#[export_name = "__joky_cps_concurrent_result_v1"]
pub(crate) unsafe extern "C" fn result(
    frame: *mut CpsFrame,
    index: usize,
    slots: usize,
) -> *mut CpsFrame {
    let frame = &mut *frame;
    let Some(packet) = frame.concurrent_results.get_mut(index) else {
        record_error(invalid("Unknown task result packet"));
        return std::ptr::null_mut();
    };
    if packet
        .as_ref()
        .is_some_and(|packet| packet.slots.len() != slots)
    {
        record_error(invalid("Task result packet layout changed"));
        return std::ptr::null_mut();
    }
    packet
        .get_or_insert_with(|| {
            let mut packet = CpsFrame::new(slots);
            packet.arena = frame.arena.clone();
            Box::new(packet)
        })
        .as_mut()
}

/// Claim one packet after successful drain for result copying and owner transfer.
/// # Safety
/// The caller exclusively owns the parent, copies only the checked result layout,
/// then transfers cleanup ownership before allowing cancellation or callbacks.
#[export_name = "__joky_cps_concurrent_take_v1"]
pub(crate) unsafe extern "C" fn take(frame: *mut CpsFrame, index: usize) -> *mut CpsFrame {
    let frame = &mut *frame;
    if frame
        .concurrent_results
        .get(index)
        .and_then(Option::as_ref)
        .is_none()
    {
        record_error(invalid("Missing task result packet"));
        return std::ptr::null_mut();
    }
    let result = frame
        .concurrent
        .as_mut()
        .ok_or_else(|| invalid("Missing static task group"))
        .and_then(|group| group.take_result(index));
    if let Err(error) = result {
        record_error(error);
        return std::ptr::null_mut();
    }
    match frame
        .concurrent_results
        .get_mut(index)
        .and_then(Option::as_deref_mut)
    {
        Some(packet) => packet,
        None => {
            record_error(invalid("Missing task result packet"));
            std::ptr::null_mut()
        }
    }
}

/// Select a drained, unclaimed result in completion order; -1 means all were claimed.
/// # Safety
/// `frame` is a live, exclusive local parent whose static group has selected.
#[export_name = "__joky_cps_concurrent_next_result_v1"]
pub(crate) unsafe extern "C" fn next_result(frame: *mut CpsFrame) -> i64 {
    match (*frame)
        .concurrent
        .as_mut()
        .ok_or_else(|| invalid("Missing static task group"))
        .and_then(LocalTaskGroup::next_result)
    {
        Ok(Some(index)) => index as i64,
        Ok(None) => -1,
        Err(error) => {
            record_error(error);
            FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_result_drain_rejects_delivery_release_and_reentry_during_stop() {
        use std::cell::{Cell, RefCell};
        thread_local! {
            static ROOT: Cell<*mut CpsFrame> = const { Cell::new(std::ptr::null_mut()) };
            static OBSERVED: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn first(frame: *mut CpsFrame) -> i32 {
            if (*frame).state == 0 {
                (*frame).state = 1;
                1
            } else {
                (*frame).result = 40;
                0
            }
        }
        unsafe extern "C" fn stop(_: *mut CpsFrame, _: u32) {
            let root = ROOT.with(Cell::get);
            let take_rejected = crate::runtime::error::checked(|| take(root, 0)).is_err();
            let drain_rejected = crate::runtime::error::checked(|| drain(root)).is_err();
            let release_rejected =
                crate::runtime::error::checked(|| super::super::release_children(root)).is_err();
            OBSERVED.with(|observed| {
                observed
                    .borrow_mut()
                    .extend([take_rejected, drain_rejected, release_rejected])
            });
        }
        unsafe extern "C" fn pending(frame: *mut CpsFrame) -> i32 {
            (*frame).host_stop = Some(stop);
            1
        }
        let ast = crate::parser::parse(
            r#"
            extern fn first() @suspends -> Int32
            extern fn pending() @suspends -> Int32
            fn root() @suspends -> Int32 {
                let value = race { | first() | pending() }
                value + 2
            }
        "#,
        )
        .unwrap();
        let module = crate::backends::jit::compile_with_registry(
            &ast,
            &[
                ("first", first as *const u8),
                ("pending", pending as *const u8),
            ],
            &Default::default(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let function = &module.exports["root"];
        let poll: super::super::CpsEntry = unsafe { std::mem::transmute(function.entry) };
        let mut frame = CpsFrame::new(function.info.frame_slots);
        ROOT.with(|root| root.set(&mut frame));
        assert_eq!(unsafe { poll(&mut frame) }, 1);
        assert_eq!(unsafe { poll(&mut frame) }, 0);
        ROOT.with(|root| root.set(std::ptr::null_mut()));
        assert_eq!(frame.result, 42);
        OBSERVED
            .with(|observed| assert_eq!(std::mem::take(&mut *observed.borrow_mut()), [true; 3]));
        assert!(frame.concurrent.is_none());
    }

    #[test]
    fn task_result_drain_keeps_first_cleanup_error_and_never_delivers() {
        use crate::runtime::cleanup::{Entry, Hook};
        unsafe extern "C" fn fail(data: *mut u8, _: *mut crate::runtime::arena::Arena) {
            record_error(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                if data.is_null() {
                    "First cleanup failure"
                } else {
                    "Second cleanup failure"
                },
            ));
        }
        let mut frame = CpsFrame::new(0);
        unsafe {
            for index in 0..3 {
                let child = super::super::child_at(&mut frame, index, 0);
                if index != 0 {
                    (*super::super::cleanup(child)).entries.insert(
                        0,
                        Entry {
                            keep_alive: None,
                            storage_size: 0,
                            storage: None,
                            owners: vec![None],
                            hooks: vec![(
                                0,
                                Hook::Native {
                                    data: if index == 1 {
                                        std::ptr::null_mut()
                                    } else {
                                        std::ptr::dangling_mut()
                                    },
                                    callback: fail,
                                },
                            )],
                        },
                    );
                }
            }
            assert_eq!(open(&mut frame, 1, 3), 0);
            result(&mut frame, 0, 1);
            assert_eq!(next(&mut frame), 0);
            assert_eq!(polled(&mut frame, 0, 0), 0);
            for attempt in 0..2 {
                let error = crate::runtime::error::checked(|| drain(&mut frame)).unwrap_err();
                assert_eq!(
                    error.message(),
                    "First cleanup failure",
                    "attempt {attempt}"
                );
                assert!(frame.children.is_empty());
                assert!(frame.concurrent.as_ref().unwrap().drain_finished());
            }
            let error = crate::runtime::error::checked(|| take(&mut frame, 0)).unwrap_err();
            assert_eq!(error.message(), "First cleanup failure");
            crate::runtime::error::checked(|| super::super::release_children(&mut frame)).unwrap();
        }
        assert!(frame.concurrent.is_none());
        assert!(frame.concurrent_results.is_empty());
    }

    #[test]
    fn task_result_packets_cleanup_active_before_published() {
        use crate::runtime::cleanup::{Entry, Hook};
        thread_local! { static DROPS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) }; }
        unsafe extern "C" fn drop_value(data: *mut u8, _: *mut crate::runtime::arena::Arena) {
            DROPS.with(|drops| drops.borrow_mut().push(data.cast::<u64>().read()));
        }
        let mut frame = CpsFrame::new(0);
        unsafe {
            for index in 0..2 {
                super::super::child_at(&mut frame, index, 0);
            }
            assert_eq!(open(&mut frame, 0, 2), 0);
            for index in 0..2 {
                let packet = result(&mut frame, index, 1);
                (&mut (*packet).slots)[0] = index as u64 + 1;
                let data = (*packet).slots.as_mut_ptr().cast();
                (*super::super::cleanup(packet)).entries.insert(
                    0,
                    Entry {
                        keep_alive: None,
                        storage_size: 0,
                        storage: None,
                        owners: vec![None],
                        hooks: vec![(
                            0,
                            Hook::Native {
                                data,
                                callback: drop_value,
                            },
                        )],
                    },
                );
                assert_eq!(next(&mut frame), index as i64);
                if index == 0 {
                    assert_eq!(polled(&mut frame, index, 0), 0);
                }
            }
        }
        // The active packet has acquired owners but failed before completion publication.
        frame.release();
        DROPS.with(|drops| assert_eq!(std::mem::take(&mut *drops.borrow_mut()), [2, 1]));
    }

    #[test]
    fn task_result_packet_survives_child_storage_reuse_until_join() {
        unsafe extern "C" fn wait(frame: *mut CpsFrame) -> i32 {
            if (*frame).state == 0 {
                (*frame).state = 1;
                1
            } else {
                (*frame).result = 2;
                0
            }
        }
        let ast = crate::parser::parse(
            r#"
            extern fn wait() @suspends -> Int32
            fn ready() @suspends -> Int32 { 40 }
            fn root() @suspends -> Int32 {
                let (a, b) = join { | ready() | wait() }
                a + b
            }
        "#,
        )
        .unwrap();
        let module = crate::backends::jit::compile_with_registry(
            &ast,
            &[("wait", wait as *const u8)],
            &Default::default(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let function = &module.exports["root"];
        let poll: super::super::CpsEntry = unsafe { std::mem::transmute(function.entry) };
        let mut frame = CpsFrame::new(function.info.frame_slots);
        assert_eq!(unsafe { poll(&mut frame) }, 1);
        assert_eq!(frame.concurrent_results[0].as_ref().unwrap().slots[0], 40);
        let early = crate::runtime::error::checked(|| unsafe { take(&mut frame, 0) });
        assert!(early.is_err());
        frame.children[0].as_mut().unwrap().result = 999;
        frame.children[0].as_mut().unwrap().slots.fill(999);
        assert_eq!(unsafe { poll(&mut frame) }, 0);
        assert_eq!(frame.result, 42);
        assert!(frame.concurrent_results.is_empty());
        assert!(crate::runtime::error::checked(|| unsafe { take(&mut frame, 0) }).is_err());
    }
}
