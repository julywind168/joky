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

fn input_engine(mode: VmMode) -> VmEngine {
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
        fn dynamic_inputs() @suspends -> Int32 {{
            let result = isolate {{
                let text = "source"
                let view = &text
                let first = Input {{ text: "first" }}
                let second = Input {{ text: "second" }}
                scope {{
                    branch {{ with_input(view, first); }}
                    branch {{ with_input(view, second); }}
                }}
                12
            }}
            if cleanups() != 0 {{ return -2 }}
            match result {{ Ok(value) => value, Err(_) => -1 }}
        }}
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
    vm
}

#[test]
fn scoped_borrow_unpublished_dynamic_inputs_finish_cleanup_before_recovery() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = input_engine(mode);
        let budget = TaskBudget::new(4 << 20);
        // The root occupies the only slot: capture succeeds, publication fails,
        // and there is no child whose lifetime could incidentally delay recovery.
        let mut pool = ParallelExecutor::new(1, 1).unwrap();
        let task = vm
            .prepare_parallel_task("dynamic_inputs", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(-1)),
            "{mode:?}"
        );
        let observations = crate::runtime::parallel::scoped::observations(task.identity());
        assert_eq!(observations.iter().filter(|o| o.source).count(), 1);
        assert!(!observations.iter().any(|o| !o.source));
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

fn budgets(amounts: impl Clone + IntoIterator<Item = usize>) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = input_engine(mode);
        for (name, expected) in [
            ("isolated_inputs", InstanceValue::Int32(12)),
            ("advanced", InstanceValue::UInt64(17)),
            ("aggregate_readers", InstanceValue::Int32(109)),
            ("aggregate_zero", InstanceValue::Int32(9)),
            ("forwarded", InstanceValue::Int32(145)),
            ("dynamic_shared", InstanceValue::Int32(42)),
            ("dynamic_inputs", InstanceValue::Int32(12)),
            ("dynamic_views", InstanceValue::Int32(12)),
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
                        if matches!(name, "isolated_inputs" | "dynamic_inputs")
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
                                "nested_isolated_success"
                                    | "isolated_inputs"
                                    | "forwarded_isolate"
                                    | "dynamic_inputs"
                                    | "aggregate_readers"
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
    budgets([0, 512, 4096, 16384, 65536, 1 << 20, 4 << 20]);
}
#[test]
#[ignore = "exhaustive: scoped descriptors, view installation, mixed owners and results"]
fn exhaustive_scoped_borrow_budgets() {
    // The shared aggregate fixture increases frozen interpreter metadata.
    // Preserve every failure boundary and stop each case at its first success.
    budgets((0..(4 << 20)).step_by(128));
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
    let callbacks = include_str!("../../../tests/fixtures/scoped_borrow_callbacks.jk")
        .replace("task.yield_now()", "checkpoint()")
        .replace("    values[0] = 42", "    checkpoint()\n    values[0] = 42");
    let plain = include_str!("../../../tests/fixtures/scoped_borrow_plain.jk")
        .replace("task.yield_now()", "checkpoint()");
    let owners = include_str!("../../../tests/fixtures/scoped_borrow_owners.jk")
        .replace("task.yield_now()", "checkpoint()");
    let enums = include_str!("../../../tests/fixtures/scoped_borrow_enums.jk")
        .replace("task.yield_now()", "checkpoint()");
    let collections = include_str!("../../../tests/fixtures/scoped_borrow_collections.jk")
        .replace("task.yield_now()", "checkpoint()");
    let indirect = include_str!("../../../tests/fixtures/scoped_borrow_indirect.jk")
        .replace("task.yield_now()", "checkpoint()");
    for (source, cases) in [
        (
            indirect,
            vec![("main", 13), ("dynamic", 42), ("owned_result", 6)],
        ),
        (
            collections,
            vec![("main", 78), ("maps", 20), ("dynamic", 42)],
        ),
        (
            enums,
            vec![
                ("main", 12),
                ("dynamic", 2),
                ("arrays", 12),
                ("fixed_move", 12),
            ],
        ),
        (owners, vec![("main", 54), ("dynamic", 42), ("strings", 10)]),
        (
            source,
            vec![
                ("main", 104),
                ("nested_isolated_success", 91),
                ("forwarded", 145),
                ("dynamic_return", 26),
                ("aggregate_capture", 30),
                ("aggregate_zero", 9),
            ],
        ),
        (callbacks, vec![("main", 39), ("dynamic", 42)]),
        (
            plain,
            vec![("main", 33), ("dynamic", 42), ("arrays", 21), ("zero", 1)],
        ),
    ] {
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
            // Sweep joins, not races whose winner may cancel a parked loser first.
            for &(name, expected) in &cases {
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

#[test]
fn scoped_borrow_dynamic_capture_keeps_sources_read_only_until_join() {
    let declarations = r#"
        import joky/array.{type Array}
        import joky/task
        fn read(values: &Array<Int32>) @suspends { task.yield_now(); values.len(); }
    "#;
    for body in [
        "var values = [1, 2]; let view = &values; scope { branch { read(view) }; values[0] = 9 }",
        "var values = [1, 2]; let view = &values; scope { branch { read(view) }; let moved = values }",
        "var values = [1, 2]; let view = &values; scope { branch { read(view) }; let write = &mut values; write[0] = 9 }",
        "scope { { let values = [1, 2]; let view = &values; branch { read(view) } } }",
    ] {
        let mut vm = VmEngine::new();
        let error = vm.load_source(&format!("{declarations} fn root() @suspends {{ {body} }}")).unwrap_err();
        assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::InvalidCall), "{body}: {error}");
        assert!(error.message().to_lowercase().contains("borrow"), "{body}: {error}");
    }
}

#[test]
fn scoped_borrow_dynamic_join_advances_local_children_while_readers_wait() {
    static READY: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn await_local(_: *mut CpsFrame) -> i32 {
        if READY.load(Ordering::Acquire) {
            return 0;
        }
        crate::runtime::parallel::pool::io_waker().unwrap().wake();
        1
    }
    unsafe extern "C" fn release(_: *mut CpsFrame) -> i32 {
        READY.store(true, Ordering::Release);
        0
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        READY.store(false, Ordering::Release);
        let mut vm = VmEngine::with_mode(mode);
        for (name, callback) in [
            ("await_local", await_local as crate::runtime::cps::CpsEntry),
            ("release", release),
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
            extern fn await_local() @suspends
            extern fn release() @suspends
            fn local(anchor: &mut Int32) @suspends { release() }
            fn root() @suspends -> UInt64 {
                let text = "shared"
                let view = &text
                var anchor = 1
                let same_worker = &mut anchor
                scope {
                    branch { await_local(); string.byte_length(view); }
                    branch { local(same_worker) }
                }
                string.byte_length(&text)
            }
        "#,
        )
        .unwrap();
        let budget = TaskBudget::new(4 << 20);
        let mut pool = ParallelExecutor::new(1, 16).unwrap();
        let task = vm
            .prepare_parallel_task("root", vec![], vec![budget.clone()])
            .unwrap()
            .submit(&pool)
            .unwrap();
        wait(|| task.is_finished());
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::UInt64(6))
        );
        assert_eq!(
            crate::runtime::parallel::scoped::observations(task.identity())
                .iter()
                .filter(|o| !o.source)
                .count(),
            1
        );
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

fn scoped_graph_budgets(amounts: impl Clone + IntoIterator<Item = usize>) {
    for (source, cases) in [
        (
            include_str!("../../../tests/fixtures/scoped_borrow_indirect.jk"),
            &[("main", 13), ("dynamic", 42), ("owned_result", 6)][..],
        ),
        (
            include_str!("../../../tests/fixtures/scoped_borrow_collections.jk"),
            &[("main", 78), ("maps", 20), ("dynamic", 42)][..],
        ),
        (
            include_str!("../../../tests/fixtures/scoped_borrow_enums.jk"),
            &[
                ("main", 12),
                ("dynamic", 2),
                ("arrays", 12),
                ("fixed_move", 12),
            ][..],
        ),
        (
            include_str!("../../../tests/fixtures/scoped_borrow_owners.jk"),
            &[("main", 54), ("dynamic", 42), ("strings", 10)][..],
        ),
        (
            include_str!("../../../tests/fixtures/scoped_borrow_callbacks.jk"),
            &[("main", 39), ("dynamic", 42)][..],
        ),
        (
            include_str!("../../../tests/fixtures/scoped_borrow_plain.jk"),
            &[("main", 33), ("dynamic", 42), ("arrays", 21), ("zero", 1)][..],
        ),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            vm.load_source(source).unwrap();
            for &(name, expected) in cases {
                let mut success = false;
                for bytes in amounts.clone() {
                    let budget = TaskBudget::new(bytes);
                    let mut pool = ParallelExecutor::new(3, 32).unwrap();
                    let result = vm
                        .prepare_parallel_task(name, vec![], vec![budget.clone()])
                        .and_then(|p| p.submit(&pool))
                        .and_then(|task| {
                            wait(|| task.is_finished());
                            task.take_result()
                        });
                    match &result {
                        Ok(value) => {
                            assert_eq!(
                                value.value(),
                                &TaskValue::Scalar(InstanceValue::Int32(expected))
                            );
                            success = true;
                        }
                        Err(error) => assert_eq!(
                            error.code(),
                            ErrorCode::ResourceLimit,
                            "{mode:?}/{name}/{bytes}: {error}"
                        ),
                    }
                    drop(result);
                    pool.wait_closed();
                    assert_eq!(budget.used(), 0, "{mode:?}/{name}/{bytes}");
                    if success {
                        break;
                    }
                }
                assert!(success);
            }
        }
    }
}

#[test]
fn scoped_borrow_callback_and_plain_budget_boundaries() {
    scoped_graph_budgets([0, 512, 4096, 16384, 65536, 1 << 20, 4 << 20]);
}
#[test]
#[ignore = "exhaustive: ordinary local storage, scoped inputs and callback allocation failures"]
fn exhaustive_scoped_borrow_plain_budgets() {
    scoped_graph_budgets((0..(4 << 20)).step_by(128));
}

#[test]
fn scoped_borrow_plain_captures_keep_mutation_and_escape_rejected() {
    let declarations = r#"
        import joky/task
        struct Point { value: Int32 }
        fn read(value: &Point) @suspends { task.yield_now(); value.value; }
    "#;
    for body in [
        "var value = Point { value: 1 }; let view = &value; scope { branch { read(view) }; value.value = 2 }",
        "var value = Point { value: 1 }; let view = &value; scope { branch { read(view) }; let write = &mut value }",
        "scope { { let value = Point { value: 1 }; let view = &value; branch { read(view) } } }",
    ] {
        let mut vm = VmEngine::new();
        let error = vm.load_source(&format!("{declarations} fn root() @suspends {{ {body} }}")).unwrap_err();
        assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::InvalidCall), "{body}: {error}");
        assert!(error.message().to_lowercase().contains("borrow"), "{body}: {error}");
    }
}

#[test]
fn scoped_borrow_enum_captures_reject_variant_changes_and_escaping_sources() {
    let declarations = r#"
        import joky/task
        enum Choice { Empty, Text(String) }
        fn read(value: &Choice) @suspends { task.yield_now(); match value { Choice.Empty => {}, Choice.Text(_) => {} } }
    "#;
    for body in [
        "var value = Choice.Text(\"x\"); let view = &value; scope { branch { read(view) }; value = Choice.Empty }",
        "var value = Choice.Text(\"x\"); let view = &value; scope { branch { read(view) }; let write = &mut value }",
        "scope { { let value = Choice.Text(\"x\"); let view = &value; branch { read(view) } } }",
    ] {
        let source = format!("{declarations} fn root() @suspends {{ {body} }}");
        let error = VmEngine::new().load_source(&source).unwrap_err();
        assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::InvalidCall), "{body}: {error}");
    }
}

#[test]
fn scoped_borrow_collection_captures_keep_mutation_and_escape_rejected() {
    let source = include_str!("../../../tests/fixtures/scoped_borrow_collections.jk");
    for body in [
        "var value = make_list(); let view = &value; scope { branch { read_list(view); }; value.push_back(\"x\") }",
        "var value = make_map(); let view = &value; scope { branch { read_map(view); }; value.clear() }",
        "var value = make_set(); let view = &value; scope { branch { read_set(view); }; let write = &mut value }",
        "scope { { let value = make_map(); let view = &value; branch { read_map(view); } } }",
    ] {
        let source = format!("{source} fn invalid() @suspends {{ {body} }}");
        let error = VmEngine::new().load_source(&source).unwrap_err();
        assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::InvalidCall), "{body}: {error}");
    }
}
