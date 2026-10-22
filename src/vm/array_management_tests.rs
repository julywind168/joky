use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record_drop(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
const SOURCE: &str = include_str!("../../tests/fixtures/array_management.jk");
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "management_library",
            include_str!("../../tests/fixtures/array_management_library.jk"),
        )
        .unwrap();
    modules
}
#[test]
fn array_management_transfers_ownership_in_both_execution_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record_drop",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                record_drop as *const u8,
            )
            .unwrap();
        }
        vm.load_source_with_modules(SOURCE, &modules())
            .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span()));
        for (name, events) in [
            ("manage_copy", vec![]),
            ("manage_owned", vec![-1, -2, -3, 1, 2, 3, -4, 4]),
            ("manage_enum_tuple", vec![-1, -2, 1, 2]),
            ("manage_wrapper", vec![]),
            ("manage_zero_and_aggregate", vec![]),
            ("manage_references", vec![]),
            ("manage_clear_failure", vec![3, 2, 1]),
            ("manage_truncate_failure", vec![3, 2, 1]),
            ("manage_budget", vec![]),
            ("manage_evaluation", vec![10, 20, 30]),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if name.ends_with("failure") || name.ends_with("bounds") {
                assert_eq!(result.unwrap_err().message(), "Division by zero");
            } else {
                assert_eq!(result.unwrap(), 42, "{mode:?} {name}");
            }
            EVENTS.with(|e| assert_eq!(*e.borrow(), events, "{mode:?} {name}"));
            LIVE.with(|n| assert_eq!(n.get(), 0, "{mode:?} {name}"));
        }
    }
}

fn tracked() {
    EVENTS.with(|e| {
        let events = e.borrow();
        for id in 1..=4 {
            let drops = events.iter().filter(|&&n| n == id).count();
            assert!(drops <= 1, "duplicate drop: {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "lost owner: {events:?}");
            }
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_drop",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record_drop as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(SOURCE, &modules()).unwrap();
    vm
}
#[test]
fn array_management_preserves_cleanup_at_every_instruction_boundary() {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "manage_task",
        2000,
        super::support::CancellationSweep::EveryInstruction,
        || EVENTS.with(|e| e.borrow_mut().clear()),
        || EVENTS.with(|e| e.borrow().len()),
        tracked,
    );
    let mut complete = false;
    for budget in 1..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("manage_transitions") {
            Ok(n) => {
                assert_eq!(n, 42);
                complete = true;
            }
            Err(e) => assert_eq!(e.code(), crate::ErrorCode::ResourceLimit),
        }
        tracked();
        if complete {
            break;
        }
    }
    assert!(complete);
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn array_management_preserves_cleanup_under_allocation_failure_and_reload() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut complete = false;
        for budget in (0..4096).step_by(8) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|l| l.set(budget));
            match vm.call_int32_0("manage_transitions") {
                Ok(n) => {
                    assert_eq!(n, 42);
                    complete = true;
                }
                Err(e) => assert_eq!(e.code(), crate::ErrorCode::ResourceLimit),
            }
            tracked();
            if complete {
                break;
            }
        }
        assert!(complete);
        crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
    }
    for finish in [false, true] {
        let mut vm = configured(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("manage_task", &[]).unwrap();
        while !EVENTS.with(|e| e.borrow().contains(&-90)) {
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
fn array_management_rejects_conflicting_loans_and_invalid_capabilities() {
    let prefix = "import joky/array.{type Array}\n        import joky/element as element import joky/slice as slices\n        struct Resource { id: Int32, values: Array<Int32> }";
    for operation in ["values.swap(0, 1)", "values.truncate(1)", "values.clear()"] {
        for body in [
            format!("fn bad(values: &Array<Resource>) {{ {operation}; }}"),
            format!("fn bad(values: &mut Array<Resource>) {{ let item = values.borrow_ref(0); {operation}; item.id; }}"),
            format!("fn bad(values: &mut Array<Resource>) {{ let item = values.borrow_mut_ref(0); {operation}; item.id = 1; }}"),
            format!("fn bad(values: &mut Array<Resource>) {{ let region = values.slice(0, 1); {operation}; slices.borrow_ref(region, 0).id; }}"),
            format!("fn bad(values: &mut Array<Resource>) {{ let region = values.slice_mut(0, 1); let child = slices.slice_mut(region, 0, 1); {operation}; slices.len(child); }}"),
            format!("fn bad(values: Array<Resource>) {{ values.release(); {operation}; }}"),
            format!("fn bad(values: &mut Array<&Int32>) @suspends {{ {operation}; }}"),
            format!("fn bad(values: &mut Array<Int32>) {{ let item = values.borrow(0); {operation}; element.read(item); }}"),
        ] {
            reject(&format!("{prefix} {body}"));
        }
    }
    for body in [
        "fn bad(values: &mut Array<Resource>) { values.swap(-1, 0); }",
        "fn bad(values: &mut Array<Resource>) { values.truncate(-1); }",
        "fn bad(values: &mut Array<Int32>) { values.swap(0, { values.clear(); 0 }); }",
        "fn bad(values: &mut Array<&Int32>) { let p = alloc(1); values.push((&*p)); values.clear() ; (*(p) = 2); }",
        "fn bad(values: &mut Array<&Int32>) { let p = alloc(1); values.push((&*p)); values.truncate(0) ; (*(p) = 2); }",
    ] {
        reject(&format!("{prefix} {body}"));
    }
}
fn reject(source: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert!(
            VmEngine::with_mode(mode).load_source(source).is_err(),
            "{mode:?}: {source}"
        );
    }
    let ast = crate::TypeRegistry::default().resolve_with_modules(
        &crate::parser::parse(source).unwrap(),
        &crate::ModuleSources::new(),
    );
    if let Ok(ast) = ast {
        assert!(
            crate::aot::AotCompiler::new("invalid-array-management")
                .unwrap()
                .compile_module(&ast)
                .is_err(),
            "{source}"
        );
    }
}
