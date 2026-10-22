//! Return, break, and continue semantics across direct and CPS lowering.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};
use crate::ErrorCode;

#[test]
fn jumps_match_in_direct_and_cps_execution() {
    let source = include_str!("../../../../tests/fixtures/jumps.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            let call = |name: &str, args: &[i32]| call_int32(&vm, suspends, name, args);
            for (name, expected) in [
                ("return_multiline", 42),
                ("return_sequence", 3),
                ("return_initializer", 42),
                ("return_assignment", 42),
                ("return_unary", 42),
                ("return_nested", 42),
                ("return_trailing", 42),
                ("return_condition", 42),
                ("break_only_condition", 42),
                ("break_in_return", 42),
                ("continue_in_return", 3),
                ("nested_condition_loops", 3),
                ("nested_breaks", 66),
                ("break_join", 6),
                ("continue_condition", 8),
                ("break_condition", 3),
                ("continue_and_break_paths", 3),
                ("short_circuit_jumps", 8),
            ] {
                assert_eq!(
                    call(name, &[]),
                    expected,
                    "{mode:?} suspends={suspends} {name}"
                );
            }
            for x in [-1i32, 0, 1, 3, 10] {
                assert_eq!(call("return_value", &[x]), x + 1);
                assert_eq!(call("return_flag", &[x]), i32::from(x != 0));
                assert_eq!(call("return_both", &[x]), if x < 0 { -1 } else { 1 });
                for name in ["return_mixed", "return_else"] {
                    assert_eq!(call(name, &[x]), if x < 0 { -1 } else { (x + 1) * 2 });
                }
                assert_eq!(call("return_argument", &[x]), if x < 0 { 9 } else { x + 1 });
                assert_eq!(call("return_operand", &[x]), if x < 0 { 9 } else { x * 2 });
                assert_eq!(call("return_short_circuit", &[x]), x.signum());
                assert_eq!(call("diverging_condition", &[x]), x.max(1));
                assert_eq!(call("break_or_return", &[x]), if x < 0 { -1 } else { 42 });
            }
            for (n, expected, returned) in
                [(0, 0, 0), (1, 1, 1), (2, 1, 2), (5, 13, 30), (10, 13, 30)]
            {
                assert_eq!(call("break_continue", &[n]), expected);
                assert_eq!(call("return_loop", &[n]), returned);
            }
            call_unit(&vm, suspends, "return_unit");
            assert_eq!(call("return_or_diverge", &[0]), 42);
        }
    }
}

#[test]
fn jumps_resume_without_replaying_conditions_operands_or_return_values() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn compute() @suspends -> Int32 {
            var i = 0; var total = 0
            while delayed_value(record(i)) < 10 {
                i = i + 1
                if i == 2 { continue }
                if i == 4 { break }
                total = total + delayed_value(record(i + 10))
            }
            return record(total) + delayed_value(record(20))
        }
        fn nested() @suspends -> Int32 {
            return delayed_value(record(1)) + delayed_value(record(2))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for (name, inputs, expected) in [
            ("compute", vec![0, 11, 1, 2, 13, 3, 20], 47),
            ("nested", vec![1, 2], 5),
        ] {
            let mut vm = parity_vm(source, mode);
            let mut task = vm.call_async(name, &[]).unwrap();
            let mut events = Vec::new();
            for (index, input) in inputs.iter().enumerate() {
                if name == "compute" && index == inputs.len() - 1 {
                    events.push(26);
                    vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
                }
                events.extend([*input, *input]);
                for _ in 0..3 {
                    task.resume().unwrap();
                    assert!(!task.is_finished());
                }
                EVENTS.with(|actual| assert_eq!(*actual.borrow(), events, "{mode:?} {name}"));
                READY.with(|ready| ready.set(true));
                task.resume().unwrap();
                READY.with(|ready| ready.set(false));
            }
            assert_eq!(task.cps_result(), Some(expected));
            assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        }
    }
}

#[test]
fn jumps_wait_for_scope_completion_and_do_not_start_aborted_scopes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(
            r#"
            extern fn delayed_value(x: Int32) @suspends -> Int32
            extern fn record(x: Int32) -> Int32
            fn branch_task(x: Int32) @suspends { return (delayed_value(x);) }
            fn skipped() @suspends -> Int32 {
                parallel { | branch_task(record(1)) | branch_task(return 42) }
            }
            fn compute() @suspends -> Int32 {
                var i = 0
                while true {
                    i = i + 1
                    parallel { | branch_task(i) | branch_task(i + 10) }
                    if i == 1 { continue }
                    break
                }
                return record(42)
            }
        "#,
            mode,
        );
        assert_eq!(
            vm.call_async("skipped", &[]).unwrap().cps_result(),
            Some(42)
        );
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1]));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut task = vm.call_async("compute", &[]).unwrap();
        for iteration in 1..=2 {
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            let expected = if iteration == 1 {
                vec![1, 11]
            } else {
                vec![1, 11, 2, 12]
            };
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
            READY.with(|ready| ready.set(false));
        }
        assert_eq!(task.cps_result(), Some(42));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 11, 2, 12, 42]));
    }
}

#[test]
fn return_failures_and_cancellation_do_not_complete_successfully() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends -> Int32 {
            return (delayed_value(record(1)); fail_now(); record(99))
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
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
                assert_eq!(error.span().unwrap().slice(source), Some("fail_now()"));
            }
        }
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.resume(id).unwrap();
        scheduler.cancel(id).unwrap();
        READY.with(|ready| ready.set(true));
        assert_eq!(scheduler.resume_all().unwrap(), 0);
        EVENTS.with(|events| assert_eq!(*events.borrow(), [1, 1]));
    }
}
