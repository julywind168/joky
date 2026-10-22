use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::{ErrorCode, FunctionSignature, Type};
use std::cell::{Cell, RefCell};

thread_local! {
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    static EXHAUST_ON_READY: Cell<bool> = const { Cell::new(false) };
}
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
    if value == -10001 && EXHAUST_ON_READY.with(Cell::get) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(0));
    }
}
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
    }
    vm.load_source(include_str!(
        "../../../../tests/fixtures/recursive_owner_cleanup.jk"
    ))
    .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn assert_cleaned() {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=5 {
            let drops = events.iter().filter(|&&v| v == id).count();
            let created = events.iter().filter(|&&v| v == -id).count();
            // Cancellation can land between construction and its host marker.
            assert!(
                drops <= 1 && drops >= created,
                "lost or duplicate owner: {events:?}"
            );
        }
    });
    crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}
struct RestoreBudget;
impl Drop for RestoreBudget {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        EXHAUST_ON_READY.with(|flag| flag.set(false));
    }
}
#[test]
fn recursive_owners_mutate_drop_and_continue_after_hook_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        reset();
        assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
        assert_cleaned();
        reset();
        assert!(vm.call_int32_0("failure").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -2, -99, 99, 2, 1]));
        assert_cleaned();
    }
}
#[test]
fn recursive_owners_release_deep_trees_without_stack_or_workspace_allocation() {
    let _restore = RestoreBudget;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        reset();
        EXHAUST_ON_READY.with(|flag| flag.set(true));
        assert_eq!(vm.call_int32_0("deep").unwrap(), 42);
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        EXHAUST_ON_READY.with(|flag| flag.set(false));
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.len(), 20001);
            assert_eq!(&events[10001..], &(1..=10000).rev().collect::<Vec<_>>());
        });
        assert_cleaned();
    }
}
#[test]
fn recursive_owners_allocation_failure_cleans_partial_trees() {
    let _restore = RestoreBudget;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let mut completed = false;
        let mut partial = false;
        for budget in (0..65536).step_by(32) {
            reset();
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("probe");
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit);
                    partial |= EVENTS.with(|events| events.borrow().contains(&-1));
                }
            }
            assert_cleaned();
            if completed {
                break;
            }
        }
        assert!(completed && partial);
    }
}
fn cancellation(sweep: CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "work",
        30000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().len()),
        assert_cleaned,
    );
}
#[test]
fn recursive_owners_cancellation_covers_transfers_and_borrows() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel recursive owners at each interpreter instruction"]
fn exhaustive_recursive_owners_cancel_at_each_instruction() {
    cancellation(CancellationSweep::EveryInstruction);
}
#[test]
fn recursive_owners_reload_keeps_old_descriptors_and_drop_code_alive() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = vm(mode);
            reset();
            let mut task = vm.call_async("work", &[]).unwrap();
            while !EVENTS.with(|events| events.borrow().contains(&-100)) {
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            assert_cleaned();
        }
    }
}

#[test]
fn recursive_owners_persistent_instances_validate_the_complete_storage_graph() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        reset();
        let id = vm.create_instance("make_state", &[]).unwrap();
        assert_eq!(
            vm.call_instance(id, "state_count", &[]).unwrap(),
            super::InstanceValue::Int32(42)
        );
        vm.call_instance(id, "state_clear", &[]).unwrap();
        vm.close_instance(id).unwrap();
        assert_cleaned();
        vm.load_source("import joky/array.{type Array} struct Hidden { value: Ptr } struct State { values: Array<Hidden> } fn make() -> State { State { values: Array.new::<Hidden>() } }").unwrap();
        assert!(vm.create_instance("make", &[]).is_err());
        vm.load_source("import joky/array.{type Array} struct Hidden { value: Int32 } struct State { values: Array<Hidden> } fn make() -> State { State { values: Array.new::<Hidden>() } }").unwrap();
        let id = vm.create_instance("make", &[]).unwrap();
        vm.close_instance(id).unwrap();
        assert_cleaned();
    }
}

#[test]
fn recursive_owners_cancel_deep_pending_trees_after_vm_destruction() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        reset();
        let task = vm.call_async("hold_deep", &[]).unwrap();
        assert!(!task.is_finished());
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.len(), 1001, "tree was released before suspension");
            assert_eq!(events.last(), Some(&-10001));
        });
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        drop(task);
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.len(), 2001);
            assert_eq!(&events[1001..], &(1..=1000).rev().collect::<Vec<_>>());
        });
        assert_cleaned();
    }
}
