use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}
extern "C" fn event(id: i32) {
    EVENTS.with(|e| e.borrow_mut().push(id));
}
const SOURCE: &str = include_str!("../../tests/fixtures/suspending_owners.jk");
fn reset() {
    EVENTS.with(|e| e.borrow_mut().clear());
}
fn check() {
    EVENTS.with(|e| {
        let e = e.borrow();
        for id in [1, 2, 3, 9] {
            let created = e.iter().filter(|&&n| n == -id).count();
            let dropped = e.iter().filter(|&&n| n == id).count();
            if created > 0 {
                assert_eq!(created, dropped, "{e:?}");
            }
            assert!(dropped <= 1, "{e:?}");
        }
    });
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "owner_event",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            event as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    vm
}
#[test]
fn suspending_owners_move_results_and_cancel_without_replaying_drops() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in [
            "record",
            "result_ok",
            "result_err",
            "tuple",
            "pair",
            "array_result",
            "builder",
            "map",
            "array_owners",
            "race_unstarted",
            "race_started",
            "dynamic",
            "dynamic_failure",
            "failed_return",
            "failure",
        ] {
            for cancel in [false, true] {
                reset();
                let result: Result<Option<u64>, crate::Error> = (|| {
                    let mut task = vm.call_async(entry, &[])?;
                    if cancel {
                        return Ok(None);
                    }
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume()?;
                    }
                    assert!(task.is_finished(), "{mode:?} {entry}");
                    Ok(task.cps_result())
                })();
                let expected_failure =
                    ["dynamic_failure", "failed_return", "failure"].contains(&entry);
                if !cancel {
                    assert_eq!(
                        result.is_err(),
                        expected_failure,
                        "{mode:?} {entry}: {result:?}"
                    );
                    if !expected_failure {
                        assert_eq!(result.unwrap(), Some(42));
                    }
                }
                check();
            }
        }
    }
}
fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = vm(VmMode::Interpreter);
    for entry in [
        "record",
        "result_ok",
        "result_err",
        "tuple",
        "pair",
        "array_result",
        "builder",
        "map",
        "array_owners",
        "dynamic",
        "race_unstarted",
    ] {
        vm.set_interpreter_limits(super::InterpreterLimits {
            poll_budget: Some(1),
            ..Default::default()
        })
        .unwrap();
        reset();
        let mut cuts = std::collections::BTreeSet::from([0]);
        let mut previous = 0;
        let mut steps: usize = 0;
        let mut task = vm.call_async(entry, &[]).unwrap();
        loop {
            let current = EVENTS.with(|e| e.borrow().len());
            if current != previous || steps.is_multiple_of(32) {
                cuts.extend(steps.saturating_sub(1)..=steps + 1);
            }
            previous = current;
            if task.is_finished() {
                break;
            }
            assert!(steps < 5000, "{entry}");
            task.resume().unwrap();
            steps += 1;
        }
        drop(task);
        check();
        cuts.insert(steps);
        if matches!(sweep, super::support::CancellationSweep::EveryInstruction) {
            cuts.extend(0..=steps);
        }
        for cut in cuts.into_iter().filter(|&n| n <= steps) {
            reset();
            let mut task = vm.call_async(entry, &[]).unwrap();
            for _ in 0..cut {
                if task.is_finished() {
                    break;
                }
                task.resume().unwrap();
            }
            drop(task);
            check();
        }
    }
}
#[test]
fn suspending_owners_cancel_at_resource_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: owning aggregate cancellation at every interpreter instruction"]
fn exhaustive_suspending_owners_cancel_at_each_instruction() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}
