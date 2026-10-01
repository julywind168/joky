use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
const SOURCE: &str = include_str!("../../tests/fixtures/mutable_borrows.jk");
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "mutable_library",
            include_str!("../../tests/fixtures/mutable_borrow_library.jk"),
        )
        .unwrap();
    modules
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
    vm.load_source_with_modules(SOURCE, &modules())
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}

#[test]
fn mutable_borrows_update_storage_in_both_execution_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, failure, events) in [
            ("mutable_match_empty", false, vec![]),
            ("mutable_match_control_flow", false, vec![3]),
            ("mutable_borrow_nested_array", false, vec![]),
            ("mutable_borrow_drop_failure", true, vec![2, 3]),
            ("mutable_borrow_modules", false, vec![]),
            ("mutable_element", false, vec![2, 21]),
            ("mutable_match_nested", false, vec![10, 20]),
            ("mutable_match_array", false, vec![41]),
            ("mutable_match_copy", false, vec![]),
            ("mutable_match_generic", false, vec![]),
            ("mutable_element_wrapper", false, vec![]),
            ("mutable_match_scalar_return", false, vec![1]),
            ("mutable_match_nested_enum", false, vec![41]),
            ("mutable_borrow_bounds", true, vec![1]),
            ("mutable_borrow_failure", true, vec![2]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            assert_eq!(result.is_err(), failure, "{mode:?} {entry}: {result:?}");
            if !failure {
                assert_eq!(result.unwrap(), 42, "{mode:?} {entry}");
            }
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {entry}"));
            LIVE.with(|count| assert_eq!(count.get(), 0, "{mode:?} {entry}"));
        }
    }
}

#[test]
fn mutable_borrows_reject_aliases_moves_and_storage_escape() {
    let prefix = r#"
        import joky/array.{type Array}
        struct Resource { id: Int32 values: Array<Int32> }
        fn identity<T>(value: &mut T) -> &mut T { value }
        fn scalar(value: &mut Option<(Resource, Int32)>) -> &mut Int32 {
            match value { | Some((_, n)) => n | None => { let fail = 1 << 32 return scalar(value) } }
        }
    "#;
    for (body, message) in [
        ("fn bad(value: &mut Option<Int32>) { let moved = value match value { | _ => {} } }", "moved"),
        ("fn bad(a: &mut Array<Resource>) -> Int32 { let r = a.borrow_mut_ref(0) a.reserve(10) r.id }", "borrow"),
        ("fn bad(a: &mut Array<Resource>) -> Int32 { let r = a.borrow_mut_ref(0) let other = a.borrow_mut_ref(1) other.id = 2 r.id }", "borrow"),
        ("fn bad(a: &mut Array<Resource>) -> Int32 { let r = a.borrow_mut_ref(0) let other = a.borrow_ref(0) r.id = 2 other.id }", "borrow"),
        ("fn bad(a: &mut Array<Resource>) -> Int32 { let r = a.borrow_ref(0) let other = a.borrow_mut_ref(0) other.id = 2 r.id }", "borrow"),
        ("fn bad(a: &mut Array<Resource>) -> Int32 { let r = identity(a.borrow_mut_ref(0)) a.pop(); r.id }", "borrow"),
        ("fn bad(a: Array<Resource>) -> Int32 { var values = a let r = values.borrow_mut_ref(0) values.release() r.id }", "borrow"),
        ("fn bad(value: &mut Option<Resource>) -> Int32 { let r = match value { | Some(r) => r | None => return 0 } let moved = r r.id }", "moved"),
        ("fn bad(value: &mut Option<(Resource, Int32)>) -> Int32 { let r = scalar(value) let other = scalar(value) memory.write(other, 1) memory.read(r) }", "borrow"),
        ("fn bad(a: &mut Array<Option<(Resource, Int32)>>) -> Int32 { let r = scalar(a.borrow_mut_ref(0)) a.pop(); memory.read(r) }", "borrow"),
        ("fn bad(a: &mut Array<Option<(Resource, Int32)>>) -> Int32 { let r = share(scalar(a.borrow_mut_ref(0))) a.reserve(10) memory.read(r) }", "borrow"),
        ("fn bad(a: &mut Array<Option<(Resource, Int32)>>) { let r = share(scalar(a.borrow_mut_ref(0))) alloc(Some(r)); }", "retained"),
        ("fn bad(a: &mut Array<Option<(Resource, Int32)>>, store: &mut Array<&Int32>) { let r = share(scalar(a.borrow_mut_ref(0))) store.push(r) }", "retained"),
        ("fn bad(a: Array<Resource>) -> &mut Resource { var values = a values.borrow_mut_ref(0) }", "return"),
        ("fn bad(a: Option<(Resource, Int32)>) -> &mut Int32 { var value = a scalar(&mut value) }", "outlive"),
        ("fn bad(value: &mut Option<Option<Int32>>) { match value { | Some(inner) => match inner { | Some(n) => { memory.write(inner, None) memory.write(n, 1) } | None => {} } | None => {} } }", "borrow"),
    ] {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert_eq!(error.code(), ErrorCode::InvalidAssignment, "{body}: {error}");
            assert!(error.message().to_lowercase().contains(message), "{body}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("invalid-mutable-borrow").unwrap().compile_module(&ast).expect_err(body);
        assert_eq!(error.code(), ErrorCode::InvalidAssignment, "{body}: {error}");
    }
}

fn tracked(context: &str) {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=6 {
            let drops = events.iter().filter(|&&event| event == id).count();
            assert!(drops <= 1, "{context}: duplicate drop {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "{context}: lost owner {events:?}");
            }
        }
    });
    LIVE.with(|count| assert_eq!(count.get(), 0, "{context}"));
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
}
#[test]
fn mutable_borrows_keep_cleanup_at_interpreter_boundaries() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("mutable_borrow_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        if completed {
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        tracked(&format!("element borrow pause {pause}"));
        if completed {
            break;
        }
    }
    assert!(completed);
    vm.set_interpreter_limits(Default::default()).unwrap();
    completed = false;
    for budget in 1..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("mutable_borrow_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
        }
        tracked(&format!("element borrow instruction budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn mutable_borrows_survive_allocation_failure_reload_and_vm_drop() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let mut completed = false;
        for budget in (1..5000).step_by(16) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("mutable_borrow_transitions") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), ErrorCode::ResourceLimit),
            }
            tracked(&format!("{mode:?} element borrow allocation {budget}"));
            if completed {
                break;
            }
        }
        assert!(completed);
        crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
    }
    for finish in [false, true] {
        let mut vm = vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("mutable_borrow_task", &[]).unwrap();
        while !EVENTS.with(|e| e.borrow().contains(&-90)) {
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
        tracked(&format!("element borrow reload finish={finish}"));
    }
}

#[test]
fn mutable_borrows_suspend_without_replaying_mutations() {
    use super::support;
    use crate::runtime::suspension::ContinuationStatus;
    let source = include_str!("../../tests/fixtures/mutable_borrow_suspend.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            for divisor in [0, 1] {
                let mut vm = support::parity_vm(source, mode);
                let mut task = vm.call_async("mutable_pending", &[divisor]).unwrap();
                for _ in 0..4 {
                    assert_eq!(task.status(), ContinuationStatus::Pending);
                    task.resume().unwrap();
                    support::EVENTS.with(|events| assert_eq!(*events.borrow(), [40, 40]));
                }
                vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
                drop(vm);
                if finish {
                    support::READY.with(|ready| ready.set(true));
                    let result = task.resume();
                    assert_eq!(result.is_ok(), divisor != 0);
                    if divisor != 0 {
                        assert_eq!(task.cps_result(), Some(42));
                    }
                }
                drop(task);
                LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        }
    }
}

#[test]
fn mutable_borrows_keep_owner_and_reference_boundaries() {
    let prefix = r#"import joky/array.{type Array}
        struct Resource { id: Int32 values: Array<Int32> }
    "#;
    for body in [
        "fn bad(a: &Array<Resource>) { a.borrow_mut_ref(0); }",
        "fn bad(a: &mut Array<Int32>) { a.borrow_mut_ref(0); }",
        "fn bad(a: &mut Array<Resource>) { let r = a.borrow_mut_ref(0) let moved = r.values }",
        "fn bad(a: &mut Array<Resource>) { let r = a.borrow_mut_ref(0) r.values = Array.new::<Int32>() }",
        "fn bad(a: &mut Array<Resource>) { memory.read(a.borrow_mut_ref(0)); }",
        "fn bad(a: &mut Array<Resource>) { alloc(a.borrow_mut_ref(0)); }",
        "fn bad(a: &mut Array<Resource>) { let pair = (a.borrow_mut_ref(0), 1) }",
        "fn bad(a: &mut Array<Resource>) @suspends { a.borrow_mut_ref(0); }",
        "fn helper<T>(value: &mut Option<T>) @suspends {} fn bad(value: &mut Option<Resource>) { helper(value) }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { r.id = 2 } | None => {} } }",
        "fn bad(value: &mut Option<Resource>) { match value { | Some(r) => r.values.release() | None => {} } }",
        "fn bad(value: &mut Option<(Resource, Int32)>) { match value { | Some((r, n)) => memory.write(value, None) | None => {} } }",
        "fn bad() { let value = alloc(Some(share(alloc(1)))) match value { | Some(r) => { let used = r } | None => {} } }",
    ] {
        let source = format!("{prefix}{body}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.span().is_some(), "{body}: {error}");
            assert!(!error.message().contains("Return type"), "{body}: {error}");
        }
        crate::aot::AotCompiler::new("invalid-mutable-boundary").unwrap().compile_module(&ast).expect_err(body);
    }
}

#[test]
fn mutable_borrows_preserve_module_privacy_and_dependencies() {
    for (body, message) in [
        ("fn bad() -> Int32 { var values = p.make() let r = p.payload(p.element(&mut values)) values.pop(); r.inspect() }", "borrow"),
        ("fn bad() { var values = p.make() match p.element(&mut values) { | Item(r) => r.values.push(1) | Empty => {} } }", "private"),
    ] {
        let source = format!("import mutable_library as p\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source_with_modules(&source, &modules()).expect_err(body);
            assert!(error.message().contains(message), "{body}: {error}");
        }
        let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules()).unwrap();
        let error = crate::aot::AotCompiler::new("invalid-mutable-module").unwrap().compile_module(&ast).expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}
