use super::*;
use crate::runtime::parallel::ParallelExecutor;
use crate::vm::{VmEngine, VmMode};
use std::{
    collections::HashSet,
    sync::Mutex,
    time::{Duration, Instant},
};
static THREADS: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
unsafe extern "C" fn touched(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
    THREADS.lock().unwrap().push(std::thread::current().id());
    0
}
fn wait(task: &ParallelTask) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "control task failed to drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn parallel_control_shares_vm_across_workers_and_keeps_local_calls_serial() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for inner in ["new", "interpreter"] {
            let mut vm = VmEngine::with_mode(mode);
            THREADS.lock().unwrap().clear();
            unsafe {
                vm.register_parallel_suspending_fn(
                    "touched",
                    FunctionSignature::new(vec![], Type::Unit),
                    touched,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
            if mode == VmMode::Interpreter {
                vm.set_interpreter_limits(InterpreterLimits {
                    poll_budget: Some(17),
                    ..Default::default()
                })
                .unwrap();
            }
            let source = include_str!("../../../tests/fixtures/parallel_control.jk")
                .replace("vm.Vm.new()", &format!("vm.Vm.{inner}()"))
                .replace("fn call(", "extern fn touched() @suspends\nfn call(")
                .replace(
                    "    engine.call::<Int32",
                    "    touched(); engine.call::<Int32",
                );
            vm.load_source(&source).unwrap();
            // The same ownership/close API also keeps its Local behavior.
            if inner == "new" {
                let task = vm.prepare_async("main", &[]).unwrap();
                assert_eq!(crate::runtime::driver::block_on(task).unwrap(), 0);
                THREADS.lock().unwrap().clear();
            }
            let budget = TaskBudget::new(32 << 20);
            let prepared = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap();
            drop(vm);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let task = prepared.submit(&pool).unwrap();
            wait(&task);
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(0)),
                "{mode:?}/{inner}"
            );
            let controls = pool.control_observations();
            assert_eq!(controls.len(), 1);
            assert_eq!(controls[0].1, 0);
            let workers: HashSet<_> = THREADS.lock().unwrap().iter().copied().collect();
            assert!(workers.len() >= 2);
            assert!(!workers.contains(&controls[0].0));
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{inner}");
        }
    }
}
#[test]
fn parallel_control_cancel_waits_for_local_frames_and_pool_remains_usable() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/parallel_control.jk"))
            .unwrap();
        let mut pool = ParallelExecutor::new(1, 32).unwrap();
        let budget = TaskBudget::new(32 << 20);
        for close in [false, true] {
            let task = vm
                .prepare_parallel_task("parked", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while !pool.control_observations().iter().any(|(_, n)| *n > 0) {
                assert!(!task.is_finished(), "{:?}", task.take_result().err());
                assert!(Instant::now() < deadline, "VM call did not start");
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
            assert!(pool.control_observations().iter().all(|(_, n)| *n == 0));
            drop(task);
            if !close {
                let task = vm
                    .prepare_parallel_task("main", vec![], vec![budget.clone()])
                    .unwrap()
                    .submit(&pool)
                    .unwrap();
                wait(&task);
                task.take_result().unwrap();
                drop(task);
            }
        }
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}
#[test]
fn parallel_control_budget_failures_refund_commands_proxies_and_local_arenas() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../../tests/fixtures/parallel_control.jk"))
            .unwrap();
        let mut success = false;
        for bytes in [0, 4096, 65536, 524288, 1048576, 2097152, 8388608, 33554432] {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(2, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|task| {
                    wait(&task);
                    task.take_result()
                });
            success = result.is_ok();
            if let Err(error) = &result {
                assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
            }
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}, budget={bytes}");
            if success {
                break;
            }
        }
        assert!(success);
    }
}

#[test]
fn parallel_control_keeps_file_authority_and_rejects_host_capability_packets() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            import joky/vm
            import joky/domain
            fn missing() @suspends -> Bool {
                let engine = vm.Vm.new()
                engine.load_file("Cargo.toml").is_err()
            }
            fn exported() @suspends -> domain.DirectoryHandle {
                let directory = domain.directory(1)
                directory.handle()
            }
        "#,
        )
        .unwrap();
        assert!(vm
            .prepare_parallel_task("exported", vec![], vec![])
            .err()
            .unwrap()
            .to_string()
            .contains("Host task values"));
        let mut pool = ParallelExecutor::new(2, 32).unwrap();
        let task = vm
            .prepare_parallel_task("missing", vec![], vec![])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(&task);
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Bool(true))
        );
        drop(task);
        pool.wait_closed();
    }
}
