//! Shared harness for the VM test files: the effect-recording host bindings,
//! the parity engine builders, and the `@suspends` fixture rewrite.
use super::{VmEngine, VmMode};
pub(super) use crate::parser::test_support::{with_suspends, Suspends};
use crate::runtime::cps::CpsFrame;
use crate::{FunctionSignature, Type};
use std::cell::{Cell, RefCell};

thread_local! {
    pub(super) static READY: Cell<bool> = const { Cell::new(false) };
    pub(super) static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

pub(super) unsafe extern "C" fn delayed_value(frame: *mut CpsFrame) -> i32 {
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

pub(super) unsafe extern "C" fn fail_now(_frame: *mut CpsFrame) -> i32 {
    7
}

pub(super) extern "C" fn record(value: i32) -> i32 {
    EVENTS.with(|events| events.borrow_mut().push(value));
    value
}

pub(super) fn parity_vm(source: &str, mode: VmMode) -> VmEngine {
    parity_vm_with_prelude(source, mode, true)
}

pub(super) fn parity_vm_with_prelude(source: &str, mode: VmMode, prelude: bool) -> VmEngine {
    READY.with(|ready| ready.set(false));
    EVENTS.with(|events| events.borrow_mut().clear());
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(prelude);
    unsafe {
        vm.register_suspending_fn(
            "delayed_value",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            delayed_value,
        )
        .unwrap();
        vm.register_fn(
            "record",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            record as *const u8,
        )
        .unwrap();
        vm.register_suspending_fn(
            "fail_now",
            FunctionSignature::new(vec![], Type::Unit),
            fail_now,
        )
        .unwrap();
    }
    vm.load_source(source).unwrap();
    vm
}

pub(super) fn vm(source: &str) -> VmEngine {
    parity_vm(source, VmMode::Jit)
}
