//! Conditional and short-circuit evaluation parity and suspension tests.

use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::{ErrorCode, FunctionSignature, Type};

#[test]
fn conditionals_and_short_circuiting_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/conditionals.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let call = |name: &str, args: &[i32]| {
                if suspends {
                    let task = vm.call_async(name, args).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else {
                    match args {
                        [] => vm.call_int32_0(name).unwrap(),
                        [a] => vm.call_int32_1(name, *a).unwrap(),
                        [a, b] => vm.call_int32_2(name, *a, *b).unwrap(),
                        _ => unreachable!(),
                    }
                }
            };
            for (name, expected) in [
                ("conditional_scopes", 34),
                ("logical_precedence", 1),
                ("logical_effects", 3),
                ("logical_snapshots", 7),
                ("conditional_grouping", 11),
            ] {
                assert_eq!(
                    call(name, &[]),
                    expected,
                    "{mode:?} suspends={suspends} {name}"
                );
            }
            for value in [i32::MIN, -1, 0, 1, i32::MAX] {
                assert_eq!(call("choose", &[value]), value.signum() * 10);
            }
            for a in [0, 1] {
                for b in [0, 1] {
                    assert_eq!(call("logical_values", &[a, b]), (a & b) * 10 + (a | b));
                    assert_eq!(
                        call("conditional_nested", &[a, b]),
                        if a == 0 {
                            20
                        } else if b == 0 {
                            12
                        } else {
                            11
                        }
                    );
                }
            }
            for (n, expected) in [(-1, 0), (0, 0), (1, 1), (3, 12), (5, 32), (10, 32)] {
                assert_eq!(call("conditional_loop", &[n]), expected);
            }
            for (x, expected) in [(0, 0), (3, 4), (-1, 0)] {
                assert_eq!(call("conditional_safe_division", &[x]), expected);
            }
            if suspends {
                assert!(vm
                    .call_async("conditional_empty", &[])
                    .unwrap()
                    .is_finished());
            } else {
                vm.call_unit_0("conditional_empty").unwrap();
            }
        }
    }
}

#[test]
fn conditional_evaluation_skips_effects_failures_and_suspensions() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends -> Int32 {
            let a = false && (fail_now(); delayed_value(99) > 0)
            let b = true || (fail_now(); delayed_value(99) > 0)
            let x = if (record(1); a || b) { record(2) } else { fail_now(); delayed_value(99) }
            let y = if false { fail_now(); delayed_value(99) } else if (record(3); true) { record(4) } else { record(99) }
            if false { fail_now() }
            x * 10 + y
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let task = vm.call_async("compute", &[]).unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(24));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 3, 4]));
    }
}

#[test]
fn conditional_paths_resume_once_and_keep_string_results_after_reload() {
    unsafe extern "C" fn byte_length(text: *const u8) -> i32 {
        crate::runtime::string::borrow(text).unwrap().len() as i32
    }
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn byte_length(text: String) -> Int32
        fn compute(x: Int32) @suspends -> Int32 {
            var flag = true
            let text = if flag && delayed_value(record(x)) > 0 {
                flag = false
                delayed_value(record(10)); "left"
            } else {
                delayed_value(record(20)); "right"
            }
            let joined = byte_length(text) + if flag { 100 } else { 200 }
            record(joined) + delayed_value(record(30))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (input, arm, joined) in [(-2, 20, 105), (0, 10, 204)] {
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut vm = VmEngine::with_mode(mode);
            unsafe {
                vm.register_suspending_fn(
                    "delayed_value",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    delayed_value,
                )
                .unwrap();
                vm.register_fn(
                    "record",
                    FunctionSignature::new(vec![Type::Int32], Type::Int32),
                    record as *const u8,
                )
                .unwrap();
                vm.register_legacy_string_fn(
                    "byte_length",
                    FunctionSignature::new(vec![Type::String], Type::Int32),
                    byte_length as *const u8,
                )
                .unwrap();
            }
            vm.load_source(source).unwrap();
            let mut task = vm.call_async("compute", &[input]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [input, input]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert!(!task.is_finished());
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [input, input, arm, arm]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert!(!task.is_finished());
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
            }
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
            drop(vm);
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert_eq!(task.cps_result(), Some((joined + 31) as u64));
            EVENTS.with(|events| {
                assert_eq!(*events.borrow(), [input, input, arm, arm, joined, 30, 30])
            });
        }
    }
}

#[test]
fn short_circuit_operands_suspend_only_when_selected() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (operator, left, runs_right, expected) in [
            ("&&", -1, false, 0),
            ("&&", 0, true, 1),
            ("||", -1, true, 1),
            ("||", 0, false, 1),
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                fn compute() @suspends -> Int32 {{
                    let selected = delayed_value(record({left})) > 0 {operator} delayed_value(record(2)) > 0
                    if selected {{ record(1) }} else {{ record(0) }}
                }}
            "#
            );
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("compute", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            EVENTS.with(|events| assert_eq!(*events.borrow(), [left, left]));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            assert_eq!(task.is_finished(), !runs_right);
            if runs_right {
                READY.with(|ready| ready.set(false));
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                EVENTS.with(|events| assert_eq!(*events.borrow(), [left, left, 2, 2]));
                READY.with(|ready| ready.set(true));
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(expected as u64));
            let events = if runs_right {
                vec![left, left, 2, 2, expected]
            } else {
                vec![left, left, expected]
            };
            EVENTS.with(|actual| assert_eq!(*actual.borrow(), events));
        }
    }
}

#[test]
fn conditional_failures_and_cancellation_skip_later_effects() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "if delayed_value(record(1)) > 0 { fail_now(); 1 } else { record(99) }",
            "if true && (delayed_value(record(1)); fail_now(); true) { record(99) } else { 0 }",
            "if false || (delayed_value(record(1)); fail_now(); true) { record(99) } else { 0 }",
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                extern fn fail_now() @suspends
                fn compute() @suspends -> Int32 {{ {expression}; record(100) }}
            "#
            );
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("compute", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            READY.with(|ready| ready.set(true));
            let error = task.resume().unwrap_err();
            match mode {
                VmMode::Jit => assert_eq!(error, ResumeError::InvalidCpsStatus(7)),
                VmMode::Interpreter => {
                    let ResumeError::Failed(error) = error else {
                        panic!("expected host failure")
                    };
                    assert_eq!(error.code(), ErrorCode::HostError);
                    assert_eq!(error.span().unwrap().slice(&source), Some("fail_now()"));
                }
            }
            assert_eq!(task.cps_result(), None);
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut scheduler = Scheduler::new();
            let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
            scheduler.cancel(id).unwrap();
            READY.with(|ready| ready.set(true));
            scheduler.resume_all().unwrap();
            assert_eq!(scheduler.task_count(), 0);
            EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
        }
    }
}

#[test]
fn conditional_scope_branches_start_only_on_selected_paths() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "if x != 0 { scope { | branch(1) | branch(2) } }",
            "x != 0 && (scope { | branch(1) | branch(2) }; true)",
            "x == 0 || (scope { | branch(1) | branch(2) }; true)",
        ] {
            for input in [0, 1] {
                let source = format!(
                    r#"
                    extern fn delayed_value(x: Int32) @suspends -> Int32
                    extern fn record(x: Int32) -> Int32
                    fn branch(x: Int32) @suspends {{ delayed_value(x); }}
                    fn compute(x: Int32) @suspends -> Int32 {{ {expression}; record(42) }}
                "#
                );
                let vm = parity_vm(&source, mode);
                let mut task = vm.call_async("compute", &[input]).unwrap();
                if input == 1 {
                    assert!(!task.is_finished());
                    for _ in 0..3 {
                        task.resume().unwrap();
                    }
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2]));
                    READY.with(|ready| ready.set(true));
                    task.resume().unwrap();
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 2, 42]));
                } else {
                    EVENTS.with(|events| assert_eq!(*events.borrow(), [42]));
                }
                assert_eq!(task.cps_result(), Some(42));
            }
        }
    }
}
