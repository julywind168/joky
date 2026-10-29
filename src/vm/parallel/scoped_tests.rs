//! Admission boundaries and failure sweeps for private scoped inputs.
use super::*;
use crate::runtime::{cps::CpsFrame, parallel::ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SOURCE: &str = include_str!("../../../tests/fixtures/scoped_borrow_join.jk");
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "scoped task did not progress");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scoped_borrow_falls_back_for_local_ancestors_and_retransfers() {
    let source = format!(
        r#"{SOURCE}
        fn nested(values: &Array<Int32>) @suspends -> Int32 {{
            let (a, b) = join {{ | array_sum(values) | array_sum(values) }}
            a + b
        }}
        fn forwarding() @suspends -> Int32 {{
            let values = [1, 2]
            let (a, _) = join {{ | nested(&values) | task.yield_now() }}
            a
        }}
        fn slow(values: &Array<Int32>) @suspends -> Int32 {{
            while true {{ task.yield_now() }}
        }}
        fn local_race() @suspends -> Int32 {{
            let values = [1, 2]
            race {{ | nested(&values) | slow(&values) }}
        }}
        fn isolated() @suspends -> Int32 {{
            let values = [1, 2]
            match isolate {{ nested(&values) }} {{ Ok(value) => value, Err(_) => 0 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        for (name, remote_inputs) in [("forwarding", 1), ("local_race", 0), ("isolated", 0)] {
            let budget = TaskBudget::new(4 << 20);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let task = vm
                .prepare_parallel_task(name, vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(|| task.is_finished());
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(6))
            );
            let observations = crate::runtime::parallel::scoped::observations(task.identity());
            assert_eq!(
                observations.iter().filter(|o| o.source).count(),
                remote_inputs,
                "{mode:?} {name}"
            );
            for child in observations.iter().filter(|o| !o.source) {
                assert!(
                    crate::runtime::parallel::scoped::observations(child.task).is_empty(),
                    "a borrowed view was forwarded to another worker"
                );
            }
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
        }
        for name in ["array_sum", "slice_sum", "text_size"] {
            assert!(vm
                .parallel_signature(name)
                .unwrap_err()
                .message()
                .contains("unsupported packet types"));
        }
    }
}

fn budgets(amounts: impl Clone + IntoIterator<Item = usize>) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let mut success = false;
        for bytes in amounts.clone() {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("advanced", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|task| {
                    wait(|| task.is_finished());
                    task.take_result()
                });
            success = result.is_ok();
            match &result {
                Ok(value) => {
                    assert_eq!(value.value(), &TaskValue::Scalar(InstanceValue::UInt64(17)))
                }
                Err(error) => assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?} {bytes}: {error}"
                ),
            }
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} {bytes}");
            if success {
                break;
            }
        }
        assert!(success, "{mode:?}: sweep did not reach a complete join");
    }
}
#[test]
fn scoped_borrow_budget_boundaries() {
    budgets([0, 512, 4096, 16384, 65536, 1 << 20]);
}
#[test]
#[ignore = "exhaustive: scoped descriptors, view installation, mixed owners and results"]
fn exhaustive_scoped_borrow_budgets() {
    budgets((0..(1 << 20)).step_by(128));
}

#[test]
#[ignore = "exhaustive: cancel at each source preparation and suspended reader checkpoint"]
fn exhaustive_scoped_borrow_cancellation_checkpoints() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static TARGET: AtomicUsize = AtomicUsize::new(0);
    static PARKED: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn checkpoint(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 1 {
            return 1;
        }
        if COUNT.fetch_add(1, Ordering::SeqCst) == TARGET.load(Ordering::SeqCst) {
            (*frame).state = 1;
            PARKED.store(true, Ordering::SeqCst);
            return 1;
        }
        0
    }
    let source = SOURCE
        .replace("task.yield_now()", "checkpoint()")
        .replace("    let (whole,", "    checkpoint()\n    let (whole,")
        .replace("    values[0] = 42", "    checkpoint()\n    values[0] = 42");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "checkpoint",
                FunctionSignature::new(vec![], Type::Unit),
                checkpoint,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&format!("{source}\nextern fn checkpoint() @suspends"))
            .unwrap();
        let mut complete = false;
        for boundary in 0..64 {
            COUNT.store(0, Ordering::SeqCst);
            TARGET.store(boundary, Ordering::SeqCst);
            PARKED.store(false, Ordering::SeqCst);
            let budget = TaskBudget::new(4 << 20);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(|| PARKED.load(Ordering::SeqCst) || task.is_finished());
            complete = !PARKED.load(Ordering::SeqCst);
            if !complete {
                task.request_cancel();
            }
            wait(|| task.is_finished());
            let result = task.take_result();
            if complete {
                assert_eq!(
                    result.as_ref().unwrap().value(),
                    &TaskValue::Scalar(InstanceValue::Int32(104))
                );
            } else {
                assert_eq!(result.as_ref().err().unwrap().code(), ErrorCode::Cancelled);
            }
            drop((result, task));
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} checkpoint {boundary}");
            if complete {
                break;
            }
        }
        assert!(complete);
    }
}
