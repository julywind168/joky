//! Block branches defer work, own captures and keep control flow branch-local.
use super::{support::*, VmMode};
use crate::runtime::{arena::LIVE, suspension::ContinuationStatus};

const SOURCE: &str = include_str!("../../../../tests/fixtures/concurrent_blocks.jk");
const CASES: &[(&str, &[i32])] = &[
    ("blocks_inner_closures", &[11, 41]),
    ("blocks_join", &[20, 10, 20, 20]),
    ("blocks_return", &[40]),
    ("blocks_question", &[40]),
    ("blocks_owned", &[-1, 40, 1]),
    ("blocks_aggregate", &[39]),
    ("blocks_owned_result", &[-1, 40, 1]),
    ("blocks_place_result", &[40]),
    ("blocks_closure_result", &[39]),
    ("blocks_isolated_return", &[40]),
    ("blocks_descendants", &[40]),
    ("blocks_borrowed", &[19, 19]),
    ("blocks_mutable", &[40]),
    ("blocks_race", &[-1, 11, 41, 11, 1]),
];

#[test]
fn concurrent_blocks_resume_without_replay_and_return_branch_results() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for &(name, expected) in CASES {
            READY.with(|r| r.set(false));
            EVENTS.with(|e| e.borrow_mut().clear());
            let mut task = vm.call_async(name, &[]).unwrap();
            let initial = EVENTS.with(|e| e.borrow().clone());
            for _ in 0..3 {
                assert_eq!(
                    task.status(),
                    ContinuationStatus::Pending,
                    "{mode:?}/{name}"
                );
                task.resume().unwrap();
                EVENTS.with(|e| assert_eq!(*e.borrow(), initial, "{name}"));
            }
            READY.with(|r| r.set(true));
            for _ in 0..32 {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            assert_eq!(task.cps_result(), Some(42), "{mode:?}/{name}");
            EVENTS.with(|e| assert_eq!(*e.borrow(), expected, "{mode:?}/{name}"));
            drop(task);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn concurrent_blocks_reject_conflicting_captures_and_escaping_control_flow() {
    for source in [
        "fn bad() { join { | {} } }",
        "fn bad() @suspends { race { | { 1 } | { false } }; }",
        "fn bad() @suspends { let text = \"owned\"; join { | { text } | { text } }; }",
        "fn bad() @suspends { var n = 0; let r = &mut n; join { | { *r = 1 } | { *r = 2 } } }",
        "fn bad() @suspends { let n = 1; join { | { &n } }; }",
        "fn bad() @suspends { let n = 1; let view = &n; join { | { let read = move || { *view }; read() } }; }",
        "fn value() -> Int32 { 1 } fn bad() @suspends { race { | { value } }; }",
        "fn bad() @suspends { while true { join { | { break } } } }",
        "fn bad() @suspends { while true { race { | { continue } } } }",
        "fn bad() @suspends { var n = 0; join { | { n = 1 } } }",
        "fn bad<F: FnOnce() @suspends -> Int32>(f: F) @suspends { join { | { f(); f() } }; }",
    ] {
        assert!(
            crate::core::prepared::check_source(source, &Default::default()).is_err(),
            "{source}"
        );
    }
    let source = "fn bad() @suspends { var n = 0; let r = &mut n; join { | { let read = move || { n }; read() } | { *r = 2; 0 } }; }";
    let error = crate::core::prepared::check_source(source, &Default::default()).unwrap_err();
    assert!(
        error.message().contains("conflicts with a live borrow"),
        "{error}"
    );
}

#[test]
fn concurrent_blocks_freeze_and_dispatch_on_parallel_workers() {
    use crate::runtime::parallel::{ParallelExecutor, TaskBudget};
    use crate::vm::{InstanceValue, TaskValue, VmEngine};
    use std::time::{Duration, Instant};
    let source = r#"
        import joky/task
        fn root() @suspends -> Int32 {
            let number = 20
            let view = &number
            let text = "ok"
            let (a, b) = join {
                | { task.yield_now(); *view }
                | {
                    let (x, y) = join {
                        | { task.yield_now(); *view }
                        | { task.yield_now(); int32(text.byte_length()) }
                    }
                    x + y
                }
            }
            a + b
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        let budget = TaskBudget::new(4 << 20);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !task.is_finished() {
            assert!(Instant::now() < deadline, "{mode:?}: blocks did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(42))
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn concurrent_blocks_cancel_and_fail_without_leaking_captures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for name in ["blocks_owned", "blocks_failure"] {
            READY.with(|r| r.set(false));
            EVENTS.with(|e| e.borrow_mut().clear());
            let task = vm.call_async(name, &[]).unwrap();
            drop(task);
            EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, 40, 1]));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        READY.with(|r| r.set(false));
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("blocks_failure", &[]).unwrap();
        READY.with(|r| r.set(true));
        assert!(task.resume().is_err());
        drop(task);
        EVENTS.with(|e| assert_eq!(*e.borrow(), [-1, 40, 1]));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    for name in [
        "blocks_inner_closures",
        "blocks_owned",
        "blocks_aggregate",
        "blocks_owned_result",
        "blocks_place_result",
        "blocks_descendants",
        "blocks_race",
    ] {
        assert_cancellation(
            &mut vm,
            name,
            100_000,
            sweep,
            || {
                READY.with(|r| r.set(true));
                EVENTS.with(|e| e.borrow_mut().clear());
            },
            || EVENTS.with(|e| e.borrow().clone()),
            || {
                LIVE.with(|n| assert_eq!(n.get(), 0));
                EVENTS.with(|e| {
                    let e = e.borrow();
                    let drops = e.iter().filter(|&&v| v == 1).count();
                    assert!(drops <= 1, "{name}: {e:?}");
                    if e.contains(&-1) {
                        assert_eq!(drops, 1, "{name}: {e:?}");
                    }
                });
            },
        );
    }
}

#[test]
fn concurrent_blocks_cancel_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore = "exhaustive: cancel inferred branch results and captures at every instruction"]
fn exhaustive_concurrent_blocks_cancel_every_instruction() {
    cancellation(CancellationSweep::EveryInstruction);
}
