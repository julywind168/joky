use super::engine::VmEngine;
use crate::runtime::{
    cps::CpsFrame,
    scheduler::{Priority, Scheduler, SchedulerError, Task},
    suspension::ResumeError,
};
use std::cell::{Cell, RefCell};

thread_local! {
    static READY: Cell<bool> = const { Cell::new(false) };
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn delayed_value(frame: *mut CpsFrame) -> i32 {
    let frame = &mut *frame;
    if frame.state == 0 {
        EVENTS.with(|events| events.borrow_mut().push(frame.slots[0] as i32));
        frame.host_data = (frame.slots[0] as i32 + 1).to_ne_bytes().to_vec();
        frame.state = 1;
        return 1;
    }
    if !READY.with(Cell::get) {
        return 1;
    }
    frame.result = i32::from_ne_bytes(frame.host_data[..].try_into().unwrap()) as u32 as u64;
    0
}

extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

fn vm(source: &str) -> VmEngine {
    READY.with(|r| r.set(false));
    EVENTS.with(|e| e.borrow_mut().clear());
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_suspending_fn("delayed_value", delayed_value);
    }
    vm.register_fn("record", record as *const u8);
    vm.load_source(source).unwrap();
    vm
}

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
    scheduler.spawn(Task {
        entry: Box::new(|| {
            record(999);
        }),
        priority: Priority::Normal,
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
        Err(SchedulerError::Resume(ResumeError::AlreadyResumed))
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
fn nested_suspending_arguments_are_evaluated_left_to_right() {
    let vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pair(x: Int32, y: Int32) @suspends -> Int32 { x * 10 + y }
        fn f() @suspends -> Int32 { pair(delayed_value(2), delayed_value(3)) }
    "#);
    let mut task = vm.call_async("f", &[]).unwrap();
    READY.with(|r| r.set(true));
    task.resume().unwrap();
    assert!(!task.is_resumed());
    task.resume().unwrap();
    assert_eq!(task.cps_result(), Some(34));
    EVENTS.with(|e| assert_eq!(*e.borrow(), [2, 3]));
}

#[test]
fn continuations_keep_their_module_alive_after_reload_and_vm_drop() {
    let mut vm = vm(r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 10 }
        fn removed() -> Int32 { 99 }
    "#);
    let mut old = vm.call_async("f", &[1]).unwrap();
    vm.reload_source(
        r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn f(x: Int32) @suspends -> Int32 { delayed_value(x) + 100 }
    "#,
    )
    .unwrap();
    assert!(vm.call_int32_0("removed").is_err());
    let mut new = vm.call_async("f", &[1]).unwrap();
    assert_ne!(old.module_version, new.module_version);
    drop(vm);
    READY.with(|r| r.set(true));
    old.resume().unwrap();
    new.resume().unwrap();
    assert_eq!(old.cps_result(), Some(12));
    assert_eq!(new.cps_result(), Some(102));
}

#[test]
fn failed_load_preserves_previous_code_and_signatures() {
    let mut vm = vm("fn f(x: Int32) -> Int32 { x + 1 }");
    assert!(vm.load_source("fn f() -> Bool { 1 }").is_err());
    assert_eq!(vm.call_int32_1("f", 41).unwrap(), 42);
    assert!(vm.call_int32_0("f").is_err());
    assert!(vm.call_unit_2("f", 1, 2).is_err());
    assert!(vm.call_async("f", &[1]).is_err());
}

#[test]
fn immediate_completion_and_repeated_suspension_have_distinct_lifecycles() {
    let vm = vm(r#"
        fn immediate(x: Int32) @suspends -> Int32 { x + 1 }
        fn unit() @suspends { }
    "#);
    let mut task = vm.call_async("immediate", &[-42]).unwrap();
    assert_eq!(task.cps_result().unwrap() as i32, -41);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyResumed));
    assert!(vm.call_async("unit", &[]).unwrap().is_resumed());
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
fn native_abi_mismatch_and_missing_suspends_are_compile_errors() {
    let mut vm = VmEngine::new();
    vm.register_fn("host", record as *const u8);
    assert!(vm
        .load_source("extern fn host(x: Int32) @suspends -> Int32")
        .unwrap_err()
        .contains("ABI"));
    unsafe {
        vm.register_suspending_fn("host", delayed_value);
    }
    assert!(vm
        .load_source("extern fn host(x: Int32) -> Int32")
        .unwrap_err()
        .contains("ABI"));
    assert!(vm
        .load_source(
            r#"
        extern fn host(x: Int32) @suspends -> Int32
        fn f() -> Int32 { host(1) }
    "#
        )
        .unwrap_err()
        .contains("@suspends"));
    assert!(vm
        .load_source("extern fn missing()")
        .unwrap_err()
        .contains("not registered"));
}

#[test]
fn invalid_host_status_propagates_without_replaying_the_call() {
    unsafe extern "C" fn bad_host(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state == 0 {
            frame.state = 1;
            1
        } else {
            7
        }
    }
    let mut vm = VmEngine::new();
    unsafe {
        vm.register_suspending_fn("bad_host", bad_host);
    }
    vm.load_source("extern fn bad_host() @suspends fn f() @suspends { bad_host() }")
        .unwrap();
    let mut task = vm.call_async("f", &[]).unwrap();
    assert_eq!(task.resume(), Err(ResumeError::InvalidCpsStatus(7)));
    assert_eq!(task.cps_result(), None);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyResumed));
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
        vm.register_suspending_fn("bad", bad_host);
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
        Err(SchedulerError::Resume(ResumeError::AlreadyResumed))
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
