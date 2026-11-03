use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;
use crate::{FunctionSignature, Type};

thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

extern "C" fn record_drop(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}

const SOURCE: &str = include_str!("../../../../tests/fixtures/owner_fields.jk");

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
        .unwrap_or_else(|e| panic!("{e} at {:?}", e.span()));
    vm
}

#[test]
fn owner_fields_preserve_values_and_cleanup_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for (entry, failure, expected) in [
            ("fields_rhs_metadata", false, vec![22, 1, 23, 3]),
            ("fields_extract_failure", true, vec![1, 2, 3]),
            ("fields_extract_original_failure", true, vec![2, 1, 3]),
            ("fields_reinitialize_failure", true, vec![2, 3, 4, 5, 6, 1]),
            ("fields_nested_generic_drop", false, vec![0, 5, 6]),
            ("fields_generic_drop", false, vec![3, 0, 4, 2, 1]),
            ("fields_first", false, vec![1, 2, 3]),
            ("fields_middle", false, vec![2, 1, 3]),
            ("fields_last", false, vec![3, 1, 2]),
            ("fields_temporary", false, vec![2, 1, 3]),
            ("fields_return", false, vec![1, 3, 2]),
            ("fields_reinitialize", false, vec![1, 3, 2, 4, 5, 6]),
            ("fields_replace", false, vec![2, 5, 1, 8, 3]),
            ("fields_replace_edges", false, vec![1, 3, 4, 2, 6]),
            ("fields_nested", false, vec![2, 9, 1, 3, 8]),
            ("fields_nested_temporary", false, vec![2, 1, 3, 9, 8]),
            ("fields_nested_replace", false, vec![2, 9, 1, 5, 3, 8]),
            ("fields_subtree", false, vec![10, 1, 2, 3, 9, 8]),
            ("fields_subtree_temporary", false, vec![10, 1, 2, 3, 9, 8]),
            (
                "fields_subtree_replace",
                false,
                vec![10, 1, 2, 3, 9, 20, 4, 5, 6, 8],
            ),
            ("fields_rhs_failure", true, vec![4, 1, 2, 3]),
            ("fields_drop_failure", true, vec![2, 4, 1, 3]),
            ("fields_original_failure", true, vec![1, 2, 3]),
            ("fields_loop", false, vec![11, 12, 10, 13, 21, 22, 20, 23]),
            ("fields_branch_true", false, vec![1, 3, 2]),
            ("fields_branch_false", false, vec![1, 3, 2]),
            ("fields_early", false, vec![2, 1, 3]),
            ("fields_transparent", false, vec![]),
            ("fields_references", false, vec![]),
            ("fields_replace_references", false, vec![]),
        ] {
            EVENTS.with(|events| events.borrow_mut().clear());
            let result = vm.call_int32_0(entry);
            assert_eq!(result.is_err(), failure, "{mode:?} {entry}: {result:?}");
            if !failure {
                assert_eq!(result.as_ref().unwrap(), &42, "{mode:?} {entry}");
            }
            if matches!(
                entry,
                "fields_rhs_failure"
                    | "fields_original_failure"
                    | "fields_extract_original_failure"
            ) {
                assert!(result.unwrap_err().message().contains("Shift"));
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected, "{mode:?}: {entry}"));
            LIVE.with(|n| assert_eq!(n.get(), 0, "{mode:?}: {entry}"));
        }
    }
}

fn check_tracked(context: &str) {
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=10 {
            let drops = events.iter().filter(|&&event| event == id).count();
            assert!(drops <= 1, "{context}, duplicate drop: {events:?}");
            if events.contains(&-id) {
                assert_eq!(drops, 1, "{context}, lost owner: {events:?}");
            }
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0, "{context}"));
}

#[test]
fn owner_field_transfers_are_atomic_at_every_interpreter_pause() {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for pause in 0..4000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("fields_task", &[]).unwrap();
        for _ in 0..pause {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        drop(task);
        check_tracked(&format!("pause {pause}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn owner_field_cleanup_survives_every_instruction_limit() {
    let mut vm = vm(VmMode::Interpreter);
    let mut completed = false;
    for budget in 1..4000 {
        EVENTS.with(|events| events.borrow_mut().clear());
        vm.set_interpreter_limits(super::InterpreterLimits {
            instruction_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        match vm.call_int32_0("fields_transitions") {
            Ok(value) => {
                assert_eq!(value, 42);
                completed = true;
            }
            Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
        }
        check_tracked(&format!("budget {budget}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn owner_field_cleanup_keeps_code_after_reload_and_vm_drop() {
    for finish in [false, true] {
        let mut vm = vm(VmMode::Interpreter);
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("fields_task", &[]).unwrap();
        for _ in 0..4000 {
            if EVENTS.with(|events| events.borrow().contains(&4)) {
                break;
            }
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert!(EVENTS.with(|events| events.borrow().contains(&4)));
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        if finish {
            for _ in 0..4000 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42));
        }
        drop(task);
        check_tracked(if finish {
            "reload finish"
        } else {
            "reload cancel"
        });
    }
}

#[test]
fn owner_fields_reject_consumed_records_and_conflicting_borrows() {
    for (body, message) in [
        ("fn bad() { var p = Single { values: Array.new::<Int32>() }; let x = p.values; p.values = Array.new::<Int32>() }", "moved"),
        ("fn bad() { var p = Single { values: Array.new::<Int32>() }; p.values = { let x = p; Array.new::<Int32>() } }", "moved"),
        ("fn bad() { let p = pack(1); let x = p.middle; p.tag; }", "moved"),
        ("fn bad() { let p = pack(1); let x = p.first; let y = p.last }", "moved"),
        ("fn bad() { let p = pack(1); let x = p.last; let y = p.first }", "moved"),
        ("fn bad() { let p = pack(1); let x = p.middle; let y = p }", "moved"),
        ("fn bad() { var p = pack(1); let x = p.middle; p.first = part::<Int32>(4) }", "moved"),
        ("fn bad() { var p = pack(1); let x = p.middle; p.tag = 5 }", "moved"),
        ("fn bad() { let p = pack(1); let x = p.middle; let y = &p.last }", "moved"),
        ("fn bad() { var p = pack(1); p.middle = { let x = p; part::<Int64>(4) } }", "moved"),
        ("fn bad() { var p = pack(1); p.middle = p.middle }", "moved"),
        ("fn bad() { var p = pack(1); p.middle = { let x = p.last; part::<Int64>(4) } }", "moved"),
        ("fn bad() { let p = nest(); let x = p.inner.middle; p.head.id; }", "moved"),
        ("fn bad() { let p = nest(); let x = p.inner.middle; p.inner.tag; }", "moved"),
        ("fn bad() { var p = pack(1); let view = &p.first; p.middle = part::<Int64>(4); view.id; }", "borrow"),
        ("fn bad() { var p = pack(1); let view = &mut p.middle; p.middle = part::<Int64>(4); view.id; }", "borrow"),
        ("fn bad() { var p = pack(1); let view = &p.last; let x = p.first; view.id; }", "borrow"),
        ("fn bad(p: &Pack) -> Part<Int64> { p.middle }", "borrowed wrapper"),
        ("fn bad(p: &mut Pack) { p.middle = part::<Int64>(4) }", "borrowed wrapper"),
        ("fn bad() { let p = groups(); let x = p.group.inner.middle }", "Drop"),
        ("fn bad() { let x = groups().group.inner.middle }", "Drop"),
        ("fn bad() { var p = groups(); p.group.inner.middle = part::<Int64>(4) }", "Drop"),
        ("fn bad() { let local = 42; var p = Refs { first: Array.new::<Int32>(), second: Array.new::<&Int32>() }; p.second.push(&local); let x = p.second }", "local borrow"),
        ("fn bad() { let local = 42; var next = Array.new::<&Int32>(); next.push(&local); var p = Refs { first: Array.new::<Int32>(), second: Array.new::<&Int32>() }; p.second = next }", "local borrow"),
        ("fn bad() { let value = alloc(42); var p = Refs { first: Array.new::<Int32>(), second: Array.new::<&Int32>() }; p.second.push((&*value)); let x = p.second ; (*(value) = 0) }", "retained"),
        ("fn bad() { let value = alloc(42); var next = Array.new::<&Int32>(); next.push((&*value)); var p = Refs { first: Array.new::<Int32>(), second: Array.new::<&Int32>() }; p.second = next ; (*(value) = 0) }", "retained"),
    ] {
        let source = format!("{SOURCE}\n{body}").replace("extern fn record_drop(value: Int32)", "fn record_drop(value: Int32) {}");
        let ast = crate::parser::parse(&source).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_module(&ast).expect_err(body);
            assert!(error.message().contains(message), "{mode:?} {body}: {error}");
            assert!(error.span().is_some());
        }
        let error = crate::aot::AotCompiler::new("invalid-owner-field")
            .unwrap().compile_module(&ast).expect_err(body);
        assert!(error.message().contains(message), "{body}: {error}");
    }
}

#[test]
fn owner_fields_preserve_generic_private_module_identity() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("owned", r#"
        import joky/array.{type Array}
        import joky/drop.{type Drop}
        extern fn record_drop(value: Int32)
        fn report(id: Int32) { record_drop(id) }
        pub struct Part<T> { pub id: Int32, values: Array<T> }
        impl<T> Drop for Part<T> { fn drop(&mut self) { report(self.id) } }
        pub struct Owned<T> { first: Part<Int32>, second: Part<T> }
        pub type Alias<T> = Owned<T>
        pub fn make<T>() -> Alias<T> {
            Owned { first: Part { id: 1, values: Array.new::<Int32>() }, second: Part { id: 2, values: Array.new::<T>() } }
        }
        pub fn extract<T>(value: Alias<T>) -> Part<T> { value.second }
        pub fn replace<T>(value: Alias<T>) -> Alias<T> {
            var current = value
            current.second = Part { id: 3, values: Array.new::<T>() }
            current
        }
    "#).unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        vm.load_source_with_modules(
            "import owned struct Tag { flag: Bool } fn extract() -> Int32 { let p = owned.extract(owned.make::<Tag>()); p.id + 40 } fn replace() -> Int32 { let p = owned.replace(owned.make::<Tag>()); 42 }", &modules
        ).unwrap_or_else(|e| panic!("{e}"));
        for (name, expected) in [("extract", vec![1, 2]), ("replace", vec![2, 1, 3])] {
            EVENTS.with(|events| events.borrow_mut().clear());
            assert_eq!(vm.call_int32_0(name).unwrap(), 42);
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        for body in [
            "let value = owned.make::<Int32>(); let x = value.second",
            "var value = owned.make::<Int32>(); value.second = owned.extract(owned.make::<Int32>())",
        ] {
            let error = vm
                .load_source_with_modules(&format!("import owned fn bad() {{ {body} }}"), &modules)
                .unwrap_err();
            assert!(error.message().contains("private"));
        }
    }
}

#[test]
fn owner_fields_keep_hooks_when_registration_allocation_fails() {
    extern "C" fn restrict_allocations() {
        crate::runtime::arena::LIMIT.with(|limit| limit.set(1));
    }
    struct ResetLimit;
    impl Drop for ResetLimit {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|limit| limit.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let mut vm = vm(VmMode::Interpreter);
    unsafe {
        vm.register_fn(
            "restrict_allocations",
            FunctionSignature::new(vec![], Type::Unit),
            restrict_allocations as *const u8,
        )
        .unwrap();
    }
    vm.load_source(&format!("{SOURCE}\nextern fn restrict_allocations()\nfn fail() -> Int32 {{ let p = pack(1); restrict_allocations(); let selected = p.middle; 42 }}")).unwrap();
    let _reset = ResetLimit;
    EVENTS.with(|events| events.borrow_mut().clear());
    let error = vm.call_int32_0("fail").unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    EVENTS.with(|events| assert_eq!(*events.borrow(), vec![2, 1, 3]));
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
