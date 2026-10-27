use super::*;
use crate::runtime::{cps::CpsFrame, parallel::ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};

static OBSERVATIONS: Mutex<Vec<(i32, std::thread::ThreadId)>> = Mutex::new(Vec::new());
unsafe extern "C" fn observe(frame: *mut CpsFrame) -> i32 {
    OBSERVATIONS
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
            "borrowed iteration did not progress"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn parallel_borrowed_iteration_executes_and_cancels_on_owning_workers() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        OBSERVATIONS.lock().unwrap().clear();
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(17),
                ..Default::default()
            })
            .unwrap();
        }
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
        let source = include_str!("../../../tests/fixtures/parallel_borrowed_iteration.jk")
            .replace(
                "fn observe(id: Int32) @suspends {}",
                "extern fn observe(id: Int32) @suspends",
            );
        vm.load_source(&source)
            .unwrap_or_else(|error| panic!("{error}"));
        let budget = TaskBudget::new(16 << 20);
        let success = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|error| panic!("{error}"));
        let parked = vm
            .prepare_parallel_task("parked", vec![], vec![budget.clone()])
            .unwrap_or_else(|error| panic!("{error}"));
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = success.submit(&pool).unwrap();
        wait_until(|| task.is_finished());
        assert_eq!(
            task.take_result()
                .unwrap_or_else(|error| panic!("{error}"))
                .value(),
            &TaskValue::Scalar(InstanceValue::Int32(0))
        );
        drop(task);
        let mut threads = HashMap::<i32, HashSet<_>>::new();
        for &(id, thread) in OBSERVATIONS.lock().unwrap().iter() {
            threads.entry(id).or_default().insert(thread);
        }
        assert_eq!(
            threads[&1].len(),
            1,
            "borrowed join must stay on the cursor's worker"
        );
        assert_eq!(threads[&2].len(), 1);
        assert_ne!(
            threads[&1], threads[&2],
            "owned branches must still distribute"
        );
        let task = parked.submit(&pool).unwrap();
        wait_until(|| {
            let seen = OBSERVATIONS.lock().unwrap();
            seen.iter().any(|(id, _)| *id == 99) && seen.iter().any(|(id, _)| *id == 100)
        });
        task.request_cancel();
        wait_until(|| task.is_finished());
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn parallel_borrowed_iteration_rejects_packets_and_local_effects() {
    let mut vm = VmEngine::new();
    unsafe extern "C" fn local(_: *mut CpsFrame) -> i32 {
        0
    }
    unsafe {
        vm.register_suspending_fn("local", FunctionSignature::new(vec![], Type::Unit), local)
            .unwrap();
    }
    vm.load_source(r#"
        import joky/array.{type Array}
        import joky/iter
        import joky/map.{type Map}
        extern fn local() @suspends
        fn cursor(value: &iter.Iter<Int32>) @suspends { for item in value {} }
        fn pair(value: &map.Iter<Int32, Int32>) @suspends { for entry in value {} }
        fn entry(value: &map.Entry<Int32, Int32>) @suspends { map.entry_value(value); }
        fn capture_cursor() @suspends { let values = [1]; let view = values.iter(); scope { branch { cursor(view) } } }
        fn capture_pair() @suspends { var values = Map.new::<Int32, Int32>(); let view = values.iter(); scope { branch { pair(view) } } }
        fn capture_entry() @suspends { var values = Map.new::<Int32, Int32>(); values.insert(1, 2); let view = values.iter(); map.advance(view); let item = map.current(view); scope { branch { entry(item) } } }
        fn bad_effect() @suspends { let values = [1]; for value in &values { local() } }
    "#).unwrap_or_else(|error| panic!("{error}"));
    for (name, reason) in [
        ("cursor", "unsupported packet types"),
        ("pair", "unsupported packet types"),
        ("entry", "unsupported packet types"),
        ("capture_cursor", "parallel branch captures"),
        ("capture_pair", "parallel branch captures"),
        ("capture_entry", "parallel branch captures"),
        ("bad_effect", "ParallelSafe"),
    ] {
        let error = vm.parallel_signature(name).unwrap_err();
        assert!(error.message().contains(reason), "{name}: {error}");
    }
}
