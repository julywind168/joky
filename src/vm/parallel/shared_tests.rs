use super::*;
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

const SOURCE: &str = include_str!("../../../tests/fixtures/shared_values.jk");

#[test]
fn shared_values_execute_and_release_in_local_and_parallel_tasks() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
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
        let source = SOURCE
            .replace("fn main()", "extern fn touched() @suspends\nfn main()")
            .replace(
                "let config = value.get()",
                "touched(); let config = value.get()",
            )
            .replace("task.sleep(60s)", "touched(); task.sleep(60s)");
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{error}"));
        {
            let mut task = vm
                .call_async("main", &[])
                .unwrap_or_else(|error| panic!("{error}"));
            while !task.is_finished() {
                task.resume().unwrap_or_else(|error| match error {
                    crate::runtime::suspension::ResumeError::Failed(error) => panic!("{error}"),
                    error => panic!("{error:?}"),
                });
            }
            assert_eq!(task.cps_result(), Some(0), "{mode:?}");
        }
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        THREADS.lock().unwrap().clear();
        let budget = TaskBudget::new(1 << 20);
        let prepared = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap();
        let cancelled = vm
            .prepare_parallel_task("suspended", vec![], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "shared task did not finish: {mode:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        task.wait().unwrap_or_else(|error| panic!("{error}"));
        let result = task.take_result().unwrap();
        assert_eq!(
            result.value(),
            &TaskValue::Scalar(InstanceValue::Int32(0)),
            "{mode:?}"
        );
        drop((task, result));
        let threads: HashSet<_> = THREADS.lock().unwrap().drain(..).collect();
        assert!(
            threads.len() >= 2,
            "shared payload must reach distinct workers"
        );
        assert!(!threads.contains(&std::thread::current().id()));
        let task = cancelled.submit(&pool).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while THREADS.lock().unwrap().len() < 2 {
            assert!(
                Instant::now() < deadline,
                "shared children did not start: {mode:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        task.request_cancel();
        while !task.is_finished() {
            assert!(
                Instant::now() < deadline,
                "shared cancellation did not finish: {mode:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().err().unwrap().code(),
            crate::ErrorCode::Cancelled
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn shared_values_reject_mutation_escaping_borrows_and_unsupported_payloads() {
    for source in [
        "fn bad(value: &Int32) { let shared = Shared.new(value) }",
        "fn bad(value: Ptr) { let shared = Shared.new(value) }",
        "fn bad(value: Resource) { let shared = Shared.new(value) }",
        "fn bad(value: &Int32) { let shared = Shared.new([value]) }",
        "fn bad() { let n = 2; let shared = Shared.new(|x: Int32| { x + n }) }",
        "fn bad() { let shared = Shared.new(Shared.new(1)) }",
        "fn bad() { let shared = Shared.new([1]); shared.get().push(2) }",
        "fn bad() -> &Int32 { let shared = Shared.new(1); shared.get() }",
        "fn bad() -> Int32 { let shared = Shared.new(1); let view = shared.get(); mem.drop(shared); *view }",
        "fn bad() { let shared = Shared.new(1); let moved = shared; let again = shared.share() }",
    ] {
        let mut vm = VmEngine::with_mode(VmMode::Interpreter);
        let source = format!("import joky/shared.{{type Shared}} import joky/mem {source}");
        let error = vm.load_source(&source).expect_err(&source);
        assert_eq!(error.phase(), crate::ErrorPhase::TypeCheck, "{error}");
        assert!(error.span().is_some(), "{error}");
    }
}

#[test]
fn shared_values_cannot_escape_through_host_task_values() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("import joky/shared.{type Shared} fn exported() @suspends -> Shared<Int32> { Shared.new(42) } struct State { value: Shared<Int32> } fn create() -> State { State { value: Shared.new(42) } }")
            .unwrap_or_else(|error| panic!("{error}"));
        let error = vm
            .prepare_parallel_task("exported", vec![], vec![])
            .err()
            .unwrap();
        assert!(error.to_string().contains("Host task values"), "{error}");
        assert!(vm.create_instance("create", &[]).is_err());
        assert_eq!(vm.instance_bytes(), 0);
    }
}

#[test]
#[ignore = "exhaustive: shared payload, handle and task packet budget failures"]
fn exhaustive_shared_task_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            import joky/shared.{type Shared}
            import joky/task
            import joky/mem
            fn child(value: Shared<Array<String>>) @suspends -> Shared<Array<String>> {
                task.yield_now()
                let copy = value.share()
                mem.drop(value)
                copy
            }
            fn main() @suspends -> Int32 {
                let value = Shared.new(["payload"])
                let first = value.share()
                let (a, b) = join { child(first), child(value) }
                assert(a.get().borrow_ref(0) == "payload", "first child result")
                assert(b.get().borrow_ref(0) == "payload", "second child result")
                0
            }
        "#,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let mut completed = false;
        for bytes in (0..=262_144).step_by(64) {
            let budget = TaskBudget::new(bytes);
            let pool = crate::runtime::parallel::ParallelExecutor::new(2, 32).unwrap();
            let outcome = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|prepared| prepared.submit(&pool))
                .and_then(|task| {
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while !task.is_finished() {
                        assert!(
                            Instant::now() < deadline,
                            "shared budget task did not finish"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    task.take_result()
                });
            if let Err(error) = &outcome {
                assert_eq!(
                    error.code(),
                    crate::ErrorCode::ResourceLimit,
                    "{mode:?}, bytes={bytes}: {error}"
                );
            } else {
                assert_eq!(
                    outcome.as_ref().unwrap().value(),
                    &TaskValue::Scalar(InstanceValue::Int32(0))
                );
                completed = true;
            }
            drop((outcome, pool));
            assert_eq!(budget.used(), 0, "{mode:?}, bytes={bytes}");
            if completed {
                break;
            }
        }
        assert!(completed, "shared budget sweep never completed: {mode:?}");
    }
}
