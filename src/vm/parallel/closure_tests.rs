//! Owned environments retain code, ownership, and worker-local activation state.
use super::*;
use crate::runtime::{cps::CpsFrame, parallel::ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("../../../tests/fixtures/parallel_owned_closures.jk");
static THREADS: Mutex<Vec<(i32, std::thread::ThreadId)>> = Mutex::new(Vec::new());
unsafe extern "C" fn observe(frame: *mut CpsFrame) -> i32 {
    THREADS
        .lock()
        .unwrap()
        .push(((&(*frame).slots)[0] as i32, std::thread::current().id()));
    0
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "owned closure task did not progress"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn parallel_owned_closures_also_execute_on_local_frames() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let mut task = vm.call_async("main", &[]).unwrap();
        for _ in 0..10000 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished(), "{mode:?}");
        assert_eq!(task.cps_result(), Some(0));
        drop(task);
        drop(vm);
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn parallel_owned_closures_release_local_storage_on_failure_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);

        let mut source = String::from(
            "import joky/task import joky/check
             struct Tracked { text: String }
             ",
        );
        for (name, tail) in [
            ("done", ""),
            ("failed", "check.panic(\"expected failure\")"),
            ("parked", "while true { task.yield_now() }"),
        ] {
            source.push_str(&format!(
                "fn {name}() @suspends {{
                    let value = Tracked {{ text: \"owned\" }}
                    let callback = move || {{ value }}
                    task.yield_now()
                    {tail}
                }}"
            ));
        }
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        for name in ["done", "failed", "parked"] {
            let mut task = vm.call_async(name, &[]).unwrap();
            let mut failed = false;
            for _ in 0..if name == "parked" { 2 } else { 100 } {
                if task.is_finished() {
                    break;
                }
                if task.resume().is_err() {
                    failed = true;
                    break;
                }
            }
            assert_eq!(failed, name == "failed", "{mode:?}: {name}");
            assert_eq!(task.is_finished(), name != "parked", "{mode:?}: {name}");
            drop(task);
            crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn parallel_owned_closures_freeze_transfer_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        THREADS.lock().unwrap().clear();
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(17),
                ..Default::default()
            })
            .unwrap();
        }
        // SAFETY: The callback uses a mutex and retains no frame or waker.
        unsafe {
            vm.register_parallel_suspending_fn(
                "observe",
                FunctionSignature::new(vec![Type::Int32], Type::Unit),
                observe,
                1,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&SOURCE.replace(
            "fn observe(id: Int32) @suspends {}",
            "extern fn observe(id: Int32) @suspends",
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        let budget = TaskBudget::new(16 << 20);
        let success = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|e| panic!("{e}"));
        let parked = vm
            .prepare_parallel_task("parked", vec![], vec![budget.clone()])
            .unwrap();
        let failed = vm
            .prepare_parallel_task("failed", vec![], vec![budget.clone()])
            .unwrap();
        vm.load_source("fn main() @suspends -> Int32 { 99 }")
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = success.submit(&pool).unwrap();
        wait_until(|| task.is_finished());
        assert_eq!(
            task.take_result()
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"))
                .value(),
            &TaskValue::Scalar(InstanceValue::Int32(0))
        );
        drop(task);
        let threads: HashMap<_, _> = THREADS.lock().unwrap().iter().copied().collect();
        assert_ne!(
            threads[&1], threads[&2],
            "closure inputs must cross worker boundaries"
        );
        assert_ne!(threads[&1], std::thread::current().id());
        THREADS.lock().unwrap().clear();
        let task = parked.submit(&pool).unwrap();
        wait_until(|| THREADS.lock().unwrap().iter().any(|(id, _)| *id == 99));
        task.request_cancel();
        wait_until(|| task.is_finished());
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop(task);
        let task = failed.submit(&pool).unwrap();
        wait_until(|| task.is_finished());
        assert!(task.take_result().is_err());
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn parallel_owned_closures_reject_local_targets() {
    let mut vm = VmEngine::new();
    unsafe extern "C" fn local() -> i32 {
        42
    }
    unsafe {
        vm.register_fn(
            "local",
            FunctionSignature::new(vec![], Type::Int32),
            local as *const u8,
        )
        .unwrap();
    }
    vm.load_source(
        r#"
        extern fn local() -> Int32
        fn discard<F>(value: F) @suspends {}
        fn bad_target() @suspends {
            let value = "capture"
            let callback = move || { local(); value }
            scope { branch { discard(callback) } }
        }
    "#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let error = vm
        .prepare_parallel_task("bad_target", vec![], vec![])
        .err()
        .expect("must reject before executing");
    assert!(error.message().contains("local"), "{error}");
}

#[test]
fn parallel_owned_closures_preserve_borrow_and_result_boundaries() {
    for source in [
        "fn main() @suspends { let n = 1; let f = || { n }; f(); }",
        "fn main() @suspends { let n = 1; let r = &n; let f = move || { *r }; f(); }",
        "fn pass<F>(f: F) @suspends -> F { f } fn main() @suspends { let f = move || { 1 }; let (a,b) = join { pass(f), pass(move || { 2 }) }; }",
        "fn main() @suspends { let f = move || { 1 }; scope { f } }",
        "fn main() @suspends { let text = \"one\"; let f = move || { text }; scope { branch { f(); } }; f(); }",
    ] {
        crate::core::prepared::check_source(source, &crate::ModuleSources::new())
            .expect_err(source);
    }
    for (body, diagnostic) in [
        (
            "let (a, b) = join { pass(wrapped), pass(wrap(move || { 2 })) }",
            "join results",
        ),
        ("let value = scope { wrapped }", "scope results"),
        ("let value = isolate { wrapped }", "isolate results"),
    ] {
        let source = format!(
            "struct Wrapper<F> {{ callback: F }}
             fn wrap<F>(callback: F) -> Wrapper<F> {{ Wrapper {{ callback }} }}
             fn pass<F>(value: F) @suspends -> F {{ value }}
             fn main() @suspends {{
                 let wrapped = wrap(move || {{ 1 }})
                 {body}
             }}"
        );
        let error = crate::core::prepared::check_source(&source, &crate::ModuleSources::new())
            .expect_err(&source);
        assert!(error.message().contains(diagnostic), "{error}");
    }
}

#[test]
#[ignore = "exhaustive: owned closure publication and partial packet budget failures"]
fn exhaustive_parallel_owned_closure_budget_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap();
        let mut success = false;
        for bytes in (0..(1 << 20)).step_by(256) {
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|task| {
                    task.wait()?;
                    task.take_result()
                });
            success = result.is_ok();
            if let Ok(output) = &result {
                assert_eq!(output.value(), &TaskValue::Scalar(InstanceValue::Int32(0)));
            }
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} budget={bytes}");
            if success {
                break;
            }
        }
        assert!(success, "budget scan must reach a fully successful run");
    }
}
