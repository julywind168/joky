//! Owning values retain storage until readers stop and the task tree closes.
use super::*;
use crate::runtime::parallel::{HostCode, ParallelExecutor};
use crate::vm::{VmEngine, VmMode};
use std::{
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};
const SOURCE: &str = include_str!("../../../tests/fixtures/parallel_drop_values.jk");
static OBSERVED: Mutex<Vec<(String, std::thread::ThreadId)>> = Mutex::new(Vec::new());
static SERIAL: Mutex<()> = Mutex::new(());
unsafe extern "C" fn trace(text: *const u8) -> i32 {
    let text = crate::runtime::string::borrow(text).unwrap();
    OBSERVED
        .lock()
        .unwrap()
        .push((text.to_owned(), std::thread::current().id()));
    0
}
fn engine(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    let host = Host {
        name: "trace".into(),
        signature: FunctionSignature::new(vec![Type::reference(Type::String, false)], Type::Int32),
        code: HostCode::TextView(trace),
        _owner: Arc::new(()),
    };
    unsafe {
        vm.register_owned_text_fn(
            "trace",
            host.signature.clone(),
            trace as *const u8,
            Some(Rc::new(host)),
        )
        .unwrap();
    }
    vm.load_source(&SOURCE.replace(
        "fn trace(text: &String) -> Int32 { println(text); 0 }",
        "extern fn trace(text: &String) -> Int32",
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "owning task did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scoped_borrow_failed_worker_root_defers_release_until_children_stop() {
    use std::sync::{Condvar, OnceLock};
    static GATE: OnceLock<(Mutex<(bool, bool)>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn hold(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (state, wake) = GATE.get().unwrap();
        let mut state = state.lock().unwrap();
        state.0 = true;
        wake.notify_all();
        let (state, timeout) = wake
            .wait_timeout_while(state, Duration::from_secs(15), |s| !s.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            crate::runtime::error::record_error(Error::new(
                ErrorPhase::Runtime,
                "Reader release timed out",
            ));
            return 3;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("reader stopped".into(), std::thread::current().id()));
        0
    }
    unsafe extern "C" fn fail(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        if !GATE.get().unwrap().0.lock().unwrap().0 {
            return 1;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("parent failed".into(), std::thread::current().id()));
        crate::runtime::error::record_error(Error::new(
            ErrorPhase::Runtime,
            "Parent probe failure",
        ));
        3
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            let (state, wake) = GATE.get().unwrap();
            state.lock().unwrap().1 = true;
            wake.notify_all();
        }
    }
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        *GATE
            .get_or_init(|| (Mutex::new((false, false)), Condvar::new()))
            .0
            .lock()
            .unwrap() = (false, false);
        OBSERVED.lock().unwrap().clear();
        let mut vm = engine(mode);
        for (name, entry) in [
            ("hold_reader", hold as crate::runtime::cps::CpsEntry),
            ("fail_parent", fail),
        ] {
            unsafe {
                vm.register_parallel_suspending_fn(
                    name,
                    FunctionSignature::new(vec![], Type::Unit),
                    entry,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
        }
        vm.load_source(
            r#"
            extern fn trace(text: &String) -> Int32
            extern fn hold_reader() @suspends
            extern fn fail_parent() @suspends
            struct Source { text: String }

            fn root() @suspends {
                let source = Source { text: "source destroyed" }
                scope { branch { hold_reader() }; fail_parent() }
            }
        "#,
        )
        .unwrap();
        let budget = TaskBudget::new(4 << 20);
        let prepared = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 16).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let release = Release;
        // Stopping is observed after Job::poll has returned the error. Merely
        // seeing the failing callback would race the generated unwind itself.
        wait(|| task.handle.stopping());
        let before_release = OBSERVED.lock().unwrap().clone();
        assert_eq!(
            before_release
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
            ["parent failed"],
            "{mode:?}: early source destruction"
        );
        drop(release);
        task.wait().unwrap();
        assert!(task
            .take_result()
            .err()
            .unwrap()
            .message()
            .contains("Parent probe failure"));
        drop(task);
        pool.wait_closed();
        let events = OBSERVED.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["parent failed", "reader stopped"],
            "{mode:?}"
        );
        assert_ne!(events[0].1, events[1].1);
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn scoped_borrow_readers_drain_before_source_drop_on_failure_cancel_and_close() {
    use std::sync::{Condvar, OnceLock};
    static GATE: OnceLock<(Mutex<(bool, bool)>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn hold(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (state, wake) = GATE.get().unwrap();
        let mut state = state.lock().unwrap();
        state.0 = true;
        wake.notify_all();
        let (state, timeout) = wake
            .wait_timeout_while(state, Duration::from_secs(15), |s| !s.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            crate::runtime::error::record_error(Error::new(
                ErrorPhase::Runtime,
                "Reader release timed out",
            ));
            return 3;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("reader stopped".into(), std::thread::current().id()));
        0
    }
    unsafe extern "C" fn fail(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        if !GATE.get().unwrap().0.lock().unwrap().0 {
            return 1;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("parent failed".into(), std::thread::current().id()));
        crate::runtime::error::record_error(Error::new(
            ErrorPhase::Runtime,
            "Parent probe failure",
        ));
        3
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            let (state, wake) = GATE.get().unwrap();
            state.lock().unwrap().1 = true;
            wake.notify_all();
        }
    }
    let _serial = SERIAL.lock().unwrap();
    for (mode, scenario) in [VmMode::Jit, VmMode::Interpreter]
        .into_iter()
        .flat_map(|mode| (0..21).map(move |scenario| (mode, scenario)))
    {
        let kind = scenario / 3;
        let dynamic = kind != 0;
        let scenario = scenario % 3;
        *GATE
            .get_or_init(|| (Mutex::new((false, false)), Condvar::new()))
            .0
            .lock()
            .unwrap() = (false, false);
        OBSERVED.lock().unwrap().clear();
        let mut vm = engine(mode);
        for (name, entry) in [
            ("hold_reader", hold as crate::runtime::cps::CpsEntry),
            ("fail_parent", fail),
        ] {
            unsafe {
                vm.register_parallel_suspending_fn(
                    name,
                    FunctionSignature::new(vec![], Type::Unit),
                    entry,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
        }
        let source = r#"
            import joky/string
            import joky/task
            extern fn trace(text: &String) -> Int32
            extern fn hold_reader() @suspends
            extern fn fail_parent() @suspends
            struct Source { text: String }

            fn reader(text: &String) @suspends -> UInt64 {
                let before = string.byte_length(text)
                hold_reader()
                before + string.byte_length(text)
            }
            fn relay(text: &String) @suspends -> UInt64 {
                let (a, b) = join { reader(text), task.yield_now() }
                a
            }
            fn peer() @suspends -> UInt64 { PEER; 0 }
            fn helper(text: &String) @suspends {
                let (a, b) = join { relay(text), peer() }
            }
            fn root() @suspends {
                let source = Source { text: "source destroyed" }
                helper(&source.text)
            }
        "#
        .replace("PEER", if scenario == 0 { "fail_parent()" } else { "{}" });
        let source = if dynamic {
            source.replace(
                "let (a, b) = join { relay(text), peer() }",
                "scope { branch { relay(text); }; peer(); }",
            )
        } else {
            source
        };
        // Static groups keep text coverage; dynamic captures exercise padded
        // aggregate buffers through the same failure/cancel/close lifetime fence.
        let source = if kind == 1 {
            source
                .replace("import joky/string", "import joky/array.{type Array}")
                .replace(
                    "struct Source { text: String }",
                    "struct Source { text: String, values: Array<(Int32, Float64)> }",
                )
                .replace(
                    "fn reader(text: &String)",
                    "fn reader(text: &Array<(Int32, Float64)>)",
                )
                .replace(
                    "fn relay(text: &String)",
                    "fn relay(text: &Array<(Int32, Float64)>)",
                )
                .replace(
                    "fn helper(text: &String)",
                    "fn helper(text: &Array<(Int32, Float64)>)",
                )
                .replace("string.byte_length(text)", "uint64(text[0].0)")
                .replace(
                    "Source { text: \"source destroyed\" }",
                    "Source { text: \"source destroyed\", values: [(7, 1.5)] }",
                )
                .replace("helper(&source.text)", "helper(&source.values)")
        } else {
            source
        };
        let source = if (2..4).contains(&kind) {
            let (view, setup, read) = if kind == 2 {
                (
                    "&(Int32, Float64)",
                    "let values = (7, 1.5)",
                    "uint64(text.0)",
                )
            } else {
                (
                    "&FixedArray<Int32, 2>",
                    "let values: FixedArray<Int32, 2> = [7, 9]",
                    "uint64(*text.borrow_ref(0))",
                )
            };
            source
                .replace(
                    "import joky/string",
                    "import joky/fixed_array.{type FixedArray}",
                )
                .replace(
                    "text: &String) @suspends",
                    &format!("text: {view}) @suspends"),
                )
                .replace("string.byte_length(text)", read)
                .replace("helper(&source.text)", &format!("{setup}; helper(&values)"))
        } else {
            source
        };
        let source = if kind == 4 {
            source
                .replace(
                    "import joky/string",
                    "import joky/string\nimport joky/array.{type Array}",
                )
                .replace(
                    "text: &String) @suspends",
                    "text: &Array<Array<String>>) @suspends",
                )
                .replace(
                    "string.byte_length(text)",
                    "string.byte_length(text.borrow_ref(0).borrow_ref(0))",
                )
                .replace(
                    "helper(&source.text)",
                    "let values = [[\"nested source\"]]; helper(&values)",
                )
        } else {
            source
        };
        let source = if kind == 5 {
            source
                .replace("import joky/string", "import joky/string\nimport joky/option.{type Option, Some, None}")
                .replace("extern fn trace", "fn enum_size(value: &Option<String>) -> UInt64 { match value { Some(s) => string.byte_length(s), None => 0 } }\nextern fn trace")
                .replace("text: &String) @suspends", "text: &Option<String>) @suspends")
                .replace("string.byte_length(text)", "enum_size(text)")
                .replace("helper(&source.text)", "let values = Some(\"enum source\"); helper(&values)")
        } else {
            source
        };
        let source = if kind == 6 {
            source
                .replace("import joky/string", "import joky/string\nimport joky/fixed_array.{type FixedArray}\nimport joky/list.{type List}\nimport joky/map.{type Map}\nimport joky/set.{type Set}\nimport joky/slice")
                .replace("extern fn trace", "fn collection_size(value: &FixedArray<List<Map<Int32, Set<String>>>, 1>) -> UInt64 { let table = list.borrow_ref(fixed_array.borrow_ref(value, 0), 0); let selected = map.borrow_ref(table, 7); let items = set.elements(selected); string.byte_length(slice.borrow_ref(items, 0)) }\nextern fn trace")
                .replace("text: &String) @suspends", "text: &FixedArray<List<Map<Int32, Set<String>>>, 1>) @suspends")
                .replace("string.byte_length(text)", "collection_size(text)")
                .replace("helper(&source.text)", "var selected = Set.new::<String>(); selected.insert(\"collection source\"); var table = Map.new::<Int32, Set<String>>(); table.insert(7, selected); var values = List.new::<Map<Int32, Set<String>>>(); values.push_back(table); let fixed: FixedArray<List<Map<Int32, Set<String>>>, 1> = [values]; helper(&fixed)")
        } else {
            source
        };
        let source = if kind == 0 {
            source
                .replace("text: &String) @suspends", "text: &Source) @suspends")
                .replace("string.byte_length(text)", "string.byte_length(&text.text)")
                .replace("helper(&source.text)", "helper(&source)")
        } else {
            source
        };
        // Exercise an indirect language call while the source is pinned by
        // nested readers. The callback contract is checked by the frontend.
        let view = match kind {
            0 => "&Source",
            1 => "&Array<(Int32, Float64)>",
            2 => "&(Int32, Float64)",
            3 => "&FixedArray<Int32, 2>",
            4 => "&Array<Array<String>>",
            5 => "&Option<String>",
            _ => "&FixedArray<List<Map<Int32, Set<String>>>, 1>",
        };
        let source = format!("{} fn callback(text: {view}) @suspends @noescape -> UInt64 {{ let call: fn({view}) @suspends @noescape -> UInt64 = reader; call(text) }}",
            source.replace("join { reader(text)", "join { callback(text)"));
        // Reuse the nested collection lifetime gate for an indirect branch
        // entry, including the old module snapshot after reload and VM drop.
        let source = if kind == 6 {
            source.replace(
                "let (a, b) = join { callback(text)",
                "let target = callback; let (a, b) = join { target(text)",
            )
        } else {
            source
        };
        vm.load_source(&source).unwrap();
        let budget = TaskBudget::new(4 << 20);
        let prepared = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap();
        vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(4, 16).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let release = Release;
        wait(|| GATE.get().unwrap().0.lock().unwrap().0);
        if scenario == 0 {
            wait(|| {
                OBSERVED
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, _)| name == "parent failed")
            });
        }
        if scenario == 1 {
            task.request_cancel();
        }
        if scenario == 2 {
            pool.request_close();
        }
        if scenario != 0 {
            wait(|| task.handle.stopping());
        }
        assert!(
            !task.is_finished(),
            "parent finished before its reader stopped"
        );
        let observations = crate::runtime::parallel::scoped::observations(task.identity());
        assert_eq!(
            observations.iter().filter(|o| !o.source).count(),
            1,
            "{mode:?}: reader did not receive a scoped view"
        );
        let tree = crate::runtime::parallel::scoped::tree_observations(task.identity());
        assert_eq!(
            tree.iter().filter(|o| !o.source).count(),
            2,
            "remote reader did not forward its loan"
        );
        assert!(tree.iter().all(|o| o.origin_parent == task.identity()));
        drop(release);
        task.wait().unwrap();
        let error = task.take_result().err().unwrap();
        if scenario == 0 {
            assert!(error.message().contains("Parent probe failure"));
        } else {
            assert_eq!(error.code(), ErrorCode::Cancelled);
        }
        drop(task);
        pool.wait_closed();
        let events = OBSERVED.lock().unwrap();
        let names: Vec<_> = events.iter().map(|(name, _)| name.as_str()).collect();
        if scenario == 0 {
            assert_eq!(names, ["parent failed", "reader stopped"]);
        } else {
            assert_eq!(names, ["reader stopped"]);
        }
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn parallel_owning_values_transfer_return_cancel_and_retain_code() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        OBSERVED.lock().unwrap().clear();
        let mut vm = engine(mode);
        let budget = TaskBudget::new(16 << 20);
        let main = vm
            .prepare_parallel_task("main", vec![], vec![budget.clone()])
            .unwrap_or_else(|e| panic!("{e}"));
        let failed = vm
            .prepare_parallel_task("failed", vec![], vec![budget.clone()])
            .unwrap();
        let parked = vm
            .prepare_parallel_task("parked", vec![], vec![budget.clone()])
            .unwrap();
        let pending_result = vm
            .prepare_parallel_task("pending_result", vec![], vec![budget.clone()])
            .unwrap();
        vm.load_source("fn replacement() -> Int32 { 99 }").unwrap();
        drop(vm);
        let mut pool = ParallelExecutor::new(3, 32).unwrap();
        let task = main.submit(&pool).unwrap();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result()
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"))
                .value(),
            &TaskValue::Scalar(InstanceValue::Int32(0))
        );
        drop(task);
        let task = failed.submit(&pool).unwrap();
        wait(|| task.is_finished());
        assert!(task.take_result().is_err());
        drop(task);
        let task = parked.submit(&pool).unwrap();
        wait(|| OBSERVED.lock().unwrap().iter().any(|(s, _)| s == "ready"));
        task.request_cancel();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().err().unwrap().code(),
            ErrorCode::Cancelled
        );
        drop(task);
        let task = pending_result.submit(&pool).unwrap();
        wait(|| {
            OBSERVED
                .lock()
                .unwrap()
                .iter()
                .any(|(s, _)| s == "produced")
        });
        task.request_cancel();
        wait(|| task.is_finished());
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
#[ignore = "exhaustive: owning packet preparation and failed installation budgets"]
fn exhaustive_parallel_owning_value_budgets() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = engine(mode);
        let mut success = false;
        for bytes in (0..(1 << 20)).step_by(256) {
            OBSERVED.lock().unwrap().clear();
            let budget = TaskBudget::new(bytes);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let result = vm
                .prepare_parallel_task("main", vec![], vec![budget.clone()])
                .and_then(|p| p.submit(&pool))
                .and_then(|t| {
                    wait(|| t.is_finished());
                    t.take_result()
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

#[test]
#[ignore = "exhaustive: each instruction of owning dyn conversion, replacement and cancellation"]
fn exhaustive_owned_dyn_instruction_cancellation() {
    let _serial = SERIAL.lock().unwrap();
    let mut vm = engine(VmMode::Interpreter);
    vm.set_interpreter_limits(crate::vm::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let mut completed = false;
    for boundary in 0..2048 {
        OBSERVED.lock().unwrap().clear();
        let mut task = vm.call_async("dynamic_lifecycle", &[]).unwrap();
        for _ in 0..boundary {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        completed = task.is_finished();
        if completed {
            assert_eq!(task.cps_result(), Some(11));
        }
        drop(task);
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0, "boundary {boundary}"));
        if completed {
            break;
        }
    }
    assert!(completed);
}

#[test]
fn scoped_borrow_host_reference_abi_keeps_the_whole_group_local() {
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        vm.load_source(
            r#"
            import joky/task
            extern fn trace(text: &String) -> Int32
            fn view(text: &String) @suspends -> Int32 { trace(text) }
            fn transit(text: &String) @suspends -> Int32 { view(text) }
            fn root() @suspends -> Int32 {
                let text = "local host reference"
                let (value, _) = join { transit(&text), task.yield_now() }
                value
            }
        "#,
        )
        .unwrap();
        let budget = TaskBudget::new(4 << 20);
        let mut pool = ParallelExecutor::new(3, 16).unwrap();
        let task = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(0))
        );
        assert!(crate::runtime::parallel::scoped::observations(task.identity()).is_empty());
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scoped_borrow_nested_race_and_isolate_wait_for_remote_readers() {
    use std::sync::{Condvar, OnceLock};
    static GATE: OnceLock<(Mutex<(bool, bool)>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn hold(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (state, wake) = GATE.get().unwrap();
        let mut state = state.lock().unwrap();
        state.0 = true;
        wake.notify_all();
        let (state, timeout) = wake
            .wait_timeout_while(state, Duration::from_secs(15), |s| !s.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            crate::runtime::error::record_error(Error::new(
                ErrorPhase::Runtime,
                "reader release timed out",
            ));
            return 3;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("reader stopped".into(), std::thread::current().id()));
        0
    }
    unsafe extern "C" fn selected(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        if !GATE.get().unwrap().0.lock().unwrap().0 {
            crate::runtime::parallel::pool::io_waker().unwrap().wake();
            return 1;
        }
        OBSERVED
            .lock()
            .unwrap()
            .push(("selected".into(), std::thread::current().id()));
        0
    }
    unsafe extern "C" fn fail(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
        if selected(frame) == 1 {
            return 1;
        }
        crate::runtime::error::record_error(Error::new(ErrorPhase::Runtime, "isolated failure"));
        3
    }
    unsafe extern "C" fn resumed(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        OBSERVED
            .lock()
            .unwrap()
            .push(("resumed".into(), std::thread::current().id()));
        0
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            let (state, wake) = GATE.get().unwrap();
            state.lock().unwrap().1 = true;
            wake.notify_all();
        }
    }
    let _serial = SERIAL.lock().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);

        for (name, callback) in [
            ("hold", hold as crate::runtime::cps::CpsEntry),
            ("selected", selected),
            ("fail", fail),
            ("resumed", resumed),
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
        vm.load_source(
            r#"
            import joky/string
            import joky/task
            extern fn hold() @suspends
            extern fn selected() @suspends
            extern fn fail() @suspends
            extern fn resumed() @suspends
            struct Source { text: String }

            fn reader(text: &String) @suspends -> UInt64 {
                let before = string.byte_length(text)
                hold()
                before + string.byte_length(text)
            }
            fn relay(text: &String) @suspends -> UInt64 {
                let (a, b) = join { reader(text), task.yield_now() }
                a
            }
            fn owned() @suspends -> Int32 {
                let source = Source { text: "borrowed source" }
                let (a, b) = join { relay(&source.text), task.yield_now() }
                0
            }
            fn dynamic_owned() @suspends -> Int32 {
                let source = Source { text: "borrowed source" }
                let view = &source.text
                scope { branch { relay(view); } }
                0
            }
            fn dynamic_isolated() @suspends -> Int32 {
                let result = isolate { scope { branch { dynamic_owned(); }; fail() }; 0 }
                resumed()
                match result { Ok(_) => 0, Err(_) => 7 }
            }
            fn dynamic_selected() @suspends {
                let source = Source { text: "borrowed source" }
                let view = &source.text
                scope { branch { relay(view); }; selected() }
            }
            fn dynamic_normal() @suspends -> Int32 {
                dynamic_selected()
                resumed()
                7
            }
            fn losing(anchor: &Int32) @suspends -> Int32 { owned() }
            fn winning(anchor: &Int32) @suspends -> Int32 { selected(); 7 }
            fn local_race() @suspends -> Int32 {
                let anchor = 1
                let value = race { losing(&anchor), winning(&anchor) }
                resumed()
                value
            }
            fn isolated() @suspends -> Int32 {
                let result = isolate { scope { branch { owned(); }; fail() }; 0 }
                resumed()
                match result { Ok(_) => 0, Err(_) => 7 }
            }
        "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for (name, cancel) in [
            ("local_race", false),
            ("local_race", true),
            ("isolated", false),
            ("isolated", true),
            ("dynamic_isolated", false),
            ("dynamic_isolated", true),
            ("dynamic_normal", false),
            ("dynamic_normal", true),
        ] {
            *GATE
                .get_or_init(|| (Mutex::new((false, false)), Condvar::new()))
                .0
                .lock()
                .unwrap() = (false, false);
            OBSERVED.lock().unwrap().clear();
            let budget = TaskBudget::new(4 << 20);
            let mut pool = ParallelExecutor::new(4, 16).unwrap();
            let task = vm
                .prepare_parallel_task(name, vec![], vec![budget.clone()])
                .unwrap()
                .submit(&pool)
                .unwrap();
            let release = Release;
            wait(|| {
                OBSERVED
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, _)| name == "selected")
            });
            assert!(!task.is_finished());
            assert_eq!(
                OBSERVED
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
                ["selected"],
                "{mode:?} {name}: premature cleanup or recovery"
            );
            let observations = crate::runtime::parallel::scoped::observations(task.identity());
            assert!(
                observations.iter().any(|o| !o.source),
                "{mode:?} {name}: no remote readers"
            );
            let tree = crate::runtime::parallel::scoped::tree_observations(task.identity());
            assert!(
                tree.iter().filter(|o| !o.source).count() >= 2,
                "{mode:?} {name}: nested reader loan was not forwarded"
            );
            if cancel {
                task.request_cancel();
            }
            drop(release);
            wait(|| task.is_finished());
            let result = task.take_result();
            if cancel {
                assert_eq!(result.err().unwrap().code(), ErrorCode::Cancelled);
            } else {
                assert_eq!(
                    result.unwrap().value(),
                    &TaskValue::Scalar(InstanceValue::Int32(7))
                );
            }
            drop(task);
            pool.wait_closed();
            let events = OBSERVED.lock().unwrap();
            let names: Vec<_> = events.iter().map(|(name, _)| name.as_str()).collect();
            if cancel {
                assert_eq!(names, ["selected", "reader stopped"]);
            } else {
                assert_eq!(names, ["selected", "reader stopped", "resumed"]);
            }
            assert_eq!(budget.used(), 0, "{mode:?} {name}");
        }
    }
}

#[test]
fn scoped_borrow_owners_release_storage_after_readers() {
    let _serial = SERIAL.lock().unwrap();
    let source = include_str!("../../../tests/fixtures/scoped_borrow_drop.jk").replace(
        "fn trace(text: &String) -> Int32 { 0 }",
        "extern fn trace(text: &String) -> Int32",
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for name in [
            "main",
            "dynamic",
            "containers",
            "failed",
            "raced",
            "isolated",
        ] {
            OBSERVED.lock().unwrap().clear();
            let mut vm = engine(mode);
            vm.set_prelude_enabled(false);
            vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
            let budget = TaskBudget::new(4 << 20);
            let prepared = vm
                .prepare_parallel_task(name, vec![], vec![budget.clone()])
                .unwrap();
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
            drop(vm);
            let mut pool = ParallelExecutor::new(3, 32).unwrap();
            let task = prepared.submit(&pool).unwrap();
            wait(|| task.is_finished());
            let result = task.take_result();
            if name == "failed" {
                assert_eq!(result.err().unwrap().code(), ErrorCode::RuntimeError);
            } else {
                result.unwrap();
            }
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0);
        }
    }
}
