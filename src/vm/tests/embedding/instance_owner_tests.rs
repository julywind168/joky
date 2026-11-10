use super::*;
use crate::runtime::arena::TRANSFER_FAIL;
use crate::{FunctionSignature, Type};

const OWNERS: &str = include_str!("../../../../tests/fixtures/persistent_owners.jk");

fn owned_vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);

    vm.load_source(OWNERS)
        .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));

    vm
}
fn clean(vm: &VmEngine) {
    assert_eq!(vm.instance_bytes(), 0);
    assert_eq!(vm.instance_peak_bytes(), 0);
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

#[test]
fn persistent_instances_own_nested_state_without_retaining_invocation_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = owned_vm(mode);
        let a = vm.create_instance("make_owner", &[]).unwrap();

        let b = vm.create_instance("make_owner", &[]).unwrap();

        let bytes = vm.instance_bytes();
        let peak = vm.instance_peak_bytes();
        let allocations = LIVE.with(std::cell::Cell::get);
        for id in 100..200 {
            assert_eq!(
                vm.call_instance(a, "advance_owner", &[V::Int32(id)])
                    .unwrap(),
                V::Int32(id - 99)
            );

            // "updated" has the same byte length as "initial".
            assert_eq!(vm.instance_bytes(), bytes);
            assert_eq!(vm.instance_peak_bytes(), peak);
            LIVE.with(|n| assert_eq!(n.get(), allocations));
        }
        assert_eq!(
            vm.call_instance(b, "owner_count", &[]).unwrap(),
            V::Int32(0)
        );
        vm.close_instance(a).unwrap();

        vm.close_instance(b).unwrap();

        clean(&vm);
        let wrapper = vm.create_instance("make_wrapper", &[]).unwrap();
        assert_eq!(
            vm.call_instance(wrapper, "wrapper_touch", &[]).unwrap(),
            V::Int32(42)
        );
        vm.close_instance(wrapper).unwrap();
        clean(&vm);
    }
}

#[test]
fn persistent_instances_transfer_failures_preserve_entry_or_destroy_exit() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for fault in 0..3 {
            let vm = owned_vm(mode);
            TRANSFER_FAIL.with(|n| n.set(Some(fault)));
            assert!(vm.create_instance("make_owner", &[]).is_err());

            assert_eq!(vm.instance_count(), 0);
            clean(&vm);
        }
        for fault in 0..6 {
            let vm = owned_vm(mode);
            let id = vm.create_instance("make_owner", &[]).unwrap();

            let before = vm.instance_bytes();
            TRANSFER_FAIL.with(|n| n.set(Some(fault)));
            assert!(vm
                .call_instance(id, "advance_owner", &[V::Int32(100)])
                .is_err());
            if fault < 3 {
                assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Ready);
                assert_eq!(vm.instance_bytes(), before);

                assert_eq!(
                    vm.call_instance(id, "owner_count", &[]).unwrap(),
                    V::Int32(0)
                );
                vm.close_instance(id).unwrap();
            } else {
                assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);

                assert!(vm.call_instance(id, "owner_count", &[]).is_err());
                vm.close_instance(id).unwrap();
            }
            clean(&vm);
        }
    }
}

#[test]
fn persistent_instances_owned_budgets_charge_once_and_refund_replaced_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = owned_vm(mode);
        let id = vm.create_instance("make_owner", &[]).unwrap();

        let baseline = vm.instance_bytes();
        assert_eq!(vm.instance_peak_bytes(), baseline);
        vm.set_instance_limits(InstanceLimits {
            max_bytes: baseline,
            ..Default::default()
        })
        .unwrap();
        // Existing storage is imported without a second persistent charge.
        vm.call_instance(id, "advance_owner", &[V::Int32(9)])
            .unwrap();

        assert_eq!(vm.instance_bytes(), baseline);
        assert!(vm.call_instance(id, "owner_grow", &[V::Int32(2)]).is_err());

        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
        clean(&vm);
        vm.close_instance(id).unwrap();
        vm.set_instance_limits(InstanceLimits::default()).unwrap();
        let id = vm.create_instance("make_owner", &[]).unwrap();

        vm.call_instance(id, "owner_grow", &[V::Int32(10)]).unwrap();
        assert!(vm.instance_bytes() > baseline);
        vm.call_instance(id, "owner_clear", &[]).unwrap();

        assert!(vm.instance_bytes() < baseline + 4096);
        vm.close_instance(id).unwrap();

        clean(&vm);
        let id = vm.create_instance("make_owner", &[]).unwrap();

        vm.set_instance_limits(InstanceLimits {
            max_peak_bytes: vm.instance_peak_bytes(),
            ..Default::default()
        })
        .unwrap();
        assert!(vm
            .call_instance(id, "advance_owner", &[V::Int32(10)])
            .is_err());

        clean(&vm);
        vm.close_instance(id).unwrap();
    }
}

#[test]
fn persistent_instances_reject_external_resources_inside_arrays() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for source in [
            "import joky/array.{type Array} struct Hidden<T> {} struct Element { ghost: Hidden<&Int32>, value: Int32 } struct Root { values: Array<Element> } fn make() -> Root { Root { values: Array.new::<Element>() } }",
            "import joky/array.{type Array} struct Element { pointer: Ptr } struct Root { values: Array<Array<Element>> } fn make() -> Root { Root { values: Array.new::<Array<Element>>() } }",
            "import joky/array.{type Array} struct Element { function: fn() -> Int32 } struct Root { values: Array<Element> } fn make() -> Root { Root { values: Array.new::<Element>() } }",
        ] {
            vm.load_source(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert!(vm.instance_function_signature("make").is_none(), "{source}");
            assert!(vm.create_instance("make", &[]).is_err());
            clean(&vm);
        }
    }
}

fn allocation_failure_sweep(modes: &[VmMode], step: usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _reset = Reset;
    for &mode in modes {
        let vm = owned_vm(mode);
        let mut complete = false;
        for limit in (0..8192).step_by(step) {
            crate::runtime::arena::LIMIT.with(|n| n.set(limit));
            let result = vm.create_instance("make_owner", &[]);
            crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
            if let Ok(id) = result {
                vm.close_instance(id).unwrap();
                complete = true;
            }

            clean(&vm);
            if complete {
                break;
            }
        }
        assert!(complete);
        complete = false;

        for limit in (0..8192).step_by(step) {
            let id = vm.create_instance("make_owner", &[]).unwrap();

            crate::runtime::arena::LIMIT.with(|n| n.set(limit));
            let result = vm.call_instance(id, "advance_owner", &[V::Int32(100)]);
            crate::runtime::arena::LIMIT.with(|n| n.set(crate::runtime::arena::MAX_BYTES));
            complete = result.is_ok();
            vm.close_instance(id).unwrap();

            clean(&vm);
            if complete {
                break;
            }
        }
        assert!(complete);
    }
}

#[test]
fn persistent_instances_owning_allocation_boundaries_release_every_initialized_value() {
    allocation_failure_sweep(&[VmMode::Jit, VmMode::Interpreter], 32);
}

#[test]
#[ignore]
fn exhaustive_persistent_instances_owning_allocation_limits() {
    allocation_failure_sweep(&[VmMode::Jit, VmMode::Interpreter], 1);
}

#[test]
fn persistent_instances_deep_arrays_transfer_all_control_blocks_and_payloads() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = owned_vm(mode);
        let id = vm.create_instance("make_tree", &[]).unwrap();
        let bytes = vm.instance_bytes();
        let allocations = LIVE.with(std::cell::Cell::get);
        for _ in 0..100 {
            assert_eq!(
                vm.call_instance(id, "tree_replace", &[]).unwrap(),
                V::UInt64(1)
            );
            assert_eq!(vm.instance_bytes(), bytes);
            LIVE.with(|n| assert_eq!(n.get(), allocations));
        }
        vm.close_instance(id).unwrap();
        clean(&vm);
    }
}

#[test]
fn persistent_instances_owning_reentry_keeps_active_storage_alive_until_close_exit() {
    use std::rc::Rc;
    type Context = (
        Rc<VmEngine>,
        super::super::InstanceId,
        super::super::InstanceId,
        bool,
    );
    thread_local! { static CONTEXT: std::cell::RefCell<Option<Context>> = const { std::cell::RefCell::new(None) }; }
    extern "C" fn callback() -> i32 {
        let (vm, active, other, close) =
            CONTEXT.with(|context| context.borrow().as_ref().unwrap().clone());
        if vm.instance_status(active).ok() != Some(InstanceStatus::Busy)
            || vm.call_instance(active, "owner_count", &[]).is_ok()
            || vm.call_instance(other, "owner_count", &[]).ok() != Some(V::Int32(0))
        {
            return -1;
        }
        if close {
            let before = vm.instance_bytes();
            if vm.close_instance(active).is_err() || vm.instance_bytes() != before {
                return -2;
            }
        }
        42
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = owned_vm(mode);
        unsafe {
            vm.register_fn(
                "callback",
                FunctionSignature::new(vec![], Type::Int32),
                callback as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&format!("{OWNERS}\nextern fn callback() -> Int32\nfn owner_invoke(state: &mut State) @noescape -> Int32 {{ let result = callback(); mem.replace(&mut state.text, \"updated\"); result }}")).unwrap();
        let vm = Rc::new(vm);
        let a = vm.create_instance("make_owner", &[]).unwrap();

        let b = vm.create_instance("make_owner", &[]).unwrap();

        CONTEXT.with(|context| *context.borrow_mut() = Some((vm.clone(), a, b, false)));
        assert_eq!(
            vm.call_instance(a, "owner_invoke", &[]).unwrap(),
            V::Int32(42)
        );

        CONTEXT.with(|context| context.borrow_mut().as_mut().unwrap().3 = true);
        assert!(vm
            .call_instance(a, "owner_invoke", &[])
            .unwrap_err()
            .message()
            .contains("closed during"));

        assert_eq!(vm.instance_count(), 1);
        vm.close_instance(b).unwrap();

        clean(&vm);
        CONTEXT.with(|context| *context.borrow_mut() = None);
    }
}

fn peak_failure_sweep(step: usize) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = owned_vm(mode);
        let mut complete = false;
        for limit in (1..8192).step_by(step) {
            vm.set_instance_limits(InstanceLimits {
                max_peak_bytes: limit,
                ..Default::default()
            })
            .unwrap();

            match vm.create_instance("make_tree", &[]) {
                Ok(id) => {
                    vm.close_instance(id).unwrap();
                    complete = true;
                }
                Err(error) => assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?} limit {limit}: {error}"
                ),
            }
            clean(&vm);
            if complete {
                break;
            }
        }
        assert!(complete);
        complete = false;
        for additional in (0..8192).step_by(step) {
            vm.set_instance_limits(InstanceLimits::default()).unwrap();
            let id = vm.create_instance("make_tree", &[]).unwrap();
            let baseline = vm.instance_peak_bytes();
            vm.set_instance_limits(InstanceLimits {
                max_peak_bytes: baseline + additional,
                ..Default::default()
            })
            .unwrap();
            match vm.call_instance(id, "tree_replace", &[]) {
                Ok(_) => complete = true,
                Err(error) => assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?} additional {additional}: {error}"
                ),
            }
            vm.close_instance(id).unwrap();
            clean(&vm);
            if complete {
                break;
            }
        }
        assert!(complete);
    }
}

#[test]
fn persistent_instances_peak_budget_includes_nested_descriptor_preparation() {
    peak_failure_sweep(32);
}

#[test]
#[ignore]
fn exhaustive_persistent_instances_peak_budget_limits() {
    peak_failure_sweep(1);
}

fn invocation_failure_sweep(step: usize) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = owned_vm(mode);
        let mut complete = false;
        for limit in (1..4096).step_by(step) {
            vm.set_instance_limits(InstanceLimits::default()).unwrap();
            let id = vm.create_instance("make_tree", &[]).unwrap();
            vm.set_instance_limits(InstanceLimits {
                max_call_bytes: limit,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(
                vm.call_instance(id, "tree_count", &[]).unwrap(),
                V::UInt64(1)
            );
            match vm.call_instance(id, "tree_replace", &[]) {
                Ok(_) => complete = true,
                Err(error) => assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?} limit {limit}: {error}"
                ),
            }
            vm.close_instance(id).unwrap();
            clean(&vm);
            if complete {
                break;
            }
        }
        assert!(complete);
    }
}

#[test]
fn persistent_instances_invocation_budget_counts_only_additional_owners_and_descriptors() {
    invocation_failure_sweep(32);
}

#[test]
#[ignore]
fn exhaustive_persistent_instances_invocation_budget_limits() {
    invocation_failure_sweep(1);
}
