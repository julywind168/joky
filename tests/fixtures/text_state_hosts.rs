use joky::runtime::{cps::CpsFrame, executor::{HostExecutor, TaskGroup}, suspension::Continuation, string};
use std::{cell::Cell, rc::Rc};
thread_local! { static CALLS: Cell<i32> = const { Cell::new(0) }; }
#[no_mangle]
unsafe extern "C" fn text_call(name: *const u8) {
    assert_eq!(string::borrow(name).unwrap(), "中🙂");
    CALLS.with(|n| n.set(n.get() + 1));
}
#[no_mangle]
unsafe extern "C" fn text_read(name: *const u8) -> f64 { text_call(name); 0.375 }
#[no_mangle]
unsafe extern "C" fn text_write_int(name: *const u8, value: i32) { text_call(name); assert_eq!(value, -42); }
#[no_mangle]
unsafe extern "C" fn text_write_float(name: *const u8, value: f64) { text_call(name); assert_eq!(value.to_bits(), (-0.125f64).to_bits()); }
extern "C" { fn check_text_hosts() -> i32; fn check_text_hosts_async(frame: *mut CpsFrame) -> i32; }
fn main() {
    assert_eq!(unsafe { check_text_hosts() }, 42);
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    // Object code and callbacks stay linked for the entire executable lifetime.
    let task = unsafe { Continuation::from_native(check_text_hosts_async, CpsFrame::new(ASYNC_SLOTS), Rc::new(())) };
    executor.spawn(&group, task).unwrap();
    executor.poll(8).unwrap();
    assert_eq!(executor.poll(8).unwrap().completed[0].result, Ok(42));
    assert_eq!(CALLS.with(Cell::get), 8);
}
