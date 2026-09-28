//! Loop execution, snapshots, and suspension parity tests.

use super::support::*;
use super::VmMode;
use crate::runtime::{scheduler::Scheduler, suspension::ResumeError};

#[test]
fn mutable_bindings_and_loops_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/loops.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            // Only declarations get the attribute, not calls or parentheses.
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for (name, args, expected) in [
                ("sum", vec![0], 0),
                ("sum", vec![1], 0),
                ("sum", vec![10], 45),
                ("sum", vec![-1], 0),
                ("nested", vec![4], 10),
                ("snapshots", vec![], 20123),
                ("shadowing", vec![], 47),
                ("condition_effects", vec![], 53),
                ("bool_loop", vec![], 3),
                ("comparison_snapshot", vec![], 2),
                ("precedence", vec![], 9),
            ] {
                let actual = if suspends {
                    let task = vm.call_async(name, &args).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else if args.is_empty() {
                    vm.call_int32_0(name).unwrap()
                } else {
                    vm.call_int32_1(name, args[0]).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            if suspends {
                assert!(vm.call_async("unit_values", &[]).unwrap().is_finished());
            } else {
                vm.call_unit_0("unit_values").unwrap();
            }
        }
    }
}

#[test]
fn terminal_and_adjacent_loop_labels_complete_in_direct_and_cps_execution() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        fn empty() { while false {} }
        fn adjacent() { while false { while false {} } while false {} }
        fn counted(n: Int32) {
            var i = 0
            while i < n {
                record(i)
                i = i + 1
                while false {}
            }
        }
        fn counted_negative() { counted(0 - 1) }
        fn counted_zero() { counted(0) }
        fn counted_one() { counted(1) }
        fn counted_four() { counted(4) }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            // Declarations here are indented, so match them anywhere in a line.
            let source = if suspends {
                source
                    .lines()
                    .map(|line| {
                        if line.trim_start().starts_with("fn ") {
                            line.replacen(')', ") @suspends", 1)
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                source.to_owned()
            };
            let vm = parity_vm(&source, mode);
            for name in ["empty", "adjacent"] {
                if suspends {
                    assert!(vm.call_async(name, &[]).unwrap().is_finished());
                } else {
                    vm.call_unit_0(name).unwrap();
                }
            }
            for (name, n) in [
                ("counted_negative", -1),
                ("counted_zero", 0),
                ("counted_one", 1),
                ("counted_four", 4),
            ] {
                EVENTS.with(|events| events.borrow_mut().clear());
                if suspends {
                    assert!(vm.call_async(name, &[]).unwrap().is_finished());
                } else {
                    vm.call_unit_0(name).unwrap();
                }
                assert_eq!(
                    EVENTS.with(|events| events.borrow().clone()),
                    (0..n).collect::<Vec<_>>(),
                    "{mode:?} suspends={suspends} n={n}"
                );
            }
        }
    }
}

#[test]
fn terminal_loop_condition_resumes_without_replaying_after_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute(n: Int32) @suspends {
            var i = 0
            while delayed_value(record(i)) < n {
                i = i + 1
                while false {}
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[3]).unwrap();
        for _ in 0..3 {
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [0, 0]);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            assert!(!task.is_finished());
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [0, 0, 1, 1, 2, 2]
        );
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
    }
}

#[test]
fn signed_int32_comparisons_cover_true_false_and_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for operator in ["==", "!=", "<", "<=", ">", ">="] {
            let source = format!(
                r#"
                fn compare(a: Int32, b: Int32) -> Int32 {{
                    var result = 0
                    var test = a {operator} b
                    while test {{ result = 1 test = false }}
                    result
                }}
            "#
            );
            let vm = parity_vm(&source, mode);
            for (a, b) in [
                (0, 0),
                (-1, 0),
                (0, -1),
                (i32::MIN, i32::MAX),
                (i32::MAX, i32::MIN),
            ] {
                let expected = match operator {
                    "==" => a == b,
                    "!=" => a != b,
                    "<" => a < b,
                    "<=" => a <= b,
                    ">" => a > b,
                    ">=" => a >= b,
                    _ => unreachable!(),
                };
                assert_eq!(
                    vm.call_int32_2("compare", a, b).unwrap(),
                    i32::from(expected),
                    "{mode:?}: {a} {operator} {b}"
                );
            }
        }
    }
}

#[test]
fn loops_resume_without_replaying_mutations_or_argument_effects() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute(n: Int32) @suspends -> Int32 {
            var i = 0
            var total = 0
            while i < n {
                let before = record(i + 100)
                total = total + delayed_value({ i = i + 1 i })
                record(before)
            }
            total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[3]).unwrap();
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [100, 1]);
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [100, 1]);
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(9));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [100, 1, 100, 101, 2, 101, 102, 3, 102]
        );
    }
}

#[test]
fn immediate_comparison_conditions_resume_once_after_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var checks = 0
            var total = 0
            while (checks = checks + 1; delayed_value(record(checks)) < 4) {
                total = total + (checks * 3)
                record(total)
            }
            checks * 10 + total
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        for expected in [
            vec![1, 1],
            vec![1, 1, 3, 2, 2],
            vec![1, 1, 3, 2, 2, 9, 3, 3],
        ] {
            READY.with(|ready| ready.set(false));
            for _ in 0..3 {
                task.resume().unwrap();
                assert!(!task.is_finished());
            }
            assert_eq!(EVENTS.with(|events| events.borrow().clone()), expected);
            READY.with(|ready| ready.set(true));
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(39));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [1, 1, 3, 2, 2, 9, 3, 3]
        );
    }
}

#[test]
fn fused_condition_keeps_operand_snapshots_across_assignment_and_pending() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 0
            var bodies = 0
            while (x = x + 1; x + delayed_value({ x = x + 1; x }) < 8) {
                bodies = bodies + 1
            }
            x * 10 + bodies
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2]);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(41));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2, 4]);
    }
}

#[test]
fn loop_conditions_can_suspend_and_scope_children_restart_each_iteration() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn branch(x: Int32) @suspends { delayed_value(x) {} }
        fn compute() @suspends -> Int32 {
            var i = 0
            while delayed_value(record(i)) <= 2 {
                scope { | branch(i + 10) | branch({ i = i + 1 i + 20 }) }
                record(i + 30)
            }
            i
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..2 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [0, 0]);
        READY.with(|ready| ready.set(true));
        for _ in 0..10 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(2));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [0, 0, 10, 21, 31, 1, 1, 11, 22, 32, 2, 2]
        );
    }
}

#[test]
fn in_place_arithmetic_preserves_snapshots_across_repeated_pending_and_reload() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn compute() @suspends -> Int32 {
            var x = 1
            var i = 0
            while i < 2 {
                let before = x
                x = x + 1
                x = x + delayed_value({ x = x + 10; record(x) })
                record(before)
                i = i + 1
            }
            x
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [12, 12]);
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(!task.is_finished());
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26]
        );
        READY.with(|ready| ready.set(false));
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26]
        );
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(43));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [12, 12, 1, 26, 26, 15]
        );
    }
}

#[test]
fn mutable_reads_are_snapshots_across_suspending_operands_and_scope_arguments() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pair(a: Int32, b: Int32) -> Int32 { (a * 10) + b }
        fn branch(x: Int32) @suspends { delayed_value(x) {} }
        fn compute() @suspends -> Int32 {
            var x = 1
            let saved = x + delayed_value({ x = 2 x })
            let args = pair(x, delayed_value({ x = 3 x }))
            scope { | branch(x) | branch({ x = 4 x }) }
            (saved * 100) + args
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        READY.with(|ready| ready.set(true));
        for _ in 0..4 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(424));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [2, 3, 3, 4]);
    }
}

#[test]
fn loop_failures_and_cancellation_do_not_execute_later_iterations() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn fail_now() @suspends
        fn compute() @suspends {
            var i = 0
            while i < 3 {
                i = i + 1
                delayed_value(record(i))
                fail_now()
                record(999)
            }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        READY.with(|ready| ready.set(true));
        assert!(task.resume().is_err());
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), None);
        assert_eq!(task.resume(), Err(ResumeError::AlreadyFinished));
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
        READY.with(|ready| ready.set(false));
        EVENTS.with(|events| events.borrow_mut().clear());
        let mut scheduler = Scheduler::new();
        let id = scheduler.suspend(vm.call_async("compute", &[]).unwrap());
        scheduler.cancel(id).unwrap();
        READY.with(|ready| ready.set(true));
        scheduler.resume_all().unwrap();
        assert_eq!(scheduler.task_count(), 0);
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
    }
}
