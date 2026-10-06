use joky::runtime::{
    borrowed_host::{joky_cps_poll_borrowing_v1, BorrowingHostV1, STOP_COMPLETED},
    cps::{CpsFrame, DONE},
    executor::{current_waker, HostExecutor, TaskGroup, TaskWaker},
    suspension::Continuation,
};
use std::{cell::{Cell, RefCell}, rc::Rc};
thread_local! {
    static TOKEN: RefCell<Option<TaskWaker>> = const { RefCell::new(None) };
    static STOPS: Cell<usize> = const { Cell::new(0) };
}
unsafe fn check(frame: *mut CpsFrame) {
    assert_eq!((&(*frame).slots)[0], 4);
    assert_eq!((&(*frame).slots)[1], 42);
    assert_eq!(f64::from_bits((&(*frame).slots)[2]), 0.125);
}
unsafe extern "C" fn poll(frame: *mut CpsFrame) -> i32 {
    check(frame);
    if (*frame).state == 0 {
        TOKEN.with(|token| *token.borrow_mut() = current_waker());
        (*frame).state = 1;
        1
    } else {
        (*frame).state = DONE;
        0
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    check(frame);
    assert_eq!(reason, STOP_COMPLETED);
    STOPS.with(|n| n.set(n.get() + 1));
}
#[no_mangle]
unsafe extern "C" fn wait_request(frame: *mut CpsFrame) -> i32 {
    static HOST: BorrowingHostV1 = BorrowingHostV1 {
        abi_version: 1,
        poll: Some(poll),
        stop: Some(stop),
    };
    joky_cps_poll_borrowing_v1(frame, &HOST)
}
extern "C" { fn wait_loop(frame: *mut CpsFrame) -> i32; }
fn main() {
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    // Generated object and callbacks remain linked throughout the task lifetime.
    let task = unsafe { Continuation::from_native(wait_loop, CpsFrame::new(WAIT_SLOTS), Rc::new(())) };
    executor.spawn(&group, task).unwrap();
    let mut result = None;
    for _ in 0..2001 {
        let report = executor.poll(1).unwrap();
        if let Some(done) = report.completed.into_iter().next() {
            result = Some(done.result);
            break;
        }
        TOKEN.with(|token| token.borrow().as_ref().unwrap().wake());
    }
    assert_eq!(result, Some(Ok(2000)));
    assert_eq!(STOPS.with(Cell::get), 2000);
    TOKEN.with(|token| token.borrow().as_ref().unwrap().wake());
    assert_eq!(executor.poll(1).unwrap().polled, 0);
    assert_eq!(executor.task_count(), 0);
}
