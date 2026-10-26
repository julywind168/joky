use super::*;
use crate::{
    runtime::parallel::ParallelExecutor,
    vm::{VmEngine, VmMode},
};
use std::time::{Duration, Instant};

fn wrapper(source: &str, inner: &str) -> String {
    let source = format!("{source}\nfn entry(_:Unit) @suspends {{ main() }}");
    let escaped = source
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('{', "{{")
        .replace('}', "}}");
    format!("import joky/vm\nfn main() @suspends ->Int32 {{let engine=vm.Vm.{inner}(); engine.load_source(\"{escaped}\")!; engine.call::<Unit,Unit>(\"entry\",{{}}); 0}}")
}
fn run(source: &str, mode: VmMode, inner: &str) {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(&wrapper(source, inner)).unwrap();
    let budget = TaskBudget::new(64 << 20);
    let prepared = vm
        .prepare_parallel_task("main", vec![], vec![budget.clone()])
        .unwrap();
    let mut pool = ParallelExecutor::new(3, 128).unwrap();
    let task = prepared.submit(&pool).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !task.is_finished() {
        assert!(
            Instant::now() < deadline,
            "domain task failed to drain: {mode:?}/{inner}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        task.take_result()
            .unwrap_or_else(|error| panic!("{mode:?}/{inner}: {error}"))
            .value(),
        &TaskValue::Scalar(InstanceValue::Int32(0)),
        "{mode:?}/{inner}"
    );
    drop(task);
    pool.wait_closed();
    assert_eq!(budget.used(), 0);
}
#[test]
fn parallel_domains_transactions_paths_and_creation() {
    for (mode, inner) in [(VmMode::Jit, "new"), (VmMode::Interpreter, "interpreter")] {
        for source in [
            include_str!("../../../tests/fixtures/state_domain_when.jk"),
            include_str!("../../../tests/fixtures/state_domain_paths.jk"),
            include_str!("../../../tests/fixtures/state_domain_creation.jk"),
        ] {
            run(source, mode, inner);
        }
    }
}
