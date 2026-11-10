//! Structured scope and join result ownership across backends.
use super::{support::*, VmEngine, VmMode};
use crate::runtime::{
    arena::LIVE,
    suspension::{ContinuationStatus, ResumeError},
};

const VALUES: &str = include_str!("../../../../tests/fixtures/structured_values.jk");
const ENTRIES: &[&str] = &[
    "scope_scalar",
    "scope_owned",
    "scope_return",
    "mixed",
    "aggregates",
    "owned",
    "discard",
    "arrays",
    "results",
    "nested",
    "indirect",
    "repeat",
    "generic_context",
    "scope_control_flow",
    "members",
    "call_shapes",
];
fn reset() {
    EVENTS.with(|v| v.borrow_mut().clear());
}
fn live(expected: usize) {
    LIVE.with(|v| assert_eq!(v.get(), expected));
}
fn cleaned() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn structured_values_execute_in_both_vm_backends() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(VALUES, mode);
        for &entry in ENTRIES {
            reset();
            let mut task = vm
                .call_async(entry, &[])
                .unwrap_or_else(|e| panic!("{mode:?} {entry}: {e}"));
            for _ in 0..1000 {
                if task.is_finished() {
                    break;
                }
                task.resume()
                    .unwrap_or_else(|e| panic!("{mode:?} {entry}: {e:?}"));
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?} {entry}");
            cleaned();
            if entry == "scope_owned" {
                EVENTS.with(|v| assert_eq!(*v.borrow(), [-1, 10, 100]));
            }
            if entry == "call_shapes" {
                EVENTS.with(|v| assert_eq!(*v.borrow(), [-1]));
            }
        }
        reset();
        let mut task = vm.call_async("host_scalar", &[]).unwrap();
        for _ in 0..4 {
            task.resume().unwrap();
            EVENTS.with(|v| assert_eq!(*v.borrow(), [19, 21]));
        }
        READY.with(|v| v.set(true));
        task.resume().unwrap();
        assert_eq!(task.cps_result(), Some(42));
        cleaned();
    }
}
#[test]
fn structured_values_failures_clean_partial_results() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(VALUES, mode);
        for entry in ["scope_failure", "branch_failure"] {
            reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..1000 {
                if task.resume().is_err() {
                    break;
                }
            }
            assert!(
                matches!(task.status(), ContinuationStatus::Failed(_)),
                "{mode:?} {entry}"
            );
            EVENTS.with(|v| assert!(!v.borrow().contains(&100)));
            cleaned();
        }
    }
}
#[test]
fn structured_values_cancel_siblings_in_registration_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(VALUES, mode);
        for (entry, fails, events) in [
            ("ordered_race", false, &[-1, -2, -3][..]),
            ("ordered_join_failure", true, &[-1, -2]),
            ("ordered_scope_failure", true, &[-1, -2]),
            ("ordered_isolate", false, &[-1, -2]),
        ] {
            reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..1000 {
                if task.is_finished() || task.resume().is_err() {
                    break;
                }
            }
            if fails {
                assert!(
                    matches!(task.status(), ContinuationStatus::Failed(_)),
                    "{mode:?} {entry}"
                );
            } else {
                assert_eq!(task.cps_result(), Some(42), "{mode:?} {entry}");
            }
            EVENTS.with(|v| assert_eq!(v.borrow().as_slice(), events, "{mode:?} {entry}"));
            cleaned();
        }
    }
}
fn structured_values_cancel(sweep: CancellationSweep) {
    let mut vm = parity_vm(VALUES, VmMode::Interpreter);
    for entry in [
        "scope_owned",
        "scope_return",
        "owned",
        "arrays",
        "results",
        "nested",
        "indirect",
    ] {
        assert_cancellation(
            &mut vm,
            entry,
            10000,
            sweep,
            reset,
            || EVENTS.with(|v| v.borrow().clone()),
            || {
                crate::runtime::arena::LIVE.with(|live| {
                    assert_eq!(live.get(), 0);
                });
                live(0);
            },
        );
    }
}
#[test]
fn structured_values_cancel_at_resource_boundaries() {
    structured_values_cancel(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel structured results at every interpreter instruction"]
fn exhaustive_structured_values_cancel_at_each_instruction() {
    structured_values_cancel(CancellationSweep::EveryInstruction);
}
#[test]
fn structured_values_allocation_failures_release_partial_results() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|v| v.set(crate::runtime::arena::MAX_BYTES));
        }
    }
    let _restore = Restore;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(VALUES, mode);
        for entry in ["owned", "arrays", "results", "scope_owned", "nested"] {
            let mut completed = false;
            for budget in (0..65536).step_by(64) {
                EVENTS.with(|v| v.borrow_mut().clear());
                crate::runtime::arena::LIMIT.with(|v| v.set(budget));
                match vm.call_async(entry, &[]) {
                    Ok(mut task) => {
                        for _ in 0..1000 {
                            if task.is_finished() {
                                break;
                            }
                            match task.resume() {
                                Ok(()) => {}
                                Err(ResumeError::Failed(error)) => {
                                    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                                    break;
                                }
                                Err(error) => panic!("{error:?}"),
                            }
                        }
                        assert!(task.is_finished());
                        completed = task.cps_result() == Some(42);
                    }
                    Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit),
                }
                crate::runtime::arena::LIMIT.with(|v| v.set(crate::runtime::arena::MAX_BYTES));
                live(0);
                crate::runtime::arena::LIVE.with(|live| {
                    assert_eq!(live.get(), 0);
                });
                if completed {
                    break;
                }
            }
            assert!(completed, "{mode:?} {entry}");
        }
    }
}

#[test]
fn structured_values_reject_references_and_callable_results() {
    for source in [
        "fn bad() @suspends { let value = scope { alloc(42) } }",
        "struct Ref { value: &Int32 } fn bad() @suspends { let value = scope { Ref { value: alloc(42) } } }",
        "fn f() {} fn bad() @suspends { let value = scope { f } }",
        "struct Holder { f: fn() -> Unit } fn f() {} fn bad() @suspends { let value = scope { Holder { f: f } } }",
        "fn f() {} fn value() @suspends -> fn() -> Unit { f } fn bad() @suspends { let result = join { value() }; }",
        "fn value() @suspends -> &Int32 { alloc(42) } fn bad() @suspends { let result = join { value() }; }",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            let error = vm.load_source(source).expect_err(source);
            assert!(error.message().contains("reference"), "{mode:?} {source}: {error}");
        }
    }
}

#[test]
fn structured_values_native_cancellation_releases_completed_branches() {
    let vm = parity_vm(VALUES, VmMode::Jit);
    for entry in [
        "scope_owned",
        "scope_return",
        "owned",
        "arrays",
        "results",
        "nested",
        "indirect",
    ] {
        for polls in 0..8 {
            reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..polls {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            drop(task);
            cleaned();
        }
    }
}
