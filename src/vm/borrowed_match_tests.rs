use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
const SOURCE: &str = include_str!("../../tests/fixtures/borrowed_matches.jk");
fn modules() -> crate::ModuleSources {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "match_library",
            include_str!("../../tests/fixtures/borrowed_match_library.jk"),
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
fn borrowed_matches_preserve_owners_in_both_execution_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, failure, events) in [
            ("shared_match_modules", false, vec![]),
            ("shared_match_copy", false, vec![]),
            ("shared_match_zero", false, vec![]),
            ("shared_match_inference", false, vec![]),
            ("shared_match_reference", false, vec![]),
            ("shared_match_wrapper", false, vec![]),
            ("shared_match_subtrees", false, vec![1, 2]),
            ("shared_match_reborrow", false, vec![1]),
            ("shared_match_nested", false, vec![90, 1, 10, 2, 3]),
            ("shared_match_returned", false, vec![3, 10, 1, 2]),
            ("shared_match_array", false, vec![1]),
            ("shared_match_loops", false, vec![1, 2]),
            ("shared_match_empty", false, vec![]),
            ("shared_match_failure", true, vec![1, 2]),
            ("shared_match_drop_failure", true, vec![2, 1, 3]),
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
fn borrowed_matches_keep_cleanup_at_interpreter_boundaries() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..2000 {
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("shared_match_task", &[]).unwrap();
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
        match vm.call_int32_0("shared_match_transitions") {
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
fn borrowed_matches_survive_allocation_failure_reload_and_vm_drop() {
    let _restore = RestoreLimit;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        let mut completed = false;
        for budget in (1..5000).step_by(16) {
            EVENTS.with(|e| e.borrow_mut().clear());
            crate::runtime::arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("shared_match_transitions") {
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
        let mut task = vm.call_async("shared_match_task", &[]).unwrap();
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
fn borrowed_matches_reject_conflicting_access_and_escaping_views() {
    let prefix = r#"
        import joky/array.{type Array}
        struct Resource { id: Int32, values: Array<Int32> }
        fn make() -> Resource { Resource { id: 1, values: Array.new::<Int32>() } }
        fn select(value: &Option<Resource>) -> &Resource {
            match value { | Some(resource) => resource | None => { let fail = 1 << 32 return select(value) } }
        }
    "#;
    for (body, message) in [
        ("fn bad() -> Int32 { var value = Some(make()) let r = select(&value) value = None r.id }", "borrow"),
        ("fn bad() -> Int32 { let value = Some(make()) let r = select(&value) let moved = value r.id }", "borrow"),
        ("fn bad() -> Int32 { var value = Some(make()) match &value { | Some(r) => { value = None r.id } | None => 0 } }", "borrow"),
        ("fn bad(a: &mut Array<Option<Resource>>) -> Int32 { let r = select(a.borrow_ref(0)) a.reserve(10) r.id }", "borrow"),
        ("fn bad(a: &mut Array<Option<Resource>>) -> Int32 { let r = select(a.borrow_ref(0)) a.pop(); r.id }", "borrow"),
        ("fn bad(a: &mut Array<Option<Resource>>) -> Int32 { let r = select(a.borrow_ref(0)) a.push(None) r.id }", "borrow"),
        ("fn bad(a: Array<Option<Resource>>) -> Int32 { let r = select(a.borrow_ref(0)) a.release() r.id }", "borrow"),
        ("fn bad(value: Option<Resource>, fallback: &Resource) -> &Resource { select(&value) }", "outlive"),
        ("fn bad(fallback: &Resource) -> &Resource { let value = Some(make()) match &value { | Some(r) => r | None => fallback } }", "outlive"),
        ("fn bad() -> Int32 { let r = { let value = Some(make()) select(&value) } r.id }", "outlive"),
        ("fn bad() -> Int32 { match { let value = Some(make()); &value } { | Some(r) => r.id | None => 0 } }", "outlive"),
    ] {
        let ast = crate::parser::parse(&format!("{prefix}{body}")).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert_eq!(error.code(), ErrorCode::InvalidAssignment, "{body}: {error}");
            assert!(error.message().contains(message), "{body}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("invalid-borrowed-match").unwrap()
            .compile_module(&ast).expect_err(body);
        assert_eq!(error.code(), ErrorCode::InvalidAssignment, "{body}: {error}");
        assert!(error.message().contains(message), "{body}: {error}");
    }
}

#[test]
fn borrowed_matches_keep_shared_type_and_pattern_restrictions() {
    let prefix = r#"
        import joky/array.{type Array}
        struct Resource { id: Int32, values: Array<Int32> }
        fn consume(r: Resource) {}
    "#;
    for body in [
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => consume(r) | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { let moved = r.values } | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { r.id = 2 } | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => r.values.push(2) | None => {} } }",
        "fn bad(value: &Option<Array<Int32>>) { match value { | Some(r) => r.release() | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { let pair = (r, 1) } | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { alloc(r); } | None => {} } }",
        "fn bad(value: &Option<Resource>) { match value { | Some(r) => { (*r); } | None => {} } }",
        "fn bad(value: &Option<Resource>) @suspends -> &Option<Resource> { match value { | Some(_) => {} | None => {} } value }",
        "fn pause() @suspends {} fn bad() @suspends -> Int32 { let value = Some(42) let r = &value pause() match r { | Some(x) => x | None => 0 } }",
        "extern fn host(value: &Option<Resource>) fn bad() {}",
    ] {
        let ast = crate::parser::parse(&format!("{prefix}{body}")).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.span().is_some(), "{body}: {error}");
            assert!(!error.message().contains("Return type"), "{body}: {error}");
        }
        crate::aot::AotCompiler::new("invalid-shared-pattern").unwrap()
            .compile_module(&ast).expect_err(body);
    }
    for (source, code, message) in [
        ("fn bad(value: &Int32) { match value { | _ => {} } }", ErrorCode::TypeMismatch, "enum reference"),
        ("fn bad(value: &Option<Int32>) { match value { | Some(_) => {} } }", ErrorCode::NonExhaustiveMatch, "None"),
        ("fn bad(value: &Option<Int32>) { match value { | _ => {} | Some(_) => {} } }", ErrorCode::UnreachableCode, "coverage"),
        ("fn bad(value: &Option<(Int32, Int32)>) { match value { | Some((a, a)) => {} | None => {} } }", ErrorCode::DuplicateDefinition, "Duplicate binding"),
        ("fn bad(value: &Option<Int32>) { match value { | Wrong => {} | _ => {} } }", ErrorCode::InvalidPattern, "no variant"),
        ("fn bad(value: &Option<Int32>) { match value { | Some((a, b)) => {} | None => {} } }", ErrorCode::InvalidPattern, "tuple payload"),
    ] {
        let ast = crate::parser::parse(source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(source);
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(error.message().contains(message), "{source}: {error}");
        }
        let error = crate::aot::AotCompiler::new("invalid-shared-pattern").unwrap()
            .compile_module(&ast).expect_err(source);
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn borrowed_matches_suspend_without_replaying_branch_effects() {
    use super::support;
    use crate::runtime::suspension::ContinuationStatus;
    let source = include_str!("../../tests/fixtures/borrowed_match_suspend.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = support::parity_vm(source, mode);
            let mut task = vm.call_async("shared_match_pending", &[]).unwrap();
            for _ in 0..4 {
                assert_eq!(task.status(), ContinuationStatus::Pending);
                task.resume().unwrap();
                support::EVENTS.with(|events| assert_eq!(*events.borrow(), [40, 40]));
            }
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
            drop(vm);
            if finish {
                support::READY.with(|ready| ready.set(true));
                task.resume().unwrap();
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn borrowed_matches_preserve_module_privacy_and_returned_dependencies() {
    for (body, message) in [
        ("fn bad() -> Int32 { var value = p.make() let r = p.select(&value) value = p.Parcel.Empty r.inspect() }", "borrow"),
        ("fn bad() { let value = p.make() match &value { | Item(r) => { let hidden = &r.values } | Empty => {} } }", "private"),
    ] {
        let source = format!("import match_library as p\n{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source_with_modules(&source, &modules()).expect_err(body);
            assert!(error.message().contains(message), "{body}: {error}");
        }
        let ast = crate::TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules()).unwrap();
        let error = crate::aot::AotCompiler::new("invalid-borrowed-module").unwrap().compile_module(&ast).expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}
