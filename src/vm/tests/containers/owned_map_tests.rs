use super::{VmEngine, VmMode};
use crate::{ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../../../tests/fixtures/owned_map.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
fn configured(mode: VmMode, entry: &str) -> VmEngine {
    configured_entries(mode, &[("probe", entry)])
}
fn configured_entries<N: AsRef<str>>(mode: VmMode, entries: &[(N, &str)]) -> VmEngine {
    let source = super::support::generic_fixture_entries(SOURCE, entries);
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    assert!(registry
        .types()
        .standard
        .set(crate::core::imports::map_test_library())
        .is_ok());
    let mut vm = VmEngine::with_mode_and_registry(mode, registry);
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(&source, &crate::ModuleSources::new())
        .unwrap_or_else(|e| {
            let location = e.span().and_then(|s| {
                e.sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text())))
            });
            panic!("{mode:?}: {e} {location:?}")
        });
    vm
}
fn reset() {
    EVENTS.with(|e| e.borrow_mut().clear());
}
fn tracked() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn owned_map_returns_moves_clears_and_rebuilds_without_dropping_values() {
    // Share specializations within each group without exceeding the 256-instance limit.
    let groups: &[&[&str]] = &[
        &["transitions", "return_move"],
        &["nested_values"],
        &["arrays", "reuse_budget"],
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for &names in groups {
            let entries: Vec<_> = names
                .iter()
                .map(|name| (format!("probe_{name}"), *name))
                .collect();
            let vm = configured_entries(mode, &entries);
            for &name in names {
                reset();
                assert_eq!(
                    vm.call_int32_0(&format!("probe_{name}")).unwrap(),
                    42,
                    "{mode:?} {name}"
                );
                if name == "transitions" {
                    EVENTS.with(|e| {
                        let events = e.borrow();
                        let start = events.iter().position(|&n| n == 101).unwrap();
                        let end = events.iter().position(|&n| n == 102).unwrap();
                        assert_eq!(end, start + 1, "rehash dropped a value: {events:?}");
                        for id in 1..=5 {
                            assert!(events.contains(&-id));
                        }
                    });
                }
                tracked();
            }
        }
        {
            let (name, message, ids) = ("runtime_failure", "Map operation failure", vec![1]);
            let vm = configured(mode, name);
            reset();
            assert_eq!(
                vm.call_int32_0("probe").unwrap_err().message(),
                message,
                "{mode:?} {name}"
            );
            EVENTS.with(|e| {
                let events = e.borrow();
                assert!(!events.contains(&99));
                for id in ids {
                    assert_eq!(events.iter().filter(|&&n| n == -id).count(), 1);
                }
            });
            tracked();
        }
    }
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn owned_map_allocation_failures_preserve_each_completed_owner() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, "transitions");
        let mut complete = false;
        let mut peak_failure = false;
        for budget in (0..65536).step_by(16) {
            reset();
            crate::runtime::arena::LIMIT.with(|l| l.set(budget));
            let result = vm.call_int32_0("probe");
            crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
            match result {
                Ok(n) => {
                    assert_eq!(n, 42);
                    complete = true;
                }
                Err(e) => {
                    assert_eq!(e.code(), ErrorCode::ResourceLimit, "{mode:?}: {e}");
                    EVENTS.with(|events| {
                        let events = events.borrow();
                        peak_failure |= events.contains(&101) && !events.contains(&102);
                    });
                }
            }
            tracked();
            if complete {
                break;
            }
        }
        assert!(complete && peak_failure, "{mode:?}");
    }
}
#[test]
fn owned_map_cancellation_boundaries_retain_old_code_after_reload() {
    let mut vm = configured(VmMode::Interpreter, "transitions");
    super::support::assert_cancellation(
        &mut vm,
        "task",
        20000,
        super::support::CancellationSweep::ResourceBoundaries,
        reset,
        || EVENTS.with(|events| events.borrow().len()),
        tracked,
    );
    for finish in [false, true] {
        let mut vm = configured(VmMode::Interpreter, "transitions");
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        reset();
        let mut task = vm.call_async("task", &[]).unwrap();
        while !EVENTS.with(|e| e.borrow().contains(&102)) {
            task.resume().unwrap();
        }
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        if finish {
            while !task.is_finished() {
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        tracked();
    }
}
#[test]
fn owned_map_instruction_exhaustion_cleans_partially_committed_storage() {
    let mut vm = configured(VmMode::Interpreter, "transitions");
    let mut complete = false;
    let mut failed_with_resources = false;
    for budget in (1..20000).step_by(32) {
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        reset();
        match vm.call_int32_0("probe") {
            Ok(n) => {
                assert_eq!(n, 42);
                complete = true;
            }
            Err(e) => {
                assert_eq!(e.code(), ErrorCode::ResourceLimit);
                failed_with_resources |= EVENTS.with(|events| events.borrow().contains(&-1));
            }
        }
        tracked();
        if complete {
            break;
        }
    }
    assert!(complete && failed_with_resources);
}

#[test]
fn owned_map_rejects_copy_reads_and_reuse_after_moves() {
    let prefix = "import joky/map.{type Map} import joky/array.{type Array}";
    for (body, message) in [
        ("fn bad(m: &Map<Int32, Array<Int32>>) { m.get(0); }", "Copy"),
        ("fn bad(m: Map<Int32, Int32>) { m.release(); m.len(); }", "moved"),
        ("fn bad(m: &mut Map<Int32, Array<Int32>>) { let a = Array.new::<Int32>(); m.insert(0, a); m.insert(1, a); }", "moved"),
    ] {
        let source = format!("{prefix} {body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let e = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
            assert!(e.message().contains(message), "{e}");
        }
        let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &crate::ModuleSources::new());
        if let Ok(ast) = ast { assert!(crate::aot::AotCompiler::new("invalid-owned-map").unwrap().compile_module(&ast).unwrap_err().message().contains(message)); }
    }
}

#[test]
#[ignore = "exhaustive: cancel at every instruction; run by the stress check in Release"]
fn exhaustive_owned_map_cancels_at_each_instruction() {
    let mut vm = configured(VmMode::Interpreter, "transitions");
    super::support::assert_cancellation(
        &mut vm,
        "task",
        20000,
        super::support::CancellationSweep::EveryInstruction,
        reset,
        || EVENTS.with(|events| events.borrow().len()),
        tracked,
    );
}
