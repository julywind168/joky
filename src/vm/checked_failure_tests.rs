use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../tests/fixtures/checked_failure.jk");
const PANIC_SOURCE: &str = include_str!("../../tests/fixtures/panic.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}

#[test]
fn panic_diverges_and_preserves_messages_order_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            unsafe {
                vm.register_fn(
                    "record_int",
                    FunctionSignature::new(vec![Type::Int32], Type::Unit),
                    record as *const u8,
                )
                .unwrap();
            }
            vm.load_source_with_modules(PANIC_SOURCE, &crate::ModuleSources::new())
                .unwrap();
            assert_eq!(vm.call_int32_0("panic_success").unwrap(), 42);
            for (name, message, events) in [
                ("panic_cleanup", "Primary panic 42", vec![10, 2, 1]),
                ("panic_argument", "Argument failure", vec![11]),
                ("panic_match", "Missing value", vec![]),
                ("panic_both", "Both branches", vec![]),
                ("panic_return", "Return operand", vec![]),
                ("panic_indirect", "Indirect panic", vec![]),
            ] {
                EVENTS.with(|e| e.borrow_mut().clear());
                let error = vm.call_int32_0(name).unwrap_err();
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{mode:?} {name}");
                assert_eq!(error.message(), message, "{mode:?} {name}");
                let span = error.span().unwrap();
                let file = error
                    .sources()
                    .get(span.source_id())
                    .unwrap_or_else(|| panic!("{mode:?} {name}: {error:?}"));
                assert!(span.slice(file.text()).unwrap().contains("panic"));
                EVENTS.with(|e| assert_eq!(*e.borrow(), events, "{mode:?} {name}"));
                LIVE.with(|n| assert_eq!(n.get(), 0, "{mode:?} {name}"));
            }
            EVENTS.with(|e| e.borrow_mut().clear());
            // Cancellation at the explicit yield must clean the owner without
            // executing the panic that follows it.
            let task = vm.call_async("panic_task", &[]).unwrap();
            assert!(!task.is_finished());
            drop(task);
            EVENTS.with(|e| assert_eq!(*e.borrow(), [3]));
            LIVE.with(|n| assert_eq!(n.get(), 0));
            EVENTS.with(|e| e.borrow_mut().clear());
            let error = {
                let mut task = vm.call_async("panic_task", &[]).unwrap();
                loop {
                    match task.resume() {
                        Err(crate::runtime::suspension::ResumeError::Failed(e)) => break e,
                        Err(e) => panic!("unexpected resume error: {e:?}"),
                        Ok(_) if task.is_finished() => panic!("panic completed successfully"),
                        _ => {}
                    }
                }
            };
            drop(vm);
            assert_eq!(error.message(), "Task panic");
            EVENTS.with(|e| assert_eq!(*e.borrow(), [3]));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn panic_uses_bound_identity_and_preserves_alias_call_sites() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "failures",
            "import joky/check.{panic as fail} pub fn value() -> Int32 { fail(\"Aliased panic\") }",
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn test() -> Int32 { panic(\"Prelude panic\") }")
            .unwrap();
        assert_eq!(
            vm.call_int32_0("test").unwrap_err().message(),
            "Prelude panic"
        );
        vm.load_source(
            "import joky/check fn test() -> Int32 { check.assert(false, \"Literal require\") 0 }",
        )
        .unwrap();
        assert_eq!(
            vm.call_int32_0("test").unwrap_err().message(),
            "Literal require"
        );
        vm.set_prelude_enabled(false);
        assert!(vm
            .load_source("fn test() { panic(\"Unavailable\") }")
            .is_err());
        vm.load_source_with_modules(
            "import failures fn test() -> Int32 { failures.value() }",
            &modules,
        )
        .unwrap();
        let error = vm.call_int32_0("test").unwrap_err();
        let span = error.span().unwrap();
        let file = error.sources().get(span.source_id()).unwrap();
        assert_eq!(file.name(), "failures.jk");
        assert_eq!(span.slice(file.text()), Some("fail(\"Aliased panic\")"));
        vm.set_prelude_enabled(true);
        vm.load_source(
            "fn panic(value: Int32) -> Int32 { value } fn test() -> Int32 { panic(42) }",
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        for (source, code) in [
            ("fn test() { panic(42) }", ErrorCode::TypeMismatch),
            ("fn test() { panic() }", ErrorCode::InvalidCall),
            ("fn test() -> Int32 { panic(\"stop\") 42 }", ErrorCode::UnreachableCode),
            ("fn add(a: Int32, b: Int32) -> Int32 { a+b } fn test() -> Int32 { add(panic(\"stop\"), 42) }", ErrorCode::UnreachableCode),
            ("fn fail() { panic(\"stop\") } fn test() -> Int32 { fail() }", ErrorCode::TypeMismatch),
            ("fn test() -> Int32 { let fail = panic let message = \"stop\" fail(&message) }", ErrorCode::TypeMismatch),
        ] {
            assert_eq!(vm.load_source(source).unwrap_err().code(), code, "{mode:?}: {source}");
        }
    }
}

#[test]
fn panic_message_allocation_failure_reclaims_temporaries() {
    use crate::runtime::arena::{LIMIT, MAX_BYTES};
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let _reset = Reset;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(r#"fn test() -> Int32 { panic("value={42}") }"#)
            .unwrap();
        let mut complete = false;
        for limit in (1..8192).step_by(31) {
            LIMIT.with(|n| n.set(limit));
            let error = vm.call_int32_0("test").unwrap_err();
            LIMIT.with(|n| n.set(MAX_BYTES));
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if error.code() == ErrorCode::RuntimeError {
                drop(vm);
                assert_eq!(error.message(), "value=42");
                complete = true;
                break;
            }
            assert_eq!(error.code(), ErrorCode::ResourceLimit);
        }
        assert!(complete, "{mode:?}");
    }
}
#[test]
fn checked_failure_preserves_messages_locations_order_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record_int",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                record as *const u8,
            )
            .unwrap();
        }
        vm.load_source_with_modules(SOURCE, &crate::ModuleSources::new())
            .unwrap();
        for (name, message, events) in [
            ("check_success", None, vec![10, 20]),
            ("check_failure", Some("Requested failure"), vec![10, 20, 1]),
            ("check_argument_failure", Some("Condition failure"), vec![2]),
            ("check_drop_failure", Some("Drop failure"), vec![4, 3]),
            ("check_first_failure", Some("First failure"), vec![5]),
            ("check_indirect", Some("Indirect failure"), vec![]),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let result = vm.call_int32_0(name);
            if let Some(message) = message {
                let error = result.unwrap_err();
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{mode:?} {name}");
                assert_eq!(error.message(), message, "{mode:?} {name}");
                let span = error.span().unwrap();
                let file = error.sources().get(span.source_id()).unwrap();
                assert!(
                    span.slice(file.text()).unwrap().contains("assert"),
                    "{mode:?} {name}: {:?}",
                    span.slice(file.text())
                );
                drop(vm);
                assert_eq!(error.message(), message);
                vm = VmEngine::with_mode(mode);
                unsafe {
                    vm.register_fn(
                        "record_int",
                        FunctionSignature::new(vec![Type::Int32], Type::Unit),
                        record as *const u8,
                    )
                    .unwrap();
                }
                vm.load_source_with_modules(SOURCE, &crate::ModuleSources::new())
                    .unwrap();
            } else {
                assert_eq!(result.unwrap(), 42);
            }
            EVENTS.with(|e| assert_eq!(*e.borrow(), events, "{mode:?} {name}"));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        EVENTS.with(|e| e.borrow_mut().clear());
        let error = match vm.call_async("check_task", &[]) {
            Err(error) => error,
            Ok(mut task) => loop {
                match task.resume() {
                    Err(crate::runtime::suspension::ResumeError::Failed(e)) => break e,
                    Err(e) => panic!("unexpected resume error: {e:?}"),
                    Ok(_) if task.is_finished() => panic!("failure completed"),
                    _ => {}
                }
            },
        };
        assert_eq!(error.message(), "Requested failure");
        EVENTS.with(|e| assert_eq!(*e.borrow(), [10, 20, 1]));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn checked_failure_aliases_and_signature_checks_use_bound_identity() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("checks", "import joky/check.{assert as ensure} pub fn validate() { let message = \"Module failure\" ensure(false, &message) }").unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source_with_modules(
            "import checks fn test() -> Int32 { checks.validate() 0 }",
            &modules,
        )
        .unwrap();
        let error = vm.call_int32_0("test").unwrap_err();
        assert_eq!(error.message(), "Module failure");
        let span = error.span().unwrap();
        let file = error.sources().get(span.source_id()).unwrap();
        assert_eq!(file.name(), "checks.jk");
        assert_eq!(span.slice(file.text()), Some("ensure(false, &message)"));
        for source in [
            "import joky/check fn bad() { { let assertion_condition = 0 let assertion_message = \"error\" check.assert(assertion_condition, &assertion_message) } }",
            "import joky/check fn bad() { check.assert(false, 0) }",
            "import joky/check fn bad() { check.assert(false) }",
        ] {
            assert!(vm.load_source(source).is_err());
        }
        vm.load_source("fn assert(condition: Bool, message: String) {} fn test() -> Int32 { assert(false, \"ordinary\") 42 }").unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
}

#[test]
fn checked_failure_native_call_sites_preserve_the_first_error_after_cleanup_and_reload() {
    let source = r#"
        import joky/check.{assert}
        import joky/array.{type Array}
        import joky/drop.{type Drop}
        struct Resource { data: Array<Int32> }
        impl Drop for Resource { fn drop(&mut self) { { let assertion_condition = false let assertion_message = "Secondary drop failure" assert(assertion_condition, &assertion_message) } } }
        fn fail() { { let assertion_condition = false let assertion_message = "Primary failure" assert(assertion_condition, &assertion_message) } }
        fn middle() {
            let resource = Resource { data: Array.new::<Int32>() }
            let indirect = fail
            indirect()
        }
        fn test() -> Int32 { middle() 0 }
    "#;
    let mut vm = VmEngine::new();
    vm.load_source_with_modules(source, &crate::ModuleSources::new())
        .unwrap();
    let error = vm.call_int32_0("test").unwrap_err();
    vm.load_source("fn replacement() -> Int32 { 42 }").unwrap();
    drop(vm);
    assert_eq!(error.message(), "Primary failure");
    let text = |s: crate::Span| {
        s.slice(error.sources().get(s.source_id()).unwrap().text())
            .unwrap()
    };
    assert_eq!(
        text(error.span().unwrap()),
        "assert(assertion_condition, &assertion_message)"
    );
    assert_eq!(
        error
            .native_call_sites()
            .iter()
            .map(|&s| text(s))
            .collect::<Vec<_>>(),
        ["indirect()", "middle()"]
    );
    assert!(error.runtime_trace().is_empty());
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
