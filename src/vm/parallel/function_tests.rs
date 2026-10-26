use super::*;
use crate::runtime::parallel::ParallelExecutor;
use crate::vm::{VmEngine, VmMode};
use std::{
    collections::HashSet,
    sync::Mutex,
    time::{Duration, Instant},
};
static THREADS: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
unsafe extern "C" fn touched(_: *mut CpsFrame) -> i32 {
    THREADS.lock().unwrap().push(std::thread::current().id());
    0
}
use crate::runtime::cps::CpsFrame;

#[test]
fn parallel_functions_freeze_indirect_entries_and_aggregate_packets() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
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
        let source = include_str!("../../../tests/fixtures/parallel_functions.jk")
            .replace("fn main()", "extern fn touched() @suspends\nfn main()")
            .replace(
                "{ callback.call(value) }",
                "{ touched(); callback.call(value) }",
            );
        vm.load_source(&source).unwrap();
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap();
        let cancelled = vm
            .prepare_parallel_task("cancelled", vec![], vec![budget.clone()])
            .unwrap();
        // Function indices/descriptors must keep the original immutable snapshot.
        vm.load_source("fn main() @suspends -> Int32 { 99 }")
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "parallel function task did not finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let result = task.take_result().unwrap();
        assert_eq!(
            result.value(),
            &TaskValue::Scalar(InstanceValue::Int32(0)),
            "{mode:?}"
        );
        drop((task, result));
        let task = cancelled.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "indirect race failed to cancel its losing callback"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::String("winner".into())
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
        let threads: HashSet<_> = THREADS.lock().unwrap().iter().copied().collect();
        assert!(threads.len() >= 2, "callbacks must run on distinct workers");
        assert!(!threads.contains(&std::thread::current().id()));
    }
}

#[test]
fn parallel_functions_reject_local_targets_and_host_callable_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        unsafe extern "C" fn local(value: i32) -> i32 {
            value
        }
        unsafe {
            vm.register_fn(
                "local",
                FunctionSignature::new(vec![Type::Int32], Type::Int32),
                local as *const u8,
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            extern fn local(value: Int32) -> Int32
            fn bad(value: Int32) -> Int32 { local(value) }
            fn safe(value: Int32) -> Int32 { value + 1 }
            fn apply(f: fn(Int32) -> Int32) @suspends -> Int32 { f(41) }
            fn root() @suspends -> Int32 { let f = bad; apply(f) }
            fn exported() @suspends -> fn(Int32) -> Int32 { safe }
        "#,
        )
        .unwrap();
        assert!(vm.prepare_parallel_task("root", vec![], vec![]).is_err());
        let error = vm
            .prepare_parallel_task("exported", vec![], vec![])
            .err()
            .unwrap();
        assert!(error.to_string().contains("Host task values"), "{error}");
    }
}
