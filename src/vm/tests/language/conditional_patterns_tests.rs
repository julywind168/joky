//! Conditional patterns reuse match binding, borrowing and cleanup semantics.
use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/conditional_patterns.jk");

#[test]
fn conditional_patterns_resume_without_replaying_condition_or_body() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn next(n: Int32) @suspends -> Option<Int32> {
            let value = delayed_value(record(n))
            if value < 4 { Some(value) } else { None }
        }
        fn compute() @suspends -> Int32 {
            var sum = 0
            if let Some(n) = next(0) { sum += delayed_value(record(n)) } else { record(999); }
            var i = 1
            while let Some(n) = next(i) {
                i += 1
                if n == 2 { continue }
                sum += n
            }
            sum
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("compute", &[]).unwrap();
        task.resume().unwrap();
        READY.with(|ready| ready.set(true));
        for _ in 0..40 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        assert_eq!(task.cps_result(), Some(5));
        assert_eq!(
            EVENTS.with(|events| events.borrow().clone()),
            [0, 0, 1, 1, 1, 1, 2, 2, 3, 3]
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn conditional_patterns_release_owned_bindings_on_completion_failure_and_cancel() {
    let source = format!(
        r#"
        {SOURCE}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pending() @suspends -> Int32 {{
            if let Some(value) = Some(owned(9)) {{
                delayed_value(record(90))
                record(-value.id);
            }}
            var i = 0
            while let Some(value) = (if i < 2 {{ Some(owned(10 + i)) }} else {{ None }}) {{
                i += 1
                delayed_value(record(-value.id))
                if i == 1 {{ continue }}
                break
            }}
            42
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for outcome in ["complete", "fail", "cancel", "cancel_loop"] {
            let source = if outcome == "cancel_loop" {
                source.replace("delayed_value(record(90))", "record(90)")
            } else if outcome == "fail" {
                source.replace(
                    "record(-value.id);",
                    "record(-value.id); let invalid = 1 % 0;",
                )
            } else {
                source.clone()
            };
            let vm = parity_vm(&source, mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            if outcome.starts_with("cancel") {
                drop(task);
                let expected = if outcome == "cancel_loop" {
                    vec![90, -9, 9, -10, -10, 10]
                } else {
                    vec![90, 90, 9]
                };
                assert_eq!(EVENTS.with(|events| events.borrow().clone()), expected);
            } else {
                READY.with(|ready| ready.set(true));
                let mut failed = false;
                for _ in 0..40 {
                    if task.is_finished() {
                        break;
                    }
                    if task.resume().is_err() {
                        failed = true;
                        break;
                    }
                }
                assert!(task.is_finished());
                assert_eq!(failed, outcome == "fail");
                if failed {
                    assert_eq!(
                        EVENTS.with(|events| events.borrow().clone()),
                        [90, 90, -9, 9]
                    );
                } else {
                    assert_eq!(task.cps_result(), Some(42));
                    assert_eq!(
                        EVENTS.with(|events| events.borrow().clone()),
                        [90, 90, -9, 9, -10, -10, 10, -11, -11, 11]
                    );
                }
                drop(task);
            }
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
