use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../tests/fixtures/checked_failure.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
#[test]
fn checked_failure_preserves_messages_locations_order_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "print_int",
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
                    span.slice(file.text()).unwrap().contains("require"),
                    "{mode:?} {name}: {:?}",
                    span.slice(file.text())
                );
                drop(vm);
                assert_eq!(error.message(), message);
                vm = VmEngine::with_mode(mode);
                unsafe {
                    vm.register_fn(
                        "print_int",
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
    modules.insert("checks", "import joky/check.{require as assert} pub fn validate() { assert(false, \"Module failure\") }").unwrap();
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
        assert_eq!(
            span.slice(file.text()),
            Some("assert(false, \"Module failure\")")
        );
        for source in [
            "import joky/check fn bad() { check.require(0, \"error\") }",
            "import joky/check fn bad() { check.require(false, 0) }",
            "import joky/check fn bad() { check.require(false) }",
        ] {
            assert!(vm.load_source(source).is_err());
        }
        vm.load_source("fn require(condition: Bool, message: String) {} fn test() -> Int32 { require(false, \"ordinary\") 42 }").unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
}
