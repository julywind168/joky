//! Conditional patterns reuse match binding, borrowing and cleanup semantics.
use super::support::*;
use super::VmMode;
use crate::runtime::arena::LIVE;
use crate::ErrorCode;

const SOURCE: &str = include_str!("../../../../tests/fixtures/conditional_patterns.jk");

#[test]
fn is_pattern_rejects_bindings_and_invalid_patterns() {
    for (source, code, message) in [
        (
            "fn f() -> Bool { Some(42) is Some(n) }",
            ErrorCode::InvalidPattern,
            "cannot introduce bindings",
        ),
        (
            "fn f() -> Bool { [42] is [..rest] }",
            ErrorCode::InvalidPattern,
            "cannot introduce bindings",
        ),
        (
            "fn f() -> Bool { Some(42) is None | Some(n) }",
            ErrorCode::InvalidPattern,
            "cannot introduce bindings",
        ),
        (
            "fn f() -> Bool { true is Some(_) }",
            ErrorCode::InvalidPattern,
            "requires an enum",
        ),
        (
            "fn f() -> Bool { {} is _ }",
            ErrorCode::TypeMismatch,
            "requires a matchable value",
        ),
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

#[test]
fn is_pattern_releases_owned_inputs_before_the_next_suspension() {
    let source = r#"
        import joky/array.{type Array}
        extern fn record(value: Int32) -> Int32
        extern fn delayed_value(value: Int32) @suspends -> Int32
        enum Packet { Data(Array<Int32>), Other(Array<Int32>) }

        fn packet(select: Int32) @suspends -> Packet {
            let id = delayed_value(record(select))
            if id == 1 { Packet.Other([42]) } else { Packet.Data([42]) }
        }

        fn run(select: Int32) @suspends -> Int32 {
            let result = packet(select) is Packet.Data([42])
            delayed_value(record(if result { 2 } else { 3 }))
            if result { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        for (select, branch, result) in [(0, 3, 0), (1, 2, 42)] {
            for cancel in [false, true] {
                READY.with(|ready| ready.set(false));
                EVENTS.with(|events| events.borrow_mut().clear());
                let mut task = vm.call_async("run", &[select]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                EVENTS.with(|events| assert_eq!(*events.borrow(), [select, select]));
                READY.with(|ready| ready.set(true));
                for _ in 0..20 {
                    if EVENTS.with(|events| events.borrow().len()) == 4 {
                        break;
                    }
                    task.resume().unwrap();
                }
                let expected = [select, select, branch, branch];
                EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
                assert!(!task.is_finished());
                LIVE.with(|live| assert_eq!(live.get(), 0));
                if !cancel {
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(result));
                }
                drop(task);
                EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
                LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        }
    }
}

#[test]
fn let_else_rejects_fallthrough_and_hides_bindings_from_failure() {
    for (source, code, message) in [
        (
            "fn f() { let Some(n) = Some(42) else {} }",
            ErrorCode::InvalidControlFlow,
            "must exit control flow",
        ),
        (
            "fn f() -> Int32 { let Some(n) = Some(42) else { return n }; n }",
            ErrorCode::UnknownName,
            "n",
        ),
        (
            "fn f() { let Some(n) = Some(42) else { break } }",
            ErrorCode::InvalidControlFlow,
            "must be inside a while loop",
        ),
        (
            "fn f() { let Some(n) = Some(42) else { return }; n = 1 }",
            ErrorCode::InvalidAssignment,
            "n",
        ),
    ] {
        let error = frontend_error(source, &crate::ModuleSources::new());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
    let error = crate::parser::parse("fn f() { let Some(n) = Some(42) }").unwrap_err();
    assert!(error.message.contains("requires an else block"), "{error}");
}

#[test]
fn let_else_suspends_once_and_drops_discarded_owners_on_both_paths() {
    let source = r#"
        import joky/array.{type Array}
        extern fn record(value: Int32) -> Int32
        extern fn delayed_value(value: Int32) @suspends -> Int32
        enum Packet { Keep((Int32, Array<Int32>, Array<Int32>)), Skip(Array<Int32>) }

        fn packet(select: Int32) @suspends -> Packet {
            let id = delayed_value(record(select))
            if id == 1 { Packet.Skip([10]) } else { Packet.Keep((id, [40], [7])) }
        }

        fn run(select: Int32) @suspends -> Int32 {
            let Packet.Keep((id, kept, _)) = packet(select) else {
                delayed_value(record(-1))
                return 42
            }
            delayed_value(record(id))
            kept[0] + id
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        for (select, branch, live_owners) in [(0, -1, 0), (1, 2, 2)] {
            for cancel in [false, true] {
                READY.with(|ready| ready.set(false));
                EVENTS.with(|events| events.borrow_mut().clear());
                let mut task = vm.call_async("run", &[select]).unwrap();
                for _ in 0..3 {
                    task.resume().unwrap();
                }
                EVENTS.with(|events| assert_eq!(*events.borrow(), [select, select]));
                READY.with(|ready| ready.set(true));
                for _ in 0..20 {
                    if EVENTS.with(|events| events.borrow().len()) == 4 {
                        break;
                    }
                    task.resume().unwrap();
                }
                let expected = [select, select, branch, branch];
                EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
                assert!(!task.is_finished());
                LIVE.with(|live| assert_eq!(live.get(), live_owners));
                if !cancel {
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42));
                }
                drop(task);
                EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
                LIVE.with(|live| assert_eq!(live.get(), 0));
            }
        }
    }
}

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
                    vec![90, -9, -10, -10]
                } else {
                    vec![90, 90]
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
                    assert_eq!(EVENTS.with(|events| events.borrow().clone()), [90, 90, -9]);
                } else {
                    assert_eq!(task.cps_result(), Some(42));
                    assert_eq!(
                        EVENTS.with(|events| events.borrow().clone()),
                        [90, 90, -9, -10, -10, -11, -11]
                    );
                }
                drop(task);
            }
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}
