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
    for id in [1, 2, 3, 99] {
        assert_eq!(
            events.iter().filter(|&&n| n == -id).count(),
            events.iter().filter(|&&n| n == id).count(),
            "{events:?}"
        );
    }
}
