//! Semicolon sequence evaluation parity tests.

use super::support::*;
use super::VmMode;

#[test]
fn newline_boundaries_and_method_chains_match_direct_and_cps_execution() {
    let source = include_str!("../../../../tests/fixtures/newlines.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(source, suspends, Suspends::All);
            let vm = parity_vm(&source, mode);
            for (name, expected) in [
                ("newline_deref", 42),
                ("newline_prefix", -42),
                ("newline_borrow", 42),
                ("newline_operators", 42),
                ("newline_grouped", 42),
                ("newline_nested_block", 42),
                ("newline_methods", 42),
                ("newline_named_methods", 42),
                ("newline_generic_method", 42),
                ("newline_owning_chain", 42),
                ("newline_tail", 42),
                ("newline_return", 42),
            ] {
                let actual = call_int32(&vm, suspends, name, &[]);
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            call_unit(&vm, suspends, "newline_discard");
        }
    }
}

#[test]
fn newline_method_chains_resume_without_replaying_the_receiver_or_arguments() {
    let source = format!(
        "{}\n{}",
        include_str!("../../../../tests/fixtures/newlines.jk"),
        r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn compute() @suspends -> Int32 {
            Value { n: delayed_value(record(19)) }
                // Continuation belongs to this receiver after Pending.
                .add(record(22))
                .value()
        }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(&source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        for _ in 0..3 {
            task.resume().unwrap();
        }
        EVENTS.with(|events| assert_eq!(*events.borrow(), [19, 19]));
        READY.with(|ready| ready.set(true));
        task.resume().unwrap();
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(42));
        EVENTS.with(|events| assert_eq!(*events.borrow(), [19, 19, 22]));
    }
}

#[test]
fn newline_method_chains_preserve_static_and_dyn_dispatch() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(
            include_str!("../../../../tests/fixtures/newline_dispatch.jk"),
            mode,
        );
        for name in ["newline_static_dispatch", "newline_dyn_dispatch"] {
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?}: {name}");
        }
    }
}

#[test]
fn semicolon_sequences_match_in_direct_and_cps_execution() {
    let source = include_str!("../../../../tests/fixtures/sequences.jk");
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
                let actual = call_int32(&vm, suspends, name, &[]);
                assert_eq!(actual, expected, "{mode:?} suspends={suspends} {name}");
            }
            call_unit(&vm, suspends, "sequence_trailing");
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
                join { | branch_task(bodies); | branch_task(bodies + 10); };
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

#[test]
fn implicit_newline_discards_point_to_the_unused_calculation() {
    let mut vm = super::VmEngine::new_interpreter();
    vm.set_prelude_enabled(false);
    for (prefix, discarded) in [
        ("let total = a\n", "- b"),
        ("", "a * b + 3"),
        ("let pair = (a, b)\n", "-pair.0"),
        ("", "true && !false"),
        ("", "&a"),
        ("let r = &a\n", "*r"),
        ("let total = a; ", "123"),
    ] {
        let source = format!("fn f(a:Int32, b:Int32) -> Int32 {{ {prefix}{discarded}\n42 }}");
        let error = vm.load_source(&source).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::UnusedValue, "{error}");
        assert_eq!(error.span().unwrap().slice(&source), Some(discarded));
    }
}
