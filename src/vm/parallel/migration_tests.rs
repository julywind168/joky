use super::*;
use crate::runtime::parallel::ParallelExecutor;
use crate::vm::{VmEngine, VmMode};
use std::time::{Duration, Instant};
#[path = "../../../tests/fixtures/migration_session.rs"]
mod fixture;
fn wait(task: &ParallelTask) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "migration task failed to drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn migration_sessions_own_candidates_and_run_on_local_and_parallel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let source = if mode == VmMode::Jit {
            fixture::source().replace("vm.Vm.interpreter()", "vm.Vm.new()")
        } else {
            fixture::source()
        };
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            crate::runtime::driver::block_on(vm.prepare_async("main", &[]).unwrap())
                .unwrap_or_else(|e| panic!("{mode:?}: {e}")),
            0
        );
        assert_eq!(
            crate::runtime::driver::block_on(vm.prepare_async("legacy_guard", &[]).unwrap())
                .unwrap_or_else(|e| panic!("{e}")),
            0
        );
        for workers in [1, 3] {
            let budget = TaskBudget::new(64 << 20);
            let mut pool = ParallelExecutor::new(workers, 64).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(&task);
            assert_eq!(
                task.take_result()
                    .unwrap_or_else(|e| panic!("{mode:?}/{workers}: {e}"))
                    .value(),
                &TaskValue::Scalar(InstanceValue::Int32(0))
            );
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{workers}");
        }
    }
}

static PREPARED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe extern "C" fn prepared(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
    PREPARED.store(true, std::sync::atomic::Ordering::Release);
    0
}
fn parked_source() -> String {
    fixture::source()
        .replace("fn control(", "extern fn prepared() @suspends\nfn control(")
        .replace(
            "    // The owning session moves",
            "    prepared(); task.sleep(60s)\n    // The owning session moves",
        )
        .replace("    run_case(0)\n", "")
        .replace("    run_case(2)\n", "")
}
#[test]
fn migration_sessions_cancel_partial_candidates_and_close_the_control_driver() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "prepared",
                FunctionSignature::new(vec![], Type::Unit),
                prepared,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&parked_source())
            .unwrap_or_else(|e| panic!("{e}"));
        for close in [false, true] {
            PREPARED.store(false, std::sync::atomic::Ordering::Release);
            let budget = TaskBudget::new(64 << 20);
            let mut pool = ParallelExecutor::new(1, 64).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while !PREPARED.load(std::sync::atomic::Ordering::Acquire) {
                assert!(!task.is_finished(), "{}", task.take_result().err().unwrap());
                assert!(Instant::now() < deadline, "candidate not prepared");
                std::thread::sleep(Duration::from_millis(1));
            }
            if close {
                pool.request_close();
            } else {
                task.request_cancel();
            }
            wait(&task);
            assert_eq!(
                task.take_result().err().unwrap().code(),
                ErrorCode::Cancelled
            );
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{close}");
        }
    }
}

#[test]
fn migration_sessions_budget_failures_reclaim_candidates_and_session_owners() {
    let mut vm = VmEngine::with_mode(VmMode::Interpreter);
    vm.load_source(&fixture::source())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut success = false;
    for bytes in [0, 4096, 65536, 524288, 2097152, 8388608, 33554432, 67108864] {
        let budget = TaskBudget::new(bytes);
        let mut pool = ParallelExecutor::new(3, 64).unwrap();
        let result = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .and_then(|p| p.submit(&pool))
            .and_then(|task| {
                wait(&task);
                task.take_result()
            });
        success = result.is_ok();
        if let Err(error) = &result {
            // Source-level assertions unwrap explicit migration errors; both forms must clean up.
            assert!(
                matches!(error.code(), ErrorCode::ResourceLimit | ErrorCode::Panic),
                "budget={bytes}: {error}"
            );
        }
        drop(result);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "budget={bytes}");
        if success {
            break;
        }
    }
    assert!(success);
}
