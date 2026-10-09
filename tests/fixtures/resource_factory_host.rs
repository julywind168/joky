use joky::runtime::resource::ResourceFactoryV1;
use std::{
    cell::RefCell,
    ffi::c_void,
    net::{Ipv4Addr, TcpListener},
};
thread_local! {
    static EVENTS: RefCell<Vec<i32>>=const {RefCell::new(Vec::new())};
}
struct Socket {
    id: i32,
    listener: TcpListener,
}
unsafe extern "C" fn create(id: u64, output: *mut *mut c_void) -> i32 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    *output = Box::into_raw(Box::new(Socket {
        id: id as i32,
        listener,
    }))
    .cast();
    EVENTS.with(|e| e.borrow_mut().push(-(id as i32)));
    if id == 99 {
        7
    } else {
        0
    }
}
unsafe extern "C" fn release(token: *mut c_void) {
    let Socket { id, listener } = *Box::from_raw(token.cast::<Socket>());
    drop(listener);
    EVENTS.with(|e| e.borrow_mut().push(id));
}
#[export_name = "__joky_resource_factory_v1_make"]
pub static FACTORY: ResourceFactoryV1 = ResourceFactoryV1 {
    abi_version: 1,
    create: Some(create),
    release: Some(release),
};
pub fn reset() {
    EVENTS.with(|e| e.borrow_mut().clear());
}
pub fn events() -> Vec<i32> {
    EVENTS.with(|e| e.borrow().clone())
}
pub fn check() {
    let events = events();
    for id in 1..=99 {
        assert_eq!(
            events.iter().filter(|&&n| n == -id).count(),
            events.iter().filter(|&&n| n == id).count(),
            "{events:?}"
        );
    }
}

use joky::runtime::{cps::CpsFrame, resource::ResourceFactoryCpsV1};
struct Operation {
    token: *mut c_void,
    observed: *mut c_void,
}
thread_local! { static STOPS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) }; }
pub fn reset_stops() {
    STOPS.with(|s| s.borrow_mut().clear());
}
pub fn stops() -> Vec<u32> {
    STOPS.with(|s| s.borrow().clone())
}
unsafe extern "C" fn poll_resource(frame: *mut CpsFrame, output: *mut *mut c_void) -> i32 {
    let id = (&(*frame).slots)[0];
    if (*frame).host_context.is_null() {
        let mut token = std::ptr::null_mut();
        assert_eq!(create(id, &mut token), 0);
        (*frame).host_context = Box::into_raw(Box::new(Operation {
            token,
            observed: token,
        }))
        .cast();
        return if id == 2 { 7 } else { 1 };
    }
    if id == 7 {
        return 1;
    }
    if id == 5 {
        return 0;
    }
    let operation = &mut *(*frame).host_context.cast::<Operation>();
    *output = std::mem::replace(&mut operation.token, std::ptr::null_mut());
    match id {
        3 => 7,
        4 => 1,
        6 => {
            unsafe extern "C" {
                fn __joky_runtime_division_by_zero(start: usize, end: usize);
            }
            __joky_runtime_division_by_zero(0, 1);
            0
        }
        _ => 0,
    }
}
unsafe extern "C" fn stop_resource(frame: *mut CpsFrame, reason: u32) {
    let operation = Box::from_raw((*frame).host_context.cast::<Operation>());
    // Access must still be valid even on malformed output or failed poll.
    assert!((*operation.observed.cast::<Socket>())
        .listener
        .local_addr()
        .is_ok());
    if !operation.token.is_null() {
        release(operation.token);
    }
    STOPS.with(|s| s.borrow_mut().push(reason));
    (*frame).host_context = std::ptr::null_mut();
}
#[export_name = "__joky_resource_factory_cps_v1_acquire"]
pub static ASYNC_FACTORY: ResourceFactoryCpsV1 = ResourceFactoryCpsV1 {
    abi_version: 1,
    poll: Some(poll_resource),
    stop: Some(stop_resource),
    release: Some(release),
};
