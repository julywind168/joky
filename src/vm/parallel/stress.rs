//! Production pool ownership boundaries, separate from Local instruction scans.
use super::*;
const OWNERS: &str = r#"
    import joky/task
    fn echo(text: String) @suspends -> String { task.yield_now(); text }
    fn pair(text: String) @suspends -> String {
        let (a, b) = join { echo(text), echo("other owner") }
        a
    }
    fn root(text: String) @suspends -> String { race { pair(text), pair("other winner") } }
    fn consume(text: String) { }
    fn bad(text: String) @suspends -> String { consume(text); task.yield_now(); 1 / 0; "bad" }
    fn failed(text: String) @suspends -> String { let (a, b) = join { bad(text), pair("loser") }; a }
    fn parked(text: String) @suspends -> String { task.sleep_ms(60000); text }
    fn cancelled(text: String) @suspends -> String { scope { branch { consume(pair("child")) } }; parked(text) }
"#;
fn attempt(vm: &VmEngine, entry: &str, workers: usize, bytes: usize, cancel: bool) -> bool {
    let budget = TaskBudget::new(bytes);
    let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
    let result = vm
        .prepare_parallel_task(
            entry,
            vec![TaskValue::String("input".repeat(32))],
            vec![budget.clone()],
        )
        .and_then(|prepared| prepared.submit(&pool))
        .and_then(|task| {
            if cancel {
                task.request_cancel();
            }
            task.wait().unwrap();
            let result = task.take_result();
            assert!(
                task.take_result().is_err(),
                "terminal packet can only be collected once"
            );
            result
        });
    let success = result.is_ok();
    if let Ok(output) = &result {
        let TaskValue::String(text) = output.value() else {
            panic!("expected owning text")
        };
        assert!(text == &"input".repeat(32) || text == "other winner");
    }
    if cancel {
        assert!(!success);
    }
    drop(result);
    pool.wait_closed();
    drop(pool);
    assert_eq!(budget.used(), 0, "{entry}, {workers}, {bytes}");
    success
}
#[test]
fn task_pool_owning_results_failure_and_cancellation_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(OWNERS).unwrap();
        for workers in [1, 3] {
            assert!(!attempt(&vm, "root", workers, 512, false));
            assert!(attempt(&vm, "root", workers, 1 << 20, false));
            assert!(!attempt(&vm, "failed", workers, 1 << 20, false));
            attempt(&vm, "cancelled", workers, 1 << 20, true);
        }
    }
}
#[test]
#[ignore = "exhaustive: production parallel capture, result and child budget failures"]
fn exhaustive_task_pool_owning_publication_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(OWNERS).unwrap();
        let mut completed = false;
        for bytes in (0..262144).step_by(256) {
            if attempt(&vm, "root", 2, bytes, false) {
                completed = true;
                break;
            }
        }
        assert!(
            completed,
            "budget scan must reach a complete successful execution"
        );
    }
}

#[test]
fn task_pool_child_failure_preserves_source_after_vm_retirement() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_named_source(
            "parallel-error.jk",
            r#"
            fn bad() @suspends { 1 / 0; }
            fn root() @suspends { scope { branch { bad() } } }
        "#,
        )
        .unwrap();
        let prepared = vm.prepare_parallel_task("root", vec![], vec![]).unwrap();
        drop(vm);
        let pool = crate::runtime::parallel::ParallelExecutor::new(2, 16).unwrap();
        let task = prepared.submit(&pool).unwrap();
        task.wait().unwrap();
        let error = task.take_result().err().unwrap();
        assert_eq!(error.message(), "Division by zero");
        assert!(error
            .sources()
            .files()
            .any(|f| f.name() == "parallel-error.jk"));
    }
}
