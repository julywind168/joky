use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, FunctionSignature, Type};
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record_drop(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
const SOURCE: &str = include_str!("../../../../tests/fixtures/memory_replace.jk");
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "replace_library",
            include_str!("../../../../tests/fixtures/memory_replace_library.jk"),
        )
        .unwrap();
    modules
}
#[test]
fn memory_replace_transfers_ownership_in_both_execution_modes() {
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
            ("replace_copy", vec![]),
            ("replace_copy_rhs", vec![]),
            ("replace_reference_array", vec![]),
            ("replace_bounds", vec![1]),
            ("replace_in_drop", vec![10, 1, 2]),
            ("replace_in_element_drop", vec![10, 1, 2]),
            ("replace_then_assign", vec![2, 1, 3, 4]),
            ("replace_copy_layout", vec![]),
            ("replace_nested", vec![2, 1, 3]),
            ("replace_wrapper_element", vec![1, 2]),
            ("replace_rhs_failure", vec![2, 1]),
            ("replace_drop_failure", vec![1, 2]),
            ("replace_option", vec![3, 4, 2, 1]),
            ("replace_element", vec![1, 2]),
            ("replace_field", vec![1, 12, 2]),
            ("replace_wrapper", vec![1, 2]),
            ("replace_enum", vec![1, 2, 3]),
            ("replace_failure", vec![1, 2]),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if name.ends_with("failure") || name == "replace_bounds" {
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
fn memory_replace_preserves_cleanup_at_every_instruction_boundary() {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "replace_task",
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
        match vm.call_int32_0("replace_transitions") {
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
fn memory_replace_preserves_cleanup_under_allocation_failure_and_reload() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut complete = false;
        for budget in (0..4096).step_by(8) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|l| l.set(budget));
            match vm.call_int32_0("replace_transitions") {
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
        let mut task = vm.call_async("replace_task", &[]).unwrap();
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
fn memory_replace_rejects_conflicting_loans_and_invalid_targets() {
    let prefix =
        "import joky/mem\nimport joky/array.{type Array} struct Resource { id: Int32, values: Array<Int32> }";
    for body in [
        "fn bad(value: &Option<Resource>) { mem.replace(value, None); }",
        "fn bad() { var value = Array.new::<Int32>(); mem.replace(&mut value, value); }",
        "fn bad(value: &mut Option<Resource>) { mem.replace(value, { mem.replace(value, None); None }); }",
        "fn bad(value: &mut Option<Resource>) { let child = match value { Some(r) => r, None => return }; mem.replace(value, None); child.id; }",
        "fn bad(value: &mut Option<Resource>) { let child = match value { Some(r) => r, None => return }; value.take(); child.id; }",
        "fn bad(value: &mut Resource, next: Resource) { let field = &value.values; mem.replace(value, next); field.len(); }",
        "fn bad(value: &mut Option<Resource>) { let moved = value; mem.replace(value, None); }",
        "fn bad(values: &mut Array<Option<Resource>>) { let old = values.borrow_ref(0); values.borrow_mut_ref(0).take(); match old { _ => {} } }",
        "fn bad(value: &mut Option<Resource>) -> &Resource { let old = mem.replace(value, None); match &old { Some(r) => r, None => bad(value) } }",
        "fn bad(value: &mut Int32) { mem.replace(value, true); }",
        "fn bad(value: &mut Int32) { mem.replace::<Bool>(value, false); }",
        "fn bad(value: Int32) { mem.replace(value, 0); }",
        "fn bad(value: &mut Array<Int32>) { mem.replace(value.borrow_mut(0), 1); }",
    ] {
        let source = format!("{prefix} {body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(vm.load_source(&source).is_err(), "{mode:?}: {body}");
        }
        let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &crate::ModuleSources::new()).unwrap();
        assert!(crate::aot::AotCompiler::new("invalid-replace").unwrap().compile_module(&ast).is_err(), "{body}");
    }
}

#[test]
fn memory_replace_suspends_without_replaying_rhs_or_mutation() {
    use super::support;
    use crate::runtime::suspension::ContinuationStatus;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            for divisor in [0, 1] {
                let mut vm = support::parity_vm(
                    include_str!("../../../../tests/fixtures/memory_replace_suspend.jk"),
                    mode,
                );
                let mut task = vm.call_async("replace_pending", &[divisor]).unwrap();
                for _ in 0..4 {
                    assert_eq!(task.status(), ContinuationStatus::Pending);
                    task.resume().unwrap();
                    support::EVENTS.with(|e| assert_eq!(*e.borrow(), [41, 40]));
                }
                vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
                drop(vm);
                if finish {
                    support::READY.with(|r| r.set(true));
                    assert_eq!(task.resume().is_ok(), divisor != 0);
                    if divisor != 0 {
                        assert_eq!(task.cps_result(), Some(42));
                    }
                }
                drop(task);
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

const MEM_SOURCE: &str = include_str!("../../../../tests/fixtures/mem.jk");
#[test]
fn mem_operations_preserve_values_and_cleanup_in_both_modes() {
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
        vm.load_source(MEM_SOURCE)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        for (name, events) in [
            ("mem_function_values", vec![]),
            ("mem_closure", vec![1, -1]),
            ("mem_copy", vec![]),
            ("mem_places", vec![]),
            ("mem_layout", vec![]),
            ("mem_owners", vec![-2, -2, 2, 1, -4, 3, 4]),
            ("mem_wrappers", vec![2, 1]),
            ("mem_enums", vec![2, 3, 1]),
            ("mem_text", vec![]),
            ("mem_references", vec![]),
            ("mem_drop_shared", vec![-1, 1]),
            ("mem_transition", vec![-1, -2, -3, 2, 1, 3]),
            ("mem_fields", vec![1, 3, 4, 2]),
            ("mem_copy_fields", vec![]),
            ("mem_drop_exclusive", vec![]),
            ("mem_assignment_drop_failure", vec![1, 2]),
            ("mem_assignment_failure", vec![2, 1]),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if name.ends_with("failure") {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), 42, "{mode:?}: {name}");
            }
            EVENTS.with(|e| assert_eq!(*e.borrow(), events, "{mode:?}: {name}"));
            LIVE.with(|n| assert_eq!(n.get(), 0, "{mode:?}: {name}"));
        }
    }
}

#[test]
fn mem_rejects_aliasing_moves_escape_and_legacy_intrinsics() {
    let prefix = "import joky/mem import joky/array.{type Array} struct Resource { id: Int32, data: Array<Int32> }";
    for body in [
        "fn bad(p: Ptr) { *p; }",
        "fn bad(p: &mut Int32) { mem.drop(p); *p = 42; }",
        "struct Refs { view: &Int32 } fn bad(p: &Refs) { let q = &(*p).view; }",
        "fn bad(p: &Int32) { *p = 42; }",
        "fn bad(p: &Int32) { let q = &mut *p; }",
        "fn bad(p: &Resource) -> Resource { *p }",
        "fn bad() { var a = Array.new::<Int32>(); mem.swap(&mut a, &mut a) }",
        "fn bad(p: &mut Int32) { mem.swap(&mut *p, &mut *p) }",
        "fn bad(p: &mut Int32) { let shared = &*p; *p = 1; let x = *shared }",
        "fn bad(p: &mut Int32) { mem.replace(&mut *p, { *p = 1; 2 }); }",
        "fn bad(p: &Int32, q: &mut Int32) { mem.swap(p, q); }",
        "fn bad(p: &mut Int32, q: &mut Bool) { mem.swap(p, q); }",
        "fn bad(p: &mut Int32) { mem.replace::<Bool>(p, 2); }",
        "fn bad(p: &mut Int32, q: &mut Int32) { mem.swap::<Bool>(p, q); }",
        "fn bad() { let x = 1; let r = &x; mem.drop(r); x = 2; }",
        "fn bad() { let a = Array.new::<Int32>(); mem.drop(a); a.len(); }",
        "fn bad() { let a = Array.new::<Int32>(); let p = &a; mem.drop(a); p.len(); }",
        "fn bad(p: &mut Resource) { let old = p; *p = *old; }",
        "fn bad() -> &Int32 { let n = 42; &*(&n) }",
        "fn bad() { let p = alloc(42); memory.read(p) }",
        "fn bad() { let p = alloc(42); memory.write(p, 1) }",
        "fn bad() { let p = alloc(42); memory.replace(p, 1) }",
        "fn bad() { let p = alloc(42); share(p) }",
    ] {
        let source = format!("{prefix} {body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(
                vm.load_source(&source).is_err(),
                "{mode:?}: accepted {body}"
            );
        }
        let ast = crate::parser::parse(&source).unwrap();
        let mut compiler = crate::aot::AotCompiler::new("invalid-mem").unwrap();
        assert!(
            compiler.compile_module(&ast).is_err(),
            "AOT: accepted {body}"
        );
    }
}

#[test]
fn mem_swap_assignment_and_drop_preserve_cleanup_at_every_boundary() {
    let _restore = RestoreLimit;
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
        vm.load_source(MEM_SOURCE).unwrap();
        let mut complete = false;
        for budget in (0..4096).step_by(8) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("mem_transition") {
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
        assert!(complete, "{mode:?}: allocation budget sweep never finished");
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        if mode != VmMode::Interpreter {
            continue;
        }
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        complete = false;
        for pause in 0..2000 {
            EVENTS.with(|e| e.borrow_mut().clear());
            let mut task = vm.call_async("mem_task", &[]).unwrap();
            for _ in 0..pause {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            complete = task.is_finished();
            if complete {
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            tracked();
            if complete {
                break;
            }
        }
        assert!(complete, "instruction cancellation sweep never finished");
        complete = false;
        for budget in 1..2000 {
            EVENTS.with(|e| e.borrow_mut().clear());
            vm.set_interpreter_limits(super::InterpreterLimits {
                instruction_budget: Some(budget),
                ..Default::default()
            })
            .unwrap();
            match vm.call_int32_0("mem_transition") {
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
        assert!(complete, "instruction failure sweep never finished");
    }
}
