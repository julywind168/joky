//! Semicolon sequence evaluation parity tests.

use super::support::*;
use super::VmMode;

#[test]
fn semicolon_sequences_match_in_direct_and_cps_execution() {
    let source = include_str!("../../tests/fixtures/sequences.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("sequence_condition", 43),
                ("sequence_bindings", 45),
                ("sequence_scope", 33),
                ("sequence_arguments", 23),
                ("sequence_precedence", 13),
                ("sequence_mixed_separators", 15),
            ] {
                let actual = if suspends {
                    let task = vm.call_async(name, &[]).unwrap();
                    assert!(task.is_finished());
                    task.cps_result().unwrap() as i32
                } else {
                    vm.call_int32_0(name).unwrap()
                };
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            if suspends {
                assert!(vm
                    .call_async("sequence_trailing", &[])
                    .unwrap()
                    .is_finished());
            } else {
                vm.call_unit_0("sequence_trailing").unwrap();
            }
        }
    }
}

#[test]
fn semicolon_sequences_resume_without_replaying_prior_expressions() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32;
        extern fn record(x: Int32) -> Int32;
        fn branch_task(x: Int32) @suspends { delayed_value(x); }
        fn compute() @suspends -> Int32 {
            var checks = 0; var bodies = 0;
            while (checks = checks + 1; delayed_value(record(checks)) < 4) {
                bodies = bodies + 1;
                parallel { | branch_task(bodies); | branch_task(bodies + 10); };
            };
            (checks * 10) + bodies
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 1]);
        READY.with(|ready| ready.set(true));
        for _ in 0..10 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(32));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [1, 1, 1, 11, 2, 2, 2, 12, 3, 3]
        );
    }
}
