//! Iterator syntax shares loop planning while fixing standard protocol identity.
use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::LIVE;

const SOURCE: &str = include_str!("../../../../tests/fixtures/for_iterators.jk");
const EXPECTED: &[i32] = &[3, -1, 1, -2, 2, 99, -1, 1, -2, 2, -3, 3, 99, -1, 1, 99];

fn pending_source() -> String {
    format!(
        r#"{SOURCE}
        extern fn delayed_value(x: Int32) @suspends -> Int32
        fn pending() @suspends -> Int32 {{
            for value in owners() {{
                delayed_value(record(value.id + 10))
                if value.id == 1 {{ continue }}
                break
            }}
            42
        }}
    "#
    )
}

#[test]
fn for_iterators_resume_cancel_and_fail_without_replaying_next() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(&pending_source(), mode, false);
        for cancel in [false, true] {
            READY.with(|ready| ready.set(false));
            EVENTS.with(|events| events.borrow_mut().clear());
            let mut task = vm.call_async("pending", &[]).unwrap();
            for _ in 0..3 {
                task.resume().unwrap();
            }
            assert!(!task.is_finished());
            EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, 11, 11]));
            if !cancel {
                READY.with(|ready| ready.set(true));
                for _ in 0..20 {
                    if task.is_finished() {
                        break;
                    }
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            let expected = if cancel {
                &[-1, 11, 11, 1, 99][..]
            } else {
                &[-1, 11, 11, 1, -2, 12, 12, 2, 99][..]
            };
            EVENTS.with(|events| assert_eq!(*events.borrow(), expected));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        EVENTS.with(|events| events.borrow_mut().clear());
        assert!(vm.call_unit_0("force_failure").is_err());
        EVENTS.with(|events| assert_eq!(*events.borrow(), [-1, 1, 99]));
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm_with_prelude(&pending_source(), VmMode::Interpreter, false);
    assert_cancellation(
        &mut vm,
        "pending",
        10_000,
        sweep,
        || {
            READY.with(|ready| ready.set(true));
            EVENTS.with(|events| events.borrow_mut().clear());
        },
        || EVENTS.with(|events| events.borrow().clone()),
        || {
            LIVE.with(|live| assert_eq!(live.get(), 0));
            EVENTS.with(|events| {
                let events = events.borrow();
                for id in [1, 2, 99] {
                    assert!(
                        events.iter().filter(|&&event| event == id).count() <= 1,
                        "{events:?}"
                    );
                }
            });
        },
    );
}

#[test]
fn for_iterators_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel iterator and item owners at every instruction boundary"]
fn exhaustive_for_iterators_instruction_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn for_iterators_execute_protocol_patterns_and_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let source = with_suspends(SOURCE, suspends, Suspends::Named("compute"));
            let vm = parity_vm_with_prelude(&source, mode, false);
            assert_eq!(call_int32(&vm, suspends, "compute", &[]), 42);
            EVENTS.with(|events| assert_eq!(*events.borrow(), EXPECTED));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn for_iterators_reject_invalid_protocol_patterns_and_scope() {
    let mut vm = parity_vm(SOURCE, VmMode::Jit);
    for body in [
        "for value in 42 {}",
        "for value in range(0, 2) {}; value;",
        "for value in range(0, 2) { value = 4 }",
        "for (a, b) in range(0, 2) {}",
        "for Some(value) in range(0, 2) {}",
        "for 1 in range(0, 2) {}",
        "for (x, x) in (Pairs { remaining: 1 }) {}",
        "let source = owners(); for _ in source {}; source;",
        "for _ in range(0, 2) {}; break",
        "for _ in range(0, 2) {}; continue",
        "let result: Int32 = for _ in range(0, 2) {}",
        "for value in range(0, 2) { unknown() }",
    ] {
        assert!(
            vm.load_source(&format!("{SOURCE}\nfn bad() {{ {body} }}"))
                .is_err(),
            "{body}"
        );
    }
    for source in [
        "struct Fake {} impl Fake { fn next(&mut self) -> Option<Int32> { None } } fn bad() { for _ in (Fake {}) {} }",
        "trait Iterator { type Item; fn next(&mut self) -> Option<Self.Item> } struct Fake {} impl Iterator for Fake { type Item = Int32; fn next(&mut self) -> Option<Int32> { None } } fn bad() { for _ in (Fake {}) {} }",
        "import joky/iter.{type Iterator} struct Fake {} impl Iterator for Fake { type Item = Int32; fn next(&mut self) @suspends -> Option<Int32> { None } }",
        "fn bad<I>(source: I) { for _ in source {} }",
    ] {
        assert!(VmEngine::new().load_source(source).is_err(), "{source}");
    }
}

#[test]
fn for_iterators_bind_standard_names_and_interpolation_dependencies() {
    let source = r#"
        import joky/iter as it
        import joky/option as opt
        enum Option { Some, None }
        fn next() -> Bool { false }
        fn compute() -> Int32 {
            var sum = 0
            for value in it.range(0, 4) { sum += value }
            sum
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(source, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 6);
        let vm = parity_vm_with_prelude(
            r#"
            import joky/iter.{range}
            fn compute() -> Int32 {
                let text = "{for _ in range(0, 2) {}}"
                42
            }
        "#,
            mode,
            false,
        );
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
