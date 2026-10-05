//! Division semantics, trap spans, and failure propagation.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, FunctionSignature, Type};

#[test]
fn division_matches_in_direct_and_cps_execution_and_keeps_error_spans() {
    let source = include_str!("../../tests/fixtures/division.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for a in [i32::MIN, -7, -1, 0, 1, 7, i32::MAX] {
                for b in [i32::MIN, -3, -1, 0, 1, 3, i32::MAX] {
                    let result = if suspends {
                        vm.call_async("divide", &[a, b])
                            .map(|task| task.cps_result().unwrap() as i32)
                    } else {
                        vm.call_int32_2("divide", a, b)
                    };
                    if b == 0 {
                        let error = result.unwrap_err();
                        assert_eq!(error.code(), ErrorCode::RuntimeError);
                        assert_eq!(error.message(), "Division by zero");
                        assert_eq!(error.span().unwrap().slice(&source), Some("a / b"));
                    } else {
                        assert_eq!(result.unwrap(), a.wrapping_div(b));
                    }
                }
            }
            let constant = if suspends {
                vm.call_async("divide_constant", &[])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_0("divide_constant").unwrap()
            };
            assert_eq!(constant, i32::MIN);
            for (name, args, span) in [
                ("divide_nested", vec![7, 0], "a / b"),
                ("divide_recurse", vec![4], "7 / n"),
                ("divide_bool_caller", vec![0], "a / b"),
            ] {
                let error = if suspends {
                    vm.call_async(name, &args).unwrap_err()
                } else if args.len() == 1 {
                    vm.call_int32_1(name, args[0]).unwrap_err()
                } else {
                    vm.call_int32_2(name, args[0], args[1]).unwrap_err()
                };
                assert_eq!(error.code(), ErrorCode::RuntimeError);
                assert_eq!(error.span().unwrap().slice(&source), Some(span));
            }
            let error = if suspends {
                vm.call_async("divide_unit", &[0]).unwrap_err()
            } else {
                vm.call_unit_1("divide_unit", 0).unwrap_err()
            };
            assert_eq!(error.message(), "Division by zero");
            let value = if suspends {
                vm.call_async("divide_skipped", &[0])
                    .unwrap()
                    .cps_result()
                    .unwrap() as i32
            } else {
                vm.call_int32_1("divide_skipped", 0).unwrap()
            };
            assert_eq!(value, 42);
        }
    }
}

#[test]
fn division_failures_stop_later_operands_arguments_and_unit_calls() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for attribute in ["", "@suspends"] {
            for expression in [
                "record(1) / record(0) + record(99)",
                "pair(divide(record(1), record(0)), record(99))",
                "unit(record(0)); record(99)",
                "if flag(record(0)) { record(99) } else { record(98) }",
                "let text = text(record(0)); record(99)",
            ] {
                let source = format!(
                    r#"
                    extern fn record(x: Int32) -> Int32
                    fn divide(a: Int32, b: Int32) {attribute} -> Int32 {{ a / b }}
                    fn unit(b: Int32) {attribute} {{ divide(1, b); }}
                    fn flag(b: Int32) {attribute} -> Bool {{ divide(1, b) > 0 }}
                    fn text(b: Int32) {attribute} -> String {{ divide(1, b); "safe" }}
                    fn pair(a: Int32, b: Int32) {attribute} -> Int32 {{ record(97); a + b }}
                    fn compute() {attribute} -> Int32 {{ {expression} }}
                "#
                );
                let vm = parity_vm(&source, mode);
                let error = if attribute.is_empty() {
                    vm.call_int32_0("compute").unwrap_err()
                } else {
                    vm.call_async("compute", &[]).unwrap_err()
                };
                assert_eq!(error.code(), ErrorCode::RuntimeError, "{expression}");
                let span = error.span().unwrap().slice(&source).unwrap();
                assert!(span == "a / b" || span == "record(1) / record(0)", "{span}");
                EVENTS.with(|events| {
                    assert_eq!(
                        *events.borrow(),
                        if expression.contains("record(1)") {
                            vec![1, 0]
                        } else {
                            vec![0]
                        }
                    )
                });
            }
        }
    }
}

#[test]
fn division_after_pending_preserves_snapshots_spans_and_terminal_failure() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "return record(7) / delayed_value(record(x))",
            "return divide(record(7), delayed_value(record(x)))",
            "var value = 7; value = value / delayed_value((value = 99; record(x))); value",
        ] {
            for (input, expected) in [(-1, None), (0, Some(7)), (1, Some(3))] {
                let source = format!(
                    r#"
                    extern fn delayed_value(x: Int32) @suspends -> Int32
                    extern fn record(x: Int32) -> Int32
                    fn divide(a: Int32, b: Int32) -> Int32 {{ a / b }}
                    fn compute(x: Int32) @suspends -> Int32 {{ {expression} }}
                "#
                );
                let mut vm = parity_vm(&source, mode);
                let mut task = vm.call_async("compute", &[input]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                let events = if expression.starts_with("var") {
                    vec![input, input]
                } else {
                    vec![7, input, input]
                };
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
                vm.load_source("fn replacement() -> Int32 { 42 }").unwrap();
                drop(vm);
                READY.with(|ready| ready.set(true));
                if let Some(expected) = expected {
                    task.resume().unwrap();
                    assert_eq!(task.cps_result(), Some(expected));
                } else {
                    let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                        panic!("expected structured division error")
                    };
                    assert_eq!(error.code(), ErrorCode::RuntimeError);
                    assert_eq!(error.message(), "Division by zero");
                    let expected = if expression.contains("divide(") {
                        "a / b"
                    } else if expression.starts_with("var") {
                        "value / delayed_value((value = 99; record(x)))"
                    } else {
                        "record(7) / delayed_value(record(x))"
                    };
                    assert_eq!(error.span().unwrap().slice(&source), Some(expected));
                    assert_eq!(task.cps_result(), None);
                }
                assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
            }
        }
    }
}

#[test]
fn division_failure_stops_scope_siblings_and_cancelled_tasks() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn failing() @suspends { let value = 7 / delayed_value(record(-1)); record(99); }
        fn sibling() @suspends { delayed_value(record(2)); record(98); }
        fn compute() @suspends -> Int32 {
            parallel { | failing() | sibling() }
            record(97)
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
        READY.with(|ready| ready.set(true));
        let ResumeError::Failed(error) = task.resume().unwrap_err() else {
            panic!("expected division error")
        };
        assert_eq!(
            error.span().unwrap().slice(source),
            Some("7 / delayed_value(record(-1))")
        );
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let cancelled = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.resume(cancelled).unwrap();
        scheduler.cancel(cancelled).unwrap();
        READY.with(|ready| ready.set(true));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        assert_eq!(crate::runtime::error::joky_runtime_error_code(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, -1, 2, 2]));
    }
}

#[test]
fn generated_native_host_failures_are_checked_before_decoding_results() {
    thread_local! { static TEXT_ENTRY: std::cell::Cell<*const u8> = const { std::cell::Cell::new(std::ptr::null()) }; }
    unsafe extern "C" fn host_text() -> *const u8 {
        TEXT_ENTRY.with(|entry| {
            let call: unsafe extern "C" fn(*mut crate::runtime::string::JokyOwnedTextV2) -> i32 =
                unsafe { std::mem::transmute(entry.get()) };
            let mut output = crate::runtime::string::JokyOwnedTextV2::default();
            unsafe { call(&mut output) };
            // The host deliberately supplies an invalid result after recording
            // its primary error. Callers must check that error before decoding.
            std::ptr::dangling()
        })
    }

    let source = "fn text() -> String { 7 / 0; \"invalid\" } fn number() -> Int32 { 8 / 0 }";
    let ast = crate::parser::parse(source).unwrap();
    let module = std::rc::Rc::new(
        crate::backends::jit::compile_with_registry(
            &ast,
            &[],
            &std::collections::HashMap::new(),
            vec![],
            crate::runtime::suspension::ModuleId(1),
            &crate::core::resolver::BindingRegistry::default(),
            &crate::TypeRegistry::default(),
        )
        .unwrap(),
    );
    TEXT_ENTRY.with(|entry| entry.set(module.exports["text"].entry));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("", mode);
        unsafe {
            vm.register_owned_text_fn(
                "text",
                FunctionSignature::new(vec![], Type::String),
                host_text as *const u8,
                Some(module.clone()),
            )
            .unwrap();
            vm.register_owned_fn(
                "number",
                FunctionSignature::new(vec![], Type::Int32),
                module.exports["number"].entry,
                Some(module.clone()),
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            extern fn text() -> String
            extern fn number() -> Int32
            extern fn record(x: Int32) -> Int32
            fn string_call() -> Int32 { let value = text(); record(99) }
            fn number_call() -> Int32 { number() + record(99) }
        "#,
        )
        .unwrap();
        for (name, expected) in [("string_call", "7 / 0"), ("number_call", "8 / 0")] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.span().unwrap().slice(source), Some(expected));
            EVENTS.with(|events| assert!(events.borrow().is_empty()));
        }
    }
}

#[test]
fn interpreter_in_place_division_preserves_failure_order_after_resume() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 12
            var i = 0
            while i < 1 {
                delayed_value(record(x))
                x = x / 0
                record(999)
                i = i + 1
            }
            x
        }
    "#;
    let vm = parity_vm(source, VmMode::Interpreter);
    let mut task = vm.call_async("compute", &[]).unwrap();
    for _ in 0..3 {
        task.resume().unwrap();
    }
    assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
    READY.with(|ready| ready.set(true));
    assert!(task.resume().is_err());
    assert!(task.is_finished());
    assert_eq!(task.cps_result(), None);
    assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
}
