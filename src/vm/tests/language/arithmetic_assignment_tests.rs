//! Remainder and compound assignment share evaluation and failure semantics.
use super::support::*;
use super::{VmEngine, VmMode};
use crate::ErrorCode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/arithmetic_assignment.jk");

#[test]
fn arithmetic_assignment_matches_in_direct_and_cps_execution() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(SOURCE, suspends, Suspends::Typed).replace(
                "fn same(value: &mut Int32) @suspends",
                "fn same(value: &mut Int32)",
            );
            let vm = parity_vm(&source, mode);
            let value = if suspends {
                vm.call_async("compute", &[]).unwrap().cps_result().unwrap() as i32
            } else {
                vm.call_int32_0("compute").unwrap()
            };
            assert_eq!(value, 42);
        }
    }
}

#[test]
fn remainder_matches_rust_and_preserves_failure_spans() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm("fn f(a: Int32, b: Int32) -> Int32 { a % b }", mode);
        for a in [i32::MIN, -7, -1, 0, 1, 7, i32::MAX] {
            for b in [i32::MIN, -3, -1, 0, 1, 3, i32::MAX] {
                let result = vm.call_int32_2("f", a, b);
                if b == 0 {
                    let error = result.unwrap_err();
                    assert_eq!(error.code(), ErrorCode::RuntimeError);
                    assert_eq!(error.message(), "Division by zero");
                    assert_eq!(
                        error
                            .span()
                            .unwrap()
                            .slice("fn f(a: Int32, b: Int32) -> Int32 { a % b }"),
                        Some("a % b")
                    );
                } else {
                    assert_eq!(result.unwrap(), a.wrapping_rem(b));
                }
            }
        }
    }
}

#[test]
fn compound_assignment_snapshots_before_suspension_and_evaluates_address_once() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn address(p: &mut Int32) -> &mut Int32 { record(1); p }
        fn f() @suspends -> Int32 {
            var x = 10
            *address(&mut x) += delayed_value(record(2))
            record(x)
            x
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for fails in [false, true] {
            let source = if fails {
                source.replace(
                    "+= delayed_value(record(2))",
                    "%= delayed_value(record(-1))",
                )
            } else {
                source.to_owned()
            };
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("f", &[]).unwrap();
            assert!(!task.is_finished());
            task.resume().unwrap();
            READY.with(|ready| ready.set(true));
            if fails {
                let error = task.resume().unwrap_err();
                assert!(matches!(
                    error,
                    crate::runtime::suspension::ResumeError::Failed(ref error) if error.code() == ErrorCode::RuntimeError
                ));
                assert!(task.is_finished());
                assert_eq!(task.cps_result(), None);
                assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, -1, -1]);
            } else {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(13));
                assert_eq!(EVENTS.with(|events| events.borrow().clone()), [1, 2, 2, 13]);
            }
        }
    }
}

#[test]
fn arithmetic_assignment_rejects_invalid_operands_and_targets() {
    for body in [
        "let x = 1; x += 2",
        "var x = 1; x += true",
        "var x: Float32 = 1.0; x %= 2.0",
        "var x: Float64 = 1.0; x <<= 2.0",
        "var x = true; x &= false",
        "let x = 1; let p = &x; *p += 1",
        "var x = 1; x += (x += 1)",
    ] {
        let mut vm = VmEngine::new();
        assert!(
            vm.load_source(&format!("fn f() {{ {body} }}")).is_err(),
            "{body}"
        );
    }
}
