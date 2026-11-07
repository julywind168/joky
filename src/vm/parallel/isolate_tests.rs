use super::*;
use crate::runtime::parallel::ParallelExecutor;
use crate::vm::{VmEngine, VmMode};
use std::time::{Duration, Instant};

fn finish(task: &ParallelTask) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "isolated task did not drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn parallel_isolate_preserves_siblings_and_parent_cancellation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn entered(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        0
    }
    let source = format!("{}\nextern fn entered() @suspends\nfn external_cancel() @suspends {{let result=isolate {{entered(); task.sleep(100s)}}; check.panic(\"cancellation swallowed\")}}", include_str!("../../../tests/fixtures/isolate.jk"));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "entered",
                FunctionSignature::new(vec![], Type::Unit),
                entered,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&source).unwrap();
        let budget = TaskBudget::new(16 << 20);
        for workers in [1, 3] {
            let mut pool = ParallelExecutor::new(workers, 64).unwrap();
            let task = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            finish(&task);
            task.take_result().unwrap();
            drop(task);
            let before = ENTERED.load(Ordering::SeqCst);
            let task = vm
                .prepare_parallel_task("external_cancel", vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while ENTERED.load(Ordering::SeqCst) == before {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            task.request_cancel();
            finish(&task);
            assert_eq!(
                task.take_result().err().unwrap().code(),
                ErrorCode::Cancelled
            );
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
        }
    }
}

fn budgets(step: usize) {
    let source = r#"import joky/task import joky/array.{type Array}
        fn run() @suspends ->Int32 {
            let result=isolate {var a=Array.new::<String>();a.push("owned input");task.yield_now();a}
            match result {Ok(a)=>if a.len()==1 {42}else{0},Err(ResourceLimit)=>7,Err(_)=>0}
        }"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let mut success = false;
        for bytes in (0..65536).step_by(step) {
            let budget = TaskBudget::new(bytes);
            let outcome = vm
                .prepare_parallel_task("run", vec![], vec![budget.clone()])
                .and_then(super::tests::finish);
            match &outcome {
                Ok(result) => match result.value() {
                    TaskValue::Scalar(InstanceValue::Int32(value)) => {
                        assert!([7, 42].contains(value));
                        success = *value == 42;
                    }
                    _ => panic!("unexpected result"),
                },
                Err(error) => assert_eq!(
                    error.code(),
                    ErrorCode::ResourceLimit,
                    "{mode:?}/{bytes}: {error}"
                ),
            }
            drop(outcome);
            assert_eq!(budget.used(), 0, "{mode:?}/{bytes}");
            if success {
                break;
            }
        }
        assert!(success, "{mode:?}");
    }
}
#[test]
fn isolate_task_budget_boundaries() {
    budgets(256)
}
#[test]
#[ignore = "exhaustive: isolate task storage at every allocation budget"]
fn exhaustive_isolate_task_budgets() {
    budgets(1)
}
