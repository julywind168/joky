//! Admission boundaries and failure sweeps for private scoped inputs.
use super::*;
use crate::runtime::{cps::CpsFrame, parallel::ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SOURCE: &str = include_str!("../../../tests/fixtures/scoped_borrow_join.jk");
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "scoped task did not progress");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scoped_borrow_nested_groups_forward_received_views() {
    let source = format!(
        r#"{SOURCE}
        fn nested(values: &Array<Int32>) @suspends -> Int32 {{
            let (a, b) = join {{ | array_sum(values) | array_sum(values) }}
            a + b
        }}
        fn forwarding() @suspends -> Int32 {{
            let values = [1, 2]
            let (a, _) = join {{ | nested(&values) | task.yield_now() }}
            a
        }}
        fn slow(values: &Array<Int32>) @suspends -> Int32 {{
            while true {{ task.yield_now() }}
        }}
        fn local_race() @suspends -> Int32 {{
            let values = [1, 2]
            race {{ | nested(&values) | slow(&values) }}
        }}
        fn isolated() @suspends -> Int32 {{
            let values = [1, 2]
            match isolate {{ nested(&values) }} {{ Ok(value) => value, Err(_) => 0 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        for (name, remote_inputs) in [("forwarding", 1), ("local_race", 2), ("isolated", 2)] {
            let budget = TaskBudget::new(4 << 20);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let task = vm
                .prepare_parallel_task(name, vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            wait(|| task.is_finished());
            assert_eq!(
                task.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::Int32(6))
            );
            let observations = crate::runtime::parallel::scoped::observations(task.identity());
            assert_eq!(
                observations.iter().filter(|o| o.source).count(),
                remote_inputs,
                "{mode:?} {name}"
            );
            let forwarded: Vec<_> = observations
                .iter()
                .filter(|o| !o.source)
                .flat_map(|child| crate::runtime::parallel::scoped::observations(child.task))
                .collect();
            if name != "isolated" {
                assert_eq!(
                    forwarded.iter().filter(|o| o.source).count(),
                    2,
                    "{mode:?} {name}"
                );
                let original = observations.iter().find(|o| o.source).unwrap();
                for input in &forwarded {
                    assert_eq!(
                        (input.origin_parent, input.origin_scope),
                        (original.origin_parent, original.origin_scope)
                    );
                    assert_eq!((input.data, input.len), (original.data, original.len));
                }
            }
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
        }
        for name in ["array_sum", "slice_sum", "text_size"] {
            assert!(vm
                .parallel_signature(name)
                .unwrap_err()
                .message()
                .contains("unsupported packet types"));
        }
    }
}

fn budgets(amounts: impl Clone + IntoIterator<Item = usize>) {
    unsafe extern "C" fn cleanups(frame: *mut CpsFrame) -> i32 {
        (*frame).result = crate::runtime::parallel::pool::cleanup_count() as u64;
        0
    }
    let source = format!(
        r#"{SOURCE}
        extern fn cleanups() @suspends -> Int32
        struct Input {{ text: String }}
        impl Drop for Input {{ fn drop(&mut self) {{}} }}
        fn with_input(text: &String, input: Input) @suspends -> Int32 {{ text_size(text) }}
        fn isolated_inputs() @suspends -> Int32 {{
            let result = isolate {{
                let text = "source"
                let (a, b) = join {{
                    | with_input(&text, Input {{ text: "first" }})
                    | with_input(&text, Input {{ text: "second" }})
                }}
                a + b
            }}
            if cleanups() != 0 {{ return -2 }}
            match result {{ Ok(value) => value, Err(_) => -1 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "cleanups",
                FunctionSignature::new(vec![], Type::Int32),
                cleanups,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&source).unwrap();
        for (name, expected) in [
            ("isolated_inputs", InstanceValue::Int32(12)),
            ("advanced", InstanceValue::UInt64(17)),
            ("forwarded", InstanceValue::Int32(145)),
            ("forwarded_race", InstanceValue::Int32(94)),
            ("forwarded_isolate", InstanceValue::Int32(94)),
            ("nested_race", InstanceValue::Int32(91)),
            ("borrowed_race", InstanceValue::Int32(68)),
            ("nested_isolated_success", InstanceValue::Int32(91)),
        ] {
            let mut success = false;
            for bytes in amounts.clone() {
                let budget = TaskBudget::new(bytes);
                let mut pool = ParallelExecutor::new(3, 32).unwrap();
                let result = vm
                    .prepare_parallel_task(name, vec![], vec![budget.clone()])
                    .and_then(|p| p.submit(&pool))
                    .and_then(|task| {
                        wait(|| task.is_finished());
                        let result = task.take_result();
                        if name == "isolated_inputs"
                            && result
                                .as_ref()
                                .is_ok_and(|value| value.value() == &TaskValue::Scalar(expected))
                        {
                            assert_eq!(
                                crate::runtime::parallel::scoped::observations(task.identity())
                                    .iter()
                                    .filter(|o| o.source)
                                    .count(),
                                2,
                                "mixed portable inputs stayed local"
                            );
                        }
                        result
                    });
                match &result {
                    Ok(value) => {
                        success = value.value() == &TaskValue::Scalar(expected);
                        if !success {
                            assert!(matches!(
                                name,
                                "nested_isolated_success" | "isolated_inputs" | "forwarded_isolate"
                            ));
                            assert_eq!(
                                value.value(),
                                &TaskValue::Scalar(InstanceValue::Int32(-1)),
                                "{mode:?} {name} {bytes}: recovery preceded portable input cleanup"
                            );
                        }
                    }
                    Err(error) => assert_eq!(
                        error.code(),
                        ErrorCode::ResourceLimit,
                        "{mode:?} {name} {bytes}: {error}"
                    ),
                }
                drop(result);
                pool.wait_closed();
                assert_eq!(budget.used(), 0, "{mode:?} {name} {bytes}");
                if success {
                    break;
                }
            }
            assert!(
                success,
                "{mode:?} {name}: sweep did not reach successful readers"
            );
        }
    }
}
#[test]
fn scoped_borrow_budget_boundaries() {
    budgets([0, 512, 4096, 16384, 65536, 1 << 20]);
}
#[test]
#[ignore = "exhaustive: scoped descriptors, view installation, mixed owners and results"]
fn exhaustive_scoped_borrow_budgets() {
    budgets((0..(1 << 20)).step_by(128));
}

#[test]
#[ignore = "exhaustive: cancel at each source preparation and suspended reader checkpoint"]
fn exhaustive_scoped_borrow_cancellation_checkpoints() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static TARGET: AtomicUsize = AtomicUsize::new(0);
    static PARKED: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn checkpoint(frame: *mut CpsFrame) -> i32 {
        if (*frame).state == 1 {
            return 1;
        }
        if COUNT.fetch_add(1, Ordering::SeqCst) == TARGET.load(Ordering::SeqCst) {
            (*frame).state = 1;
            PARKED.store(true, Ordering::SeqCst);
            return 1;
        }
        0
    }
    let source = SOURCE
        .replace("task.yield_now()", "checkpoint()")
        .replace("    let (whole,", "    checkpoint()\n    let (whole,")
        .replace("    values[0] = 42", "    checkpoint()\n    values[0] = 42");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_parallel_suspending_fn(
                "checkpoint",
                FunctionSignature::new(vec![], Type::Unit),
                checkpoint,
                0,
                Arc::new(()),
            )
            .unwrap();
        }
        vm.load_source(&format!("{source}\nextern fn checkpoint() @suspends"))
            .unwrap();
        for (name, expected) in [
            ("main", 104),
            ("nested_isolated_success", 91),
            ("forwarded", 145),
        ] {
            let mut complete = false;
            for boundary in 0..64 {
                COUNT.store(0, Ordering::SeqCst);
                TARGET.store(boundary, Ordering::SeqCst);
                PARKED.store(false, Ordering::SeqCst);
                let budget = TaskBudget::new(4 << 20);
                let mut pool = ParallelExecutor::new(3, 32).unwrap();
                let task = vm
                    .prepare_parallel_task(name, vec![], vec![budget.clone()])
                    .unwrap()
                    .submit(&pool)
                    .unwrap();
                wait(|| PARKED.load(Ordering::SeqCst) || task.is_finished());
                complete = !PARKED.load(Ordering::SeqCst);
                if !complete {
                    task.request_cancel();
                }
                wait(|| task.is_finished());
                let result = task.take_result();
                if complete {
                    assert_eq!(
                        result.as_ref().unwrap().value(),
                        &TaskValue::Scalar(InstanceValue::Int32(expected))
                    );
                } else {
                    let error = result.as_ref().err().unwrap();
                    assert_eq!(
                        error.code(),
                        ErrorCode::Cancelled,
                        "{mode:?} {name} checkpoint {boundary}: {error}"
                    );
                }
                drop((result, task));
                pool.wait_closed();
                assert_eq!(budget.used(), 0, "{mode:?} {name} checkpoint {boundary}");
                if complete {
                    break;
                }
            }
            assert!(complete);
        }
    }
}

#[test]
fn scoped_borrow_isolate_does_not_hide_fatal_source_cleanup() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn fatal_drop(_: *const u8) -> i32 {
        crate::runtime::error::record_error(Error::new_with_code(
            ErrorPhase::Abi,
            ErrorCode::AbiError,
            "fatal source cleanup",
        ));
        0
    }
    unsafe extern "C" fn reader(_: *mut CpsFrame) -> i32 {
        STARTED.store(true, Ordering::Release);
        1
    }
    unsafe extern "C" fn fail(_: *mut CpsFrame) -> i32 {
        if !STARTED.load(Ordering::Acquire) {
            return 1;
        }
        crate::runtime::error::record_error(Error::new_with_code(
            ErrorPhase::Runtime,
            ErrorCode::RuntimeError,
            "recoverable sibling failure",
        ));
        3
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        STARTED.store(false, Ordering::Release);
        let mut vm = VmEngine::with_mode(mode);
        for (name, callback) in [
            ("reader", reader as crate::runtime::cps::CpsEntry),
            ("fail", fail),
        ] {
            unsafe {
                vm.register_parallel_suspending_fn(
                    name,
                    FunctionSignature::new(vec![], Type::Unit),
                    callback,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
        }
        let host = Host {
            name: "fatal_drop".into(),
            signature: FunctionSignature::new(
                vec![Type::reference(Type::String, false)],
                Type::Int32,
            ),
            code: crate::runtime::parallel::HostCode::TextView(fatal_drop),
            _owner: Arc::new(()),
        };
        unsafe {
            vm.register_owned_text_fn(
                "fatal_drop",
                host.signature.clone(),
                fatal_drop as *const u8,
                Some(std::rc::Rc::new(host)),
            )
            .unwrap();
        }
        vm.load_source(
            r#"
            import joky/task
            import joky/drop.{type Drop}
            extern fn fatal_drop(text: &String) -> Int32
            struct Source { text: String }
            impl Drop for Source { fn drop(&mut self) { fatal_drop(&self.text); } }
            extern fn reader() @suspends
            extern fn fail() @suspends
            fn borrowed(text: &String) @suspends { reader() }
            fn sibling(text: &String) @suspends { fail() }
            fn root() @suspends -> Int32 {
                let result = isolate {
                    let source = Source { text: "source" }
                    join { | borrowed(&source.text) | sibling(&source.text) }
                }
                99
            }
        "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let budget = TaskBudget::new(4 << 20);
        let mut pool = ParallelExecutor::new(3, 16).unwrap();
        let task = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(|| task.is_finished());
        let error = task
            .take_result()
            .err()
            .unwrap_or_else(|| panic!("{mode:?}: fatal cleanup was recovered"));
        assert_eq!(error.code(), ErrorCode::AbiError, "{mode:?}: {error}");
        assert_eq!(
            crate::runtime::parallel::scoped::observations(task.identity())
                .iter()
                .filter(|o| o.source)
                .count(),
            2
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}
