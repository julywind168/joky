//! P3 exercises shared by JIT, interpreter and a loaded AOT object.
use super::{InstanceLimits, InstanceStatus, InstanceValue as V, VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};

pub(crate) const SOURCE: &str = include_str!("../../tests/fixtures/persistent_containers.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn events(expected: &[i32]) {
    EVENTS.with(|events| {
        let mut actual = std::mem::take(&mut *events.borrow_mut());
        let mut expected = expected.to_vec();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    });
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
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
    vm
}
fn clean(vm: &VmEngine) {
    assert_eq!(vm.instance_bytes(), 0);
    assert_eq!(vm.instance_peak_bytes(), 0);
    LIVE.with(|n| assert_eq!(n.get(), 0));
}

pub(crate) fn exercise(vm: &VmEngine, events: impl Fn(&[i32])) {
    exercise_cycles(vm, events, 100);
}

fn exercise_cycles(vm: &VmEngine, events: impl Fn(&[i32]), cycles: i32) {
    let builder = vm.create_instance("make_builder", &[]).unwrap();
    assert_eq!(
        vm.call_instance(builder, "builder_append", &[]).unwrap(),
        V::UInt64(3)
    );
    assert_eq!(
        vm.call_instance(builder, "builder_append", &[]).unwrap(),
        V::UInt64(6)
    );
    vm.close_instance(builder).unwrap();
    clean(vm);
    let a = vm.create_instance("make_containers", &[]).unwrap();
    let b = vm.create_instance("make_containers", &[]).unwrap();
    let baseline = vm.instance_bytes();
    let allocations = LIVE.with(std::cell::Cell::get);
    for generation in 1..=cycles {
        assert_eq!(
            vm.call_instance(a, "container_fill", &[V::Int32(32)])
                .unwrap(),
            V::UInt64(32)
        );
        assert_eq!(
            vm.call_instance(a, "container_text_count", &[]).unwrap(),
            V::UInt64(32)
        );
        assert_eq!(
            vm.call_instance(a, "container_builder_length", &[])
                .unwrap(),
            V::UInt64(8)
        );
        assert_eq!(
            vm.call_instance(b, "container_count", &[]).unwrap(),
            V::UInt64(0)
        );
        assert_eq!(
            vm.call_instance(a, "container_verify", &[V::Int32(31)])
                .unwrap(),
            V::UInt64(15)
        );
        assert!(vm.instance_bytes() > baseline);
        vm.call_instance(a, "container_replace", &[V::Int32(0)])
            .unwrap();
        events(&[0]);
        assert_eq!(
            vm.call_instance(a, "container_remove", &[V::Int32(1)])
                .unwrap(),
            V::UInt64(31)
        );
        events(&[1]);
        // Reinsert through a tombstone, then replace every owner, including builder.finish().
        vm.call_instance(a, "container_fill", &[V::Int32(2)])
            .unwrap();
        events(&[1000]);
        vm.call_instance(a, "container_reset", &[]).unwrap();
        events(&(0..32).collect::<Vec<_>>());
        assert_eq!(
            vm.call_instance(a, "container_generation", &[]).unwrap(),
            V::Int32(generation)
        );
        assert_eq!(vm.instance_bytes(), baseline);
        assert_eq!(vm.instance_peak_bytes(), baseline);
        LIVE.with(|n| assert_eq!(n.get(), allocations));
    }
    vm.close_instance(a).unwrap();
    vm.close_instance(b).unwrap();
    clean(vm);
    for operation in ["container_remove", "container_clear", "close"] {
        let id = vm.create_instance("make_containers", &[]).unwrap();
        vm.call_instance(id, "container_fill", &[V::Int32(3)])
            .unwrap();
        vm.call_instance(id, "container_poison", &[]).unwrap();
        events(&[0]);
        if operation == "close" {
            assert!(vm.close_instance(id).is_err());
            assert!(vm.close_instance(id).is_err());
        } else {
            let args = if operation == "container_remove" {
                vec![V::Int32(0)]
            } else {
                vec![]
            };
            assert!(vm.call_instance(id, operation, &args).is_err());
            assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
            assert!(vm.instance_error(id).unwrap().is_some());
            assert!(vm.call_instance(id, "container_count", &[]).is_err());
            vm.close_instance(id).unwrap();
        }
        events(&[-99, 1, 2]);
        clean(vm);
    }
}

#[test]
fn persistent_instances_containers_reclaim_storage_and_finish_fallible_elements() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        exercise(&vm(mode), events);
    }
}

#[test]
fn persistent_instances_containers_pin_old_methods_and_drop_across_reload() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        let old = vm.create_instance("make_containers", &[]).unwrap();
        vm.call_instance(old, "container_fill", &[V::Int32(3)])
            .unwrap();
        assert!(vm.load_source("fn invalid() -> Int32 { false }").is_err());
        assert_eq!(
            vm.call_instance(old, "container_count", &[]).unwrap(),
            V::UInt64(3)
        );
        // Same names, different layout and destructor behavior.
        vm.load_source(
            &SOURCE
                .replace("generation: Int32", "generation: Int64")
                .replace(
                    "-> Int32 { state.generation }",
                    "-> Int64 { state.generation }",
                )
                .replace("record_drop(self.id)", "record_drop(self.id + 100)"),
        )
        .unwrap();
        let new = vm.create_instance("make_containers", &[]).unwrap();
        vm.call_instance(new, "container_fill", &[V::Int32(2)])
            .unwrap();
        vm.call_instance(old, "container_replace", &[V::Int32(0)])
            .unwrap();
        events(&[0]);
        vm.close_instance(old).unwrap();
        events(&[1000, 1, 2]);
        let report = vm.instance_cleanup_report();
        drop(vm);
        events(&[100, 101]);
        assert!(report.error().is_none());
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

pub(crate) fn transfer_failures(vm: &VmEngine, events: impl Fn(&[i32])) {
    use crate::runtime::arena::TRANSFER_FAIL;
    for fault in 0..3 {
        TRANSFER_FAIL.with(|n| n.set(Some(fault)));
        assert!(vm
            .create_instance("make_populated_containers", &[])
            .is_err());
        assert_eq!(vm.instance_count(), 0);
        clean(vm);
        events(&[0, 1, 2]);
    }
    for fault in 0..6 {
        let id = vm.create_instance("make_containers", &[]).unwrap();
        vm.call_instance(id, "container_fill", &[V::Int32(3)])
            .unwrap();
        let before = vm.instance_bytes();
        TRANSFER_FAIL.with(|n| n.set(Some(fault)));
        assert!(vm
            .call_instance(id, "container_replace", &[V::Int32(0)])
            .is_err());
        if fault < 3 {
            assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Ready);
            assert_eq!(vm.instance_bytes(), before);
            events(&[]);
            vm.close_instance(id).unwrap();
            events(&[0, 1, 2]);
        } else {
            assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
            events(&[0, 1000, 1, 2]);
            vm.close_instance(id).unwrap();
        }
        clean(vm);
    }
}
#[test]
fn persistent_instances_containers_transfer_failures_have_one_cleanup_owner() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        transfer_failures(&vm(mode), events);
    }
}

fn budget_sweep(step: usize) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        for domain in 0..3 {
            let mut complete = false;
            for limit in (1..32768).step_by(step) {
                vm.set_instance_limits(InstanceLimits::default()).unwrap();
                let id = vm.create_instance("make_containers", &[]).unwrap();
                let baseline = vm.instance_bytes();
                let limits = match domain {
                    0 => InstanceLimits {
                        max_bytes: baseline + limit,
                        ..Default::default()
                    },
                    1 => InstanceLimits {
                        max_call_bytes: limit,
                        ..Default::default()
                    },
                    _ => InstanceLimits {
                        max_peak_bytes: baseline + limit,
                        ..Default::default()
                    },
                };
                vm.set_instance_limits(limits).unwrap();
                let result = vm.call_instance(id, "container_fill", &[V::Int32(3)]);
                if result.is_ok() {
                    vm.set_instance_limits(InstanceLimits::default()).unwrap();
                    vm.call_instance(id, "container_reset", &[]).unwrap();
                    assert_eq!(vm.instance_bytes(), baseline);
                    complete = true;
                } else {
                    assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
                    assert!(vm.call_instance(id, "container_count", &[]).is_err());
                }
                vm.close_instance(id).unwrap();
                clean(&vm);
                EVENTS.with(|events| {
                    let mut dropped = std::mem::take(&mut *events.borrow_mut());
                    dropped.sort();
                    assert!(dropped.iter().all(|id| (0..3).contains(id)));
                    assert!(dropped.windows(2).all(|pair| pair[0] != pair[1]));
                    if complete {
                        assert_eq!(dropped, [0, 1, 2]);
                    }
                });
                if complete {
                    break;
                }
            }
            assert!(complete, "{mode:?} budget domain {domain}");
        }
    }
}
#[test]
fn persistent_instances_containers_budget_failure_boundaries() {
    budget_sweep(257);
}
#[test]
#[ignore = "Release per-byte persistent/call/peak budget sweep"]
fn exhaustive_persistent_instances_containers_budget_limits() {
    budget_sweep(1);
}

#[test]
#[ignore = "Independent transfer performance measurement; no timing assertions"]
fn persistent_instances_containers_transfer_measurement() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for count in [0, 8, 32, 128, 512] {
            let id = vm.create_instance("make_containers", &[]).unwrap();
            vm.call_instance(id, "container_fill", &[V::Int32(count)])
                .unwrap();
            let bytes = vm.instance_bytes();
            let allocations = LIVE.with(std::cell::Cell::get);
            let calls = 200;
            let start = std::time::Instant::now();
            for _ in 0..calls {
                std::hint::black_box(vm.call_instance(id, "container_count", &[]).unwrap());
            }
            eprintln!("{mode:?} entries={count} allocations={allocations} charged_bytes={bytes} calls={calls} ns_per_call={}", start.elapsed().as_nanos() / calls);
            assert_eq!(vm.instance_bytes(), bytes);
            LIVE.with(|n| assert_eq!(n.get(), allocations));
            vm.close_instance(id).unwrap();
            events(&(0..count).collect::<Vec<_>>());
            clean(&vm);
        }
    }
}

#[test]
fn persistent_instances_containers_reject_hidden_borrows_and_external_resources() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for source in [
            "import joky/map.{type Map} fn make() -> Map<Int32, &Int32> { Map.new::<Int32, &Int32>() }",
            "import joky/map.{type Map} import joky/array.{type Array} struct Root { values: Array<Map<Int32, Ptr>> } fn make() -> Root { Root { values: Array.new::<Map<Int32, Ptr>>() } }",
            "import joky/map.{type Map} struct Hidden<T> {} fn make() -> Map<Int32, Hidden<fn() -> Int32>> { Map.new::<Int32, Hidden<fn() -> Int32>>() }",
            "import joky/string.{type StringBuilder} struct Hidden<T> {} struct Root { builder: StringBuilder ,ghost: Hidden<&Int32> } fn make() -> Root { Root { builder: StringBuilder.new() ,ghost: Hidden::<&Int32> {} } }",
        ] {
            vm.load_source(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert!(vm.instance_function_signature("make").is_none(), "{source}");
            assert!(vm.create_instance("make", &[]).is_err());
            clean(&vm);
        }
    }
}

#[test]
#[ignore = "Release long container replacement lifecycle"]
fn exhaustive_persistent_instances_containers_lifecycle() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        exercise_cycles(&vm(mode), events, 2000);
    }
}
