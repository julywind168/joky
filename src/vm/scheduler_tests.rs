//! Scheduler, scope polling, and continuation lifecycle tests.

use super::support::*;
use super::VmEngine;
use crate::runtime::{
    cps::CpsFrame,
    scheduler::{Scheduler, SchedulerError},
    suspension::ResumeError,
};
use crate::{FunctionSignature, Type};

#[test]
fn nested_calls_preserve_temporaries_and_resume_without_replaying_side_effects() {
    let vm = vm(r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn add(x: Int32, y: Int32) -> Int32 { x + y }
        fn inner(x: Int32) @suspends -> Int32 {
            record(100) + delayed_value(record(x))
        }
        fn outer(x: Int32) @suspends -> Int32 {
            let y = add(record(10), inner(x))
            let y = y + delayed_value(20)
            record(y)
        }
    "#);
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("outer", &[2]).unwrap());
    assert_eq!(scheduler.suspended_count(), 1);
    assert_eq!(
        scheduler.take_completed(id).unwrap_err(),
        SchedulerError::NotCompleted(id)
    );
    assert_eq!(scheduler.continuation(id).unwrap().cps_result(), None);
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2]));
    // Polling before readiness must preserve the pending native frame.
    scheduler.resume(id).unwrap();
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2]));
    // Other work can run while the native operation is pending.
    scheduler.spawn(|| {
        record(999);
    });
    scheduler.run_all();
    READY.with(|r| r.set(true));
    scheduler.resume(id).unwrap();
    assert_eq!(scheduler.suspended_count(), 1); // second pending call
    assert_eq!(scheduler.resume_all().unwrap(), 1);
    assert_eq!(scheduler.suspended_count(), 0);
    assert_eq!(scheduler.resume_all().unwrap(), 0);
    assert_eq!(scheduler.continuation(id).unwrap().cps_result(), Some(134));
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::Resume(ResumeError::AlreadyFinished))
    );
    EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 100, 2, 2, 999, 20, 134]));
    let result = scheduler.take_completed(id).unwrap();
    assert_eq!(result.cps_result(), Some(134));
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::UnknownContinuation(id))
    );
}

#[test]
fn scope_polls_all_branches_and_waits_for_each_completion() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn left(value: Int32) @suspends { delayed_value(value) {} }
        fn right(value: Int32) @suspends { delayed_value(value) {} }
        fn all() @suspends -> Int32 {
            parallel {
                | left(1)
                | right(2)
            }
            42
        }
    "#);
    let mut task = vm.call_async("all", &[]).unwrap();
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
    assert!(!task.is_finished());
    READY.with(|ready| ready.set(true));
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(42));
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
}

#[test]
fn scope_requires_suspending_unit_function_branches() {
    let mut vm = VmEngine::new();
    for (source, message) in [
        (
            "fn f() { parallel { | f() } }",
            "parallel must be inside an @suspends function",
        ),
        (
            "fn branch() {} fn f() @suspends { parallel { | branch() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn branch() @suspends -> Int32 { 1 } fn f() @suspends { parallel { | branch() } }",
            "must be an @suspends function returning Unit",
        ),
        (
            "fn f() @suspends { parallel { } }",
            "parallel requires at least one branch",
        ),
    ] {
        assert!(
            vm.load_source(source)
                .unwrap_err()
                .message()
                .contains(message),
            "{source}"
        );
    }
}

#[test]
fn scope_failure_stops_polling_and_releases_siblings() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn fail_now() @suspends
        fn pending() @suspends { delayed_value(1) {} }
        fn failed() @suspends { fail_now() }
        fn all() @suspends { parallel { | pending() | failed() } }
    "#);
    let error = vm.call_async("all", &[]).unwrap_err();
    assert!(format!("{error:?}").contains("failure status 7"));
    EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
}

#[test]
fn nested_suspending_arguments_are_evaluated_left_to_right() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pair(x: Int32, y: Int32) @suspends -> Int32 { x * 10 + y }
        fn f() @suspends -> Int32 { pair(delayed_value(2), delayed_value(3)) }
    "#);
    let mut task = vm.call_async("f", &[]).unwrap();
    READY.with(|r| r.set(true));
    task.resume().unwrap();
    assert!(!task.is_finished());
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(34));
    EVENTS.with(|e| assert_eq!(*e.borrow(), [2, 3]));
}

#[test]
fn immediate_completion_and_repeated_suspension_have_distinct_lifecycles() {
    let vm = vm(r#"
        fn immediate(x: Int32) @suspends -> Int32 { x + 1 }
        fn unit() @suspends { }
    "#);
    let mut task = vm.call_async("immediate", &[-42]).unwrap();
    assert_eq!(task.cps_result().unwrap() as i32, -41);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    assert!(vm.call_async("unit", &[]).unwrap().is_finished());
    assert!(vm.call_int32_1("immediate", 1).is_err());
    assert!(vm.call_async("immediate", &[]).is_err());
}

#[test]
fn cancellation_drops_pending_frames_and_does_not_reuse_ids() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f() @suspends -> Int32 { delayed_value(1) }
    "#);
    let mut scheduler = Scheduler::new();
    let old = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    scheduler.cancel(old).unwrap();
    let new = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    assert_ne!(new, old);
    assert_eq!(
        scheduler.resume(old),
        Err(SchedulerError::UnknownContinuation(old))
    );
    assert_eq!(scheduler.suspended_count(), 1);
    READY.with(|r| r.set(true));
    scheduler.resume(new).unwrap();
    assert_eq!(scheduler.take_completed(new).unwrap().cps_result(), Some(2));
}

#[test]
fn failed_task_keeps_its_error_and_cannot_be_taken_as_completed() {
    use crate::runtime::suspension::ContinuationStatus;
    unsafe extern "C" fn bad_host(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 0 {
            (*frame).state = 1;
            1
        } else {
            9
        }
    }
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_suspending_fn("bad", FunctionSignature::new(vec![], Type::Unit), bad_host)
            .unwrap();
    }
    vm.load_source("extern fn bad() @suspends fn f() @suspends { bad() }")
        .unwrap();
    let mut scheduler = Scheduler::new();
    let id = scheduler.suspend(vm.call_async("f", &[]).unwrap());
    assert_eq!(
        scheduler.take_failed(id).unwrap_err(),
        SchedulerError::NotFailed(id)
    );
    assert_eq!(
        scheduler.resume_all(),
        Err(SchedulerError::Resume(ResumeError::InvalidCpsStatus(9)))
    );
    assert_eq!(scheduler.suspended_count(), 0);
    assert_eq!(
        scheduler.take_completed(id).unwrap_err(),
        SchedulerError::Resume(ResumeError::InvalidCpsStatus(9))
    );
    assert_eq!(
        scheduler.resume(id),
        Err(SchedulerError::Resume(ResumeError::AlreadyFinished))
    );
    assert_eq!(scheduler.resume_all().unwrap(), 0);
    let failed = scheduler.take_failed(id).unwrap();
    assert_eq!(
        failed.status(),
        ContinuationStatus::Failed(ResumeError::InvalidCpsStatus(9))
    );
    assert_eq!(failed.cps_result(), None);
    assert_eq!(
        scheduler.continuation(id).unwrap_err(),
        SchedulerError::UnknownContinuation(id)
    );
}
