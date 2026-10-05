use joky::runtime::{cps::{CpsFrame, DONE}, executor::{Clock, HostExecutor, TaskGroup, TaskWaker, current_waker}, suspension::Continuation};
use std::{cell::{Cell, RefCell}, rc::Rc, time::{Duration, Instant}};
thread_local! {
    static TOKEN: RefCell<Option<TaskWaker>> = const { RefCell::new(None) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static POLLS: Cell<usize> = const { Cell::new(0) };
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}
#[no_mangle]
unsafe extern "C" fn wait(frame: *mut CpsFrame) -> i32 {
    POLLS.with(|n| n.set(n.get() + 1));
    TOKEN.with(|t| *t.borrow_mut() = current_waker());
    if READY.with(Cell::get) { (*frame).state = DONE; 0 } else { 1 }
}
#[no_mangle]
extern "C" fn event(n: i32) { EVENTS.with(|v| v.borrow_mut().push(n)); }
extern "C" { fn work(frame: *mut CpsFrame) -> i32; fn branches(frame: *mut CpsFrame) -> i32; }
struct ClockSource(Cell<Instant>);
impl Clock for ClockSource { fn now(&self) -> Instant { self.0.get() } }
fn main() {
    let clock = Rc::new(ClockSource(Cell::new(Instant::now())));
    let executor = HostExecutor::new(clock.clone(), 8);
    let group = TaskGroup::new();
    // Linked object and runtime share this executable's lifetime and exact ABI.
    let task = unsafe { Continuation::from_native(work, CpsFrame::new(WORK_SLOTS), Rc::new(())) };
    executor.spawn(&group, task).unwrap();
    assert_eq!(executor.poll(10).unwrap().polled, 1);
    let stale = TOKEN.with(|t| t.borrow().clone().unwrap());
    for _ in 0..20 { assert_eq!(executor.poll(10).unwrap().polled, 0); }
    assert_eq!(POLLS.with(Cell::get), 1);
    READY.with(|v| v.set(true)); stale.wake();
    executor.poll(10).unwrap();
    clock.0.set(clock.now() + Duration::from_millis(9));
    assert_eq!(executor.poll(10).unwrap().polled, 0);
    clock.0.set(clock.now() + Duration::from_millis(1));
    assert_eq!(executor.poll(10).unwrap().polled, 1);
    assert_eq!(executor.poll(10).unwrap().completed[0].result, Ok(42));
    stale.wake(); assert_eq!(executor.poll(10).unwrap().polled, 0);
    assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 2, 3]);
    EVENTS.with(|v| v.borrow_mut().clear()); READY.with(|v| v.set(false));
    let task = unsafe { Continuation::from_native(branches, CpsFrame::new(BRANCH_SLOTS), Rc::new(())) };
    executor.spawn(&group, task).unwrap(); executor.poll(10).unwrap();
    let stale = TOKEN.with(|t| t.borrow().clone().unwrap());
    clock.0.set(clock.now() + Duration::from_millis(5));
    executor.poll(10).unwrap();
    assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 5]);
    executor.cancel_group(&group); stale.wake();
    assert_eq!(executor.poll(10).unwrap().polled, 0);
    assert_eq!(executor.task_count(), 0);
}
