use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record_drop(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
const SOURCE: &str = include_str!("../../tests/fixtures/owning_slices.jk");
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "slice_library",
            include_str!("../../tests/fixtures/slice_library.jk"),
        )
        .unwrap();
    modules
}
#[test]
fn owning_slices_transfer_ownership_in_both_execution_modes() {
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
            ("slice_shared", vec![-1, -2, -3, -4, 4, 3, 2, 1]),
            ("slice_mutable", vec![-1, -2, -3, -4, -5, 2, 5, 13, 4, 1]),
            ("slice_wrapper", vec![]),
            ("slice_enum", vec![-1, 42]),
            ("slice_tuple", vec![-1, -2, -3, -4, 2, 1, 3, 4]),
            ("slice_empty", vec![-1, 1]),
            ("slice_bounds", vec![-1, 1]),
            ("slice_sub_bounds", vec![-1, 1]),
            ("slice_drop_failure", vec![1, 2, 3]),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if name.ends_with("failure") || name.ends_with("bounds") {
                assert!(result.is_err());
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
fn owning_slices_preserve_cleanup_at_every_instruction_boundary() {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "slice_task",
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
        match vm.call_int32_0("slice_transitions") {
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
fn owning_slices_preserve_cleanup_under_allocation_failure_and_reload() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut complete = false;
        for budget in (0..4096).step_by(8) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|l| l.set(budget));
            match vm.call_int32_0("slice_transitions") {
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
        let mut task = vm.call_async("slice_task", &[]).unwrap();
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
fn owning_slices_reject_conflicting_loans_and_escapes() {
    let prefix = "import joky/mem\nimport joky/array.{type Array}\n        import joky/slice as slices.{type Slice}\n        struct Resource { id: Int32, values: Array<Int32> }";
    for body in [
        "fn bad(values: &mut Array<Resource>) { let region = values.slice(0, 1); values.pop(); slices.borrow_ref(region, 0).id; }",
        "fn bad(values: &mut Array<Resource>) { let region = values.slice_mut(0, 1); values.push(Resource { id: 0, values: Array.new::<Int32>() }); slices.len(region); }",
        "fn bad(values: &mut Array<Resource>) { let region = values.slice_mut(0, 1); let a = slices.borrow_mut_ref(region, 0); let b = slices.borrow_mut_ref(region, 0); a.id = b.id; }",
        "fn bad(values: &mut Array<Resource>) { let region = values.slice_mut(0, 1); let child = slices.slice_mut(region, 0, 1); slices.borrow_ref(region, 0).id; slices.len(child); }",
        "fn bad(values: &Slice<Resource>) { slices.borrow_mut_ref(values, 0); }",
        "fn bad(values: &Slice<Resource>) -> Resource { slices.get(values, 0).unwrap_or(Resource { id: 0, values: Array.new::<Int32>() }) }",
        "fn bad(values: &mut Slice<Resource>, next: Resource) { slices.set(values, 0, next); }",
        "fn bad(values: &Slice<Resource>) { slices.borrow(values, 0); }",
        "fn bad(values: &mut Slice<Resource>) { slices.borrow_mut(values, 0); }",
        "fn bad(values: &Slice<(Int32, &Int32)>) { slices.borrow_ref(values, 0); }",
        "fn bad(values: &mut Slice<(Int32, &Int32)>) { slices.borrow_mut_ref(values, 0); }",
        "fn bad() -> &Slice<Resource> { let values = Array.new::<Resource>(); values.slice(0, 0) }",
        "fn bad() -> &Resource { let values = Array.new::<Resource>(); slices.borrow_ref(values.slice(0, 0), 0) }",
        "fn bad(values: Array<Resource>) -> &Slice<Resource> { values.slice(0, 0) }",
        "struct Stored { view: &Slice<Resource> } fn bad() {}",
        "fn bad(values: &Slice<Resource>) @suspends -> UInt64 { slices.len(values) }",
        "fn bad(values: &mut Slice<Resource>, next: Resource) { let item = slices.borrow_ref(values, 0); mem.replace(slices.borrow_mut_ref(values, 0), next); item.id; }",
        "fn bad(values: &Slice<Resource>) { alloc(slices.borrow_ref(values, 0)); }",
    ] {
        let source = format!("{prefix} {body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(vm.load_source(&source).is_err(), "{mode:?}: {body}");
        }
        let ast = crate::TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &crate::ModuleSources::new());
        if let Ok(ast) = ast {
            assert!(crate::aot::AotCompiler::new("invalid-slice")
                .unwrap().compile_module(&ast).is_err(), "{body}");
        }
    }
}
