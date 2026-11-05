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

#[test]
fn parallel_reservation_sample() {
    let source = format!(
        "{}\nfn main() @suspends {{demo({{}})}}",
        include_str!("../../../examples/reservation_logic.jk")
    );
    for (mode, inner) in [(VmMode::Jit, "new"), (VmMode::Interpreter, "interpreter")] {
        run(&source, mode, inner);
    }
}

#[test]
fn parallel_domains_optional_paths_and_owning_variants() {
    for (mode, inner) in [(VmMode::Jit, "new"), (VmMode::Interpreter, "interpreter")] {
        run(
            include_str!("../../../tests/fixtures/state_domain_optional.jk"),
            mode,
            inner,
        );
    }
}

#[test]
fn direct_parallel_single_domain_survives_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../../tests/fixtures/parallel_single_domain.jk"
        ))
        .unwrap();
        let budget = TaskBudget::new(64 << 20);
        let root = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 128).unwrap();
        let task = root.submit(&pool).unwrap();
        task.wait().unwrap();
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Unit)
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn direct_parallel_domains_reject_dynamic_paths_and_periodic_entries() {
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(
        r#"
        import joky/domain
        #state
        struct Stock { count: Int32 }
        #state
        struct Clock { ticks: Int32 }
        impl Clock { #every(1s) fn tick(&mut self) { self.ticks += 1 } }
        fn clock(n: Int32) -> Clock { Clock { ticks: n } }
        fn path() @suspends {
            let directory = domain.directory(1)
            let routes = directory.handle()
            when (stock: Stock = routes[1]) { stock.count }!;
        }
        fn periodic() @suspends { domain.create(clock, 0).close() }
    "#,
    )
    .unwrap();
    for entry in ["path", "periodic"] {
        let error = vm
            .prepare_parallel_task(entry, vec![], vec![])
            .err()
            .expect("Local entry");
        assert!(
            error.message().contains("state domain operation is Local"),
            "{error}"
        );
    }
}
