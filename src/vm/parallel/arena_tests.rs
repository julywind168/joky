//! Explicit regions remain worker-local while sharing the tree's budget.
use super::*;
use crate::runtime::parallel::{HostCode, ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("../../../tests/fixtures/parallel_explicit_arena.jk");
static OBSERVED: Mutex<Vec<(String, std::thread::ThreadId)>> = Mutex::new(Vec::new());
static SERIAL: Mutex<()> = Mutex::new(());
unsafe extern "C" fn observe(text: *const u8) -> i32 {
    let text = crate::runtime::string::borrow(text).unwrap();
    OBSERVED
        .lock()
        .unwrap()
        .push((text.to_owned(), std::thread::current().id()));
    0
}
fn engine(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    if mode == VmMode::Interpreter {
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(17),
            ..Default::default()
        })
        .unwrap();
    }
    let host = Host {
        name: "observe".into(),
        signature: FunctionSignature::new(vec![Type::reference(Type::String, false)], Type::Int32),
        code: HostCode::TextView(observe),
        _owner: Arc::new(()),
    };
    unsafe {
        vm.register_owned_text_fn(
            "observe",
            host.signature.clone(),
            observe as *const u8,
            Some(Rc::new(host)),
        )
        .unwrap();
    }
    vm.load_source(&SOURCE.replace(
        "fn observe(text: &String) -> Int32 { 0 }",
        "extern fn observe(text: &String) -> Int32",
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "worker arena task did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn parallel_explicit_arena_executes_fails_and_cancels_on_owning_workers() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        for workers in [1, 3] {
            OBSERVED.lock().unwrap().clear();
            let budget = TaskBudget::new(1 << 20);
            let mut pool = ParallelExecutor::new(workers, 32).unwrap();
            for (entry, expected) in [
                ("main", None),
                ("failed", Some(ErrorCode::RuntimeError)),
                ("parked", Some(ErrorCode::Cancelled)),
            ] {
                let task = vm
                    .prepare_parallel_task(entry, vec![], vec![budget.clone()])
                    .unwrap_or_else(|e| panic!("{mode:?}/{entry}: {e}"))
                    .submit(&pool)
                    .unwrap();
                if entry == "parked" {
                    wait(|| OBSERVED.lock().unwrap().iter().any(|(s, _)| s == "ready"));
                    task.request_cancel();
                }
                wait(|| task.is_finished());
                let result = task.take_result();
                if let Some(code) = expected {
                    assert_eq!(result.err().unwrap().code(), code, "{mode:?}/{entry}");
                } else {
                    assert_eq!(
                        result.unwrap_or_else(|e| panic!("{mode:?}: {e}")).value(),
                        &TaskValue::Scalar(InstanceValue::Int32(0))
                    );
                }
                drop(task);
            }
            let events = OBSERVED.lock().unwrap();
            for name in ["left", "right"] {
                let ids: Vec<_> = events
                    .iter()
                    .filter(|(s, _)| s == name)
                    .map(|(_, t)| *t)
                    .collect();
                assert_eq!(ids.len(), 2);
                assert_eq!(
                    ids[0], ids[1],
                    "a worker retains its regions across interpreter slices"
                );
                assert_ne!(ids[0], std::thread::current().id());
            }
            drop(events);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{workers}");
        }
        // Prepared code remains usable after the source module is replaced.
        let prepared = vm.prepare_parallel_task("main", vec![], vec![]).unwrap();
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(1, 32).unwrap();
        let task = prepared.submit(&pool).unwrap();
        wait(|| task.is_finished());
        task.take_result().unwrap();
        drop(task);
        pool.wait_closed();
    }
}

#[test]
fn parallel_explicit_arena_keeps_host_packets_and_local_effects_rejected() {
    let mut vm = VmEngine::new();
    unsafe extern "C" fn local() -> i32 {
        1
    }
    unsafe {
        vm.register_fn(
            "local",
            FunctionSignature::new(vec![], Type::Int32),
            local as *const u8,
        )
        .unwrap();
    }
    vm.load_source("import joky/arena.{type Arena} extern fn local() -> Int32 fn helper(arena: &Arena) -> Int32 { let p = arena.alloc(1); *p } fn bad_helper() -> Int32 { let arena = Arena.new(); let p = arena.alloc(local()); *p } fn bad() @suspends -> Int32 { bad_helper() }").unwrap();
    assert!(vm
        .parallel_signature("helper")
        .unwrap_err()
        .message()
        .contains("unsupported packet types"));
    assert!(vm
        .prepare_parallel_task("bad", vec![], vec![])
        .err()
        .unwrap()
        .message()
        .contains("local"));
}

#[test]
#[ignore = "exhaustive: worker-local explicit region creation, allocation and cleanup budgets"]
fn exhaustive_parallel_explicit_arena_budgets() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = engine(mode);
        let mut success = false;
        for bytes in (0..(1 << 20)).step_by(512) {
            OBSERVED.lock().unwrap().clear();
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|task| {
                    wait(|| task.is_finished());
                    task.take_result()
                });
            success = result.is_ok();
            drop(result);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?} bytes={bytes}");
            if success {
                break;
            }
        }
        assert!(success);
    }
}
