//! Core expression and unary operand evaluation parity tests.

use super::support::*;
use super::VmMode;
use crate::runtime::suspension::ResumeError;
use crate::ErrorCode;

#[test]
fn unary_and_bool_operands_resume_once_with_snapshots_after_reload() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn delayed_bool(x: Int32) @suspends -> Bool { delayed_value(x) > 0 }
        fn compute() @suspends -> Int32 {
            var x = 5
            let result = -record(x) + -delayed_value((x = 10; record(x))) * 2
            var flag = true
            var test = flag == !delayed_bool((flag = false; record(-1)))
            var extra = 0
            while test { extra = record(42); test = false }
            result + extra
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10]));
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10, -1, -1]));
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(15));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [5, 10, 10, -1, -1, 42]));
    }
}

#[test]
fn unary_operands_propagate_failure_without_later_effects() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for expression in [
            "-(delayed_value(record(1)); fail_now(); record(99))",
            "!(delayed_value(record(1)); fail_now(); record(99); true)",
        ] {
            let source = format!(
                r#"
                extern fn delayed_value(x: Int32) @suspends -> Int32
                extern fn record(x: Int32) -> Int32
                extern fn fail_now() @suspends
                fn compute() @suspends {{ {expression}; }}
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
        }
    }
}
