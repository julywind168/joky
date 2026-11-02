//! TP2: real linked code and owning storage travel independently of the compiler.
use super::{instances::shared_runtime_object, AotCompiler, ScratchDir};
use crate::{
    runtime::{cps::CpsEntry, parallel::TaskBudget},
    vm::TaskValue,
};
use std::{collections::HashMap, process::Command, sync::Arc};

fn linked_task_entries(
    source: &str,
    names: &[&str],
    label: &str,
) -> (
    ScratchDir,
    Vec<crate::backends::aot::ParallelEntry>,
    Arc<libloading::Library>,
) {
    let scratch = ScratchDir::new(label);
    let mut compiler = AotCompiler::new(label).unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_or_else(|e| panic!("{e}"));
    let entries: Vec<_> = names
        .iter()
        .map(|name| compiler.parallel_entry(name).unwrap())
        .collect();
    let mut symbols: HashMap<_, _> = crate::runtime::cps::symbols()
        .into_iter()
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::buffer::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::place::symbols())
        .chain(crate::runtime::float_env::symbols())
        .map(|(n, p)| (n.to_owned(), p))
        .collect();
    symbols.insert(
        "joky_task_yield_now".into(),
        crate::stdlib::task::yield_now as *const u8,
    );
    symbols.insert(
        "joky_task_sleep_ms".into(),
        crate::stdlib::task::sleep_ms as *const u8,
    );
    symbols.insert(
        "joky_task_sleep_ns".into(),
        crate::stdlib::task::sleep_ns as *const u8,
    );
    symbols.insert(
        "__joky_task_yield_now".into(),
        crate::stdlib::task::yield_internal as *const u8,
    );
    symbols.insert(
        "__joky_task_sleep_ms".into(),
        crate::stdlib::task::sleep_internal as *const u8,
    );
    symbols.insert(
        "joky_time_unix_micros".into(),
        crate::stdlib::time::joky_time_unix_micros as *const u8,
    );
    let registry = crate::stdlib::module_registry();
    let output = registry.binding("__joky_write_stdout").unwrap();
    let crate::runtime::bindings::HostEntry::Direct(entry) = output.entry.as_ref().unwrap() else {
        panic!("Expected the native stdout binding");
    };
    symbols.insert("joky_io_write_stdout_v2".into(), *entry);
    symbols.insert(
        "__joky_domain_poll_v4".into(),
        crate::runtime::domain::poll_native as *const u8,
    );
    symbols.insert(
        "__joky_password_poll".into(),
        crate::runtime::password::poll_native as *const u8,
    );
    let object = scratch.path().join("code.o");
    let bridge = scratch.path().join("bridge.o");
    let library = scratch.path().join("code.so");
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    compiler.write_object(&object).unwrap();
    let linked = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg(&bridge)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(linked.status.success(), "{linked:?}");
    let library = Arc::new(unsafe { libloading::Library::new(&library).unwrap() });
    (scratch, entries, library)
}

#[test]
fn full_slices_keep_repeated_utf8_conversion_bounded_on_all_backends() {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    const SOURCE: &str = r#"
        import joky/string
        import joky/result.{type Result, Ok, Err}
        fn main() @suspends -> Int32 {
            var count = 0
            while count < 1000 {
                let bytes = [uint8(104), uint8(195), uint8(169)]
                let text = match string.from_utf8(bytes) {
                    Ok(text) => text, Err(_) => return -1
                }
                if string.byte_length(&text) != 3 { return -2 }
                count += 1
            }
            count
        }
    "#;
    let (_scratch, entries, library) = linked_task_entries(SOURCE, &["main"], "full-slice-utf8");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(SOURCE).unwrap();
        }
        // A retained Buffer header per conversion alone would exceed this limit.
        let budget = TaskBudget::new(128 << 10);
        let prepared = if let Some(vm) = &vm {
            vm.prepare_parallel_task("main", vec![], vec![budget.clone()])
                .unwrap()
        } else {
            let entry = &entries[0];
            unsafe {
                entry.prepare(
                    *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                    library.clone(),
                    vec![],
                    vec![budget.clone()],
                )
            }
            .unwrap()
        };
        let mut pool = crate::runtime::parallel::ParallelExecutor::new(1, 4).unwrap();
        let task = prepared.submit(&pool).unwrap();
        task.wait().unwrap();
        let result = task
            .take_result()
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(
            result.value(),
            &TaskValue::Scalar(InstanceValue::Int32(1000))
        );
        drop(result);
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0, "{mode:?}");
    }
}

#[test]
fn scoped_borrow_join_preserves_values_on_all_backends() {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    const SOURCE: &str = include_str!("../../../../tests/fixtures/scoped_borrow_join.jk");
    let (_scratch, entries, library) =
        linked_task_entries(SOURCE, &["main", "advanced"], "scoped-borrow");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(SOURCE).unwrap();
            let mut local = vm.call_async("main", &[]).unwrap();
            for _ in 0..10000 {
                if local.is_finished() {
                    break;
                }
                local.resume().unwrap();
            }
            assert!(local.is_finished());
            assert_eq!(local.cps_result(), Some(104));
        }
        for workers in [1, 3] {
            let budget = TaskBudget::new(4 << 20);
            let prepared = if let Some(vm) = &vm {
                vm.prepare_parallel_task("main", vec![], vec![budget.clone()])
                    .unwrap()
            } else {
                let entry = &entries[0];
                unsafe {
                    entry.prepare(
                        *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                        library.clone(),
                        vec![],
                        vec![budget.clone()],
                    )
                }
                .unwrap()
            };
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 16).unwrap();
            let task = prepared.submit(&pool).unwrap();
            task.wait().unwrap();
            let result = task
                .take_result()
                .unwrap_or_else(|e| panic!("{mode:?}, {workers}: {e}"));
            assert_eq!(
                result.value(),
                &TaskValue::Scalar(InstanceValue::Int32(104)),
                "{mode:?}, {workers}"
            );
            let observations = crate::runtime::parallel::scoped::observations(task.identity());
            let sources: Vec<_> = observations.iter().filter(|o| o.source).collect();
            let targets: Vec<_> = observations.iter().filter(|o| !o.source).collect();
            assert_eq!(
                sources.len(),
                6,
                "{mode:?}, {workers}: borrowed join was not admitted"
            );
            assert_eq!(
                targets.len(),
                6,
                "{mode:?}, {workers}: missing installed views"
            );
            for target in &targets {
                assert!(
                    sources
                        .iter()
                        .any(|source| source.scope == target.scope && source.data == target.data),
                    "payload copied during transport"
                );
            }
            if workers > 1 {
                assert!(
                    targets
                        .iter()
                        .any(|target| target.thread != sources[0].thread),
                    "readers did not leave the parent worker"
                );
            } else {
                assert!(targets
                    .iter()
                    .all(|target| target.thread == sources[0].thread));
            }
            let advanced = if let Some(vm) = &vm {
                vm.prepare_parallel_task("advanced", vec![], vec![budget.clone()])
                    .unwrap()
            } else {
                let entry = &entries[1];
                unsafe {
                    entry.prepare(
                        *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                        library.clone(),
                        vec![],
                        vec![budget.clone()],
                    )
                }
                .unwrap()
            };
            let advanced = advanced.submit(&pool).unwrap();
            advanced.wait().unwrap();
            assert_eq!(
                advanced.take_result().unwrap().value(),
                &TaskValue::Scalar(InstanceValue::UInt64(17)),
                "{mode:?}, {workers}"
            );
            drop(advanced);
            drop((result, task));
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}, {workers}");
        }
    }
}

#[test]
fn task_parallel_aggregate_values_and_roots_on_all_backends() {
    use crate::vm::{parallel::aggregates, InstanceValue, VmEngine, VmMode};
    let names = [
        "root",
        "single",
        "empty",
        "copied",
        "nested_empty",
        "mixed",
        "identity",
    ];
    let (_scratch, entries, library) =
        linked_task_entries(aggregates::SOURCE, &names, "task-aggregates");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(aggregates::SOURCE).unwrap();
        }
        for workers in [1, 3] {
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
            let mut budgets = Vec::new();
            for (index, name) in names[..names.len() - 1].iter().enumerate() {
                let sig = vm
                    .as_ref()
                    .map(|vm| vm.parallel_signature(name).unwrap())
                    .unwrap_or_else(|| entries[index].signature().clone());
                for variant in 0..if *name == "root" { 3 } else { 1 } {
                    let (inputs, expected) = match *name {
                        "root" => {
                            let input = aggregates::input(&sig.params[0], variant);
                            (vec![input.clone()], aggregates::expected(&sig, input))
                        }
                        "single" => {
                            let v = TaskValue::Record {
                                ty: sig.params[0].clone(),
                                fields: vec![TaskValue::Scalar(InstanceValue::Int32(42))],
                            };
                            (vec![v.clone()], v)
                        }
                        "empty" => {
                            let v = TaskValue::Record {
                                ty: sig.params[0].clone(),
                                fields: vec![],
                            };
                            (vec![v.clone()], v)
                        }
                        "copied" => {
                            let v = TaskValue::Tuple(vec![
                                TaskValue::Scalar(InstanceValue::Int8(-100)),
                                TaskValue::Scalar(InstanceValue::UInt16(65000)),
                                TaskValue::Scalar(InstanceValue::Float32(-1.25)),
                                TaskValue::Scalar(InstanceValue::Unit),
                            ]);
                            (vec![v.clone()], v)
                        }
                        "mixed" => (
                            vec![
                                TaskValue::Scalar(InstanceValue::Unit),
                                TaskValue::String("mixed arguments".into()),
                                TaskValue::Scalar(InstanceValue::Int32(42)),
                            ],
                            TaskValue::Tuple(vec![
                                TaskValue::String("mixed arguments".into()),
                                TaskValue::Scalar(InstanceValue::Int32(42)),
                            ]),
                        ),
                        _ => (
                            vec![],
                            TaskValue::Tuple(vec![
                                TaskValue::Scalar(InstanceValue::Unit),
                                TaskValue::Record {
                                    ty: sig.ret.tuple_element_types().unwrap()[1].clone(),
                                    fields: vec![],
                                },
                            ]),
                        ),
                    };
                    let budget = TaskBudget::new(1 << 20);
                    let prepared = if let Some(vm) = &vm {
                        vm.prepare_parallel_task(name, inputs, vec![budget.clone()])
                            .unwrap()
                    } else {
                        unsafe {
                            entries[index].prepare(
                                *library
                                    .get::<CpsEntry>(entries[index].symbol().as_bytes())
                                    .unwrap(),
                                library.clone(),
                                inputs,
                                vec![budget.clone()],
                            )
                        }
                        .unwrap()
                    };
                    let task = prepared.submit(&pool).unwrap();
                    task.wait().unwrap();
                    let result = task
                        .take_result()
                        .unwrap_or_else(|e| panic!("{mode:?}, {name}, variant={variant}: {e:?}"));
                    assert_eq!(
                        result.value(),
                        &expected,
                        "{mode:?}, {name}, variant={variant}"
                    );
                    assert!(task.take_result().is_err());
                    drop((result, task));
                    budgets.push((name, variant, budget));
                }
            }
            // Completion releases language values, but the worker can retain
            // task metadata until its polling turn unwinds. Drop joins workers.
            drop(pool);
            for (name, variant, budget) in budgets {
                assert_eq!(budget.used(), 0, "{mode:?}, {name}, variant={variant}");
            }
        }
    }
    let entry = entries.last().unwrap();
    let expected = aggregates::input(&entry.signature().params[0], 2);
    let weak = Arc::downgrade(&library);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![expected.clone()],
            vec![],
        )
    }
    .unwrap();
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = prepared.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(output.value(), &expected);
}

#[test]
fn parallel_aot_frozen_code_and_owning_capture() {
    let scratch = ScratchDir::new("tp2-storage");
    let mut compiler = AotCompiler::new("tp2").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    compiler
        .compile_module(
            &crate::parser::parse(
                r#"
        fn echo(text: String) @suspends -> String { text }
        fn root(text: String) @suspends -> String {
            let (a, b) = join { | echo(text) | echo("temporary") }
            a
        }
        fn local(value: Ptr) @suspends -> Ptr { value }
    "#,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(compiler.parallel_entry("local").is_err());
    let checked = compiler.parallel_entry("root").unwrap();
    let symbols: HashMap<_, _> = crate::runtime::cps::symbols()
        .into_iter()
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::buffer::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::place::symbols())
        .chain(crate::runtime::float_env::symbols())
        .map(|(name, ptr)| (name.into(), ptr))
        .collect();
    let object = scratch.path().join("code.o");
    let bridge = scratch.path().join("bridge.o");
    let path = scratch.path().join("code.so");
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    compiler.write_object(&object).unwrap();
    let output = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg(&bridge)
        .arg("-o")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let library = Arc::new(unsafe { libloading::Library::new(&path).unwrap() });
    let entry = unsafe {
        *library
            .get::<CpsEntry>(checked.symbol().as_bytes())
            .unwrap()
    };
    let budget = TaskBudget::new(1 << 20);
    let task = unsafe {
        checked.prepare(
            entry,
            library.clone(),
            vec![TaskValue::String("AOT owns 跨线程".into())],
            vec![budget.clone()],
        )
    }
    .unwrap();

    let pooled = unsafe {
        checked.prepare(
            entry,
            library.clone(),
            vec![TaskValue::String("AOT pool owns".into())],
            vec![budget.clone()],
        )
    }
    .unwrap();
    let weak = Arc::downgrade(&library);
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = task.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert_eq!(output.value(), &TaskValue::String("AOT owns 跨线程".into()));

    let executor = crate::runtime::parallel::ParallelExecutor::new(2, 16).unwrap();
    let pooled = pooled.submit(&executor).unwrap();
    pooled.wait().unwrap();
    let pooled_output = pooled.take_result().unwrap();
    assert_eq!(
        pooled_output.value(),
        &TaskValue::String("AOT pool owns".into())
    );
    drop((pooled, executor));
    assert!(
        weak.upgrade().is_none(),
        "code can retire after task cleanup"
    );
    drop((output, pooled_output));
    assert_eq!(budget.used(), 0);
}

#[test]
fn task_pool_generated_branches_overlap_and_cancel_on_all_backends() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, VmEngine, VmMode},
        FunctionSignature, Type,
    };
    use std::sync::{Condvar, Mutex, OnceLock};
    static GATE: OnceLock<(Mutex<Vec<std::thread::ThreadId>>, Condvar)> = OnceLock::new();
    unsafe extern "C" fn overlap(_: *mut crate::runtime::cps::CpsFrame) -> i32 {
        let (threads, wake) = GATE.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()));
        let mut threads = threads.lock().unwrap();
        threads.push(std::thread::current().id());
        wake.notify_all();
        let (threads, timeout) = wake
            .wait_timeout_while(threads, std::time::Duration::from_secs(5), |v| v.len() < 2)
            .unwrap();
        if timeout.timed_out() && threads.len() < 2 {
            crate::runtime::error::record_error(crate::Error::new(
                crate::ErrorPhase::Runtime,
                "Generated task branches did not overlap",
            ));
            return 3;
        }
        0
    }
    const SOURCE: &str = r#"
        import joky/drop.{type Drop}
        struct Guard { zero: Int32, text: String }
        impl Drop for Guard { fn drop(&mut self) { 1 / self.zero; } }
        extern fn overlap() @suspends
        fn consume(text: String) { }
        fn leaf(n: Int32) @suspends -> Int32 {
            let text = "allocated on worker"
            overlap()
            var i = 0
            var sum = 0
            while i < 128 { sum = sum + i; i = i + 1 }
            consume(text)
            n + sum - 8128
        }
        fn joined() @suspends -> Int32 { let (a, b) = join { | leaf(41) | leaf(42) }; a + b }
        fn raced() @suspends -> Int32 { race { | leaf(41) | leaf(42) } }
        fn branched() @suspends -> Int32 { scope { branch { leaf(41); }; leaf(42) } }
        fn burn() -> Int32 { var n = 0; while true { n = n + 1 } }
        fn slow() @suspends -> Int32 { overlap(); burn() }
        fn fast() @suspends -> Int32 { overlap(); 42 }
        fn cancel_loop() @suspends -> Int32 { race { | slow() | fast() } }
        fn losing_text() @suspends -> String {
            let guard = Guard { zero: 0, text: "cleanup owner" }
            overlap()
            while true { }
        }
        fn winning_text() @suspends -> String { overlap(); "winner" }
        fn cleanup_race() @suspends -> String { race { | losing_text() | winning_text() } }
        fn cancel_cleanup() @suspends -> String { let (a, b) = join { | losing_text() | losing_text() }; a }
        fn cancel_body() @suspends -> String {
            scope { branch { losing_text(); }; branch { losing_text(); }; while true { } }
        }
    "#;
    let scratch = ScratchDir::new("tp3-workers");
    let mut compiler = AotCompiler::new("tp3").unwrap();
    compiler.set_prelude_enabled(false).unwrap();
    unsafe {
        compiler
            .declare_parallel_host("overlap", FunctionSignature::new(vec![], Type::Unit), 0)
            .unwrap();
    }
    compiler
        .compile_module(&crate::parser::parse(SOURCE).unwrap())
        .unwrap();
    let entries: Vec<_> = [
        "joined",
        "raced",
        "branched",
        "cancel_loop",
        "cleanup_race",
        "cancel_cleanup",
        "cancel_body",
    ]
    .into_iter()
    .map(|name| compiler.parallel_entry(name).unwrap())
    .collect();
    let mut symbols: HashMap<_, _> = crate::runtime::cps::symbols()
        .into_iter()
        .chain(crate::runtime::error::symbols())
        .chain(crate::runtime::arena::symbols())
        .chain(crate::runtime::buffer::symbols())
        .chain(crate::runtime::cleanup::symbols())
        .chain(crate::runtime::owned_text::symbols())
        .chain(crate::runtime::place::symbols())
        .chain(crate::runtime::float_env::symbols())
        .map(|(n, p)| (n.to_owned(), p))
        .collect();
    symbols.insert("overlap".into(), overlap as *const u8);
    let bridge = scratch.path().join("bridge.o");
    let object = scratch.path().join("code.o");
    let library = scratch.path().join("code.so");
    std::fs::write(&bridge, shared_runtime_object(&compiler.module, &symbols)).unwrap();
    compiler.write_object(&object).unwrap();
    let status = Command::new("cc")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg(&object)
        .arg(&bridge)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let library = Arc::new(unsafe { libloading::Library::new(library).unwrap() });
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            unsafe {
                vm.register_parallel_suspending_fn(
                    "overlap",
                    FunctionSignature::new(vec![], Type::Unit),
                    overlap,
                    0,
                    Arc::new(()),
                )
                .unwrap();
            }
            vm.load_source(SOURCE).unwrap();
        }
        for (index, name) in [
            "joined",
            "raced",
            "branched",
            "cancel_loop",
            "cleanup_race",
            "cancel_cleanup",
            "cancel_body",
        ]
        .into_iter()
        .enumerate()
        {
            GATE.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()))
                .0
                .lock()
                .unwrap()
                .clear();
            let executor =
                ParallelExecutor::new(if name == "cancel_body" { 3 } else { 2 }, 64).unwrap();
            let budget = TaskBudget::new(1 << 20);
            let prepared = if let Some(vm) = &vm {
                vm.prepare_parallel_task(name, vec![], vec![budget.clone()])
                    .unwrap()
            } else {
                unsafe {
                    entries[index].prepare(
                        *library
                            .get::<CpsEntry>(entries[index].symbol().as_bytes())
                            .unwrap(),
                        library.clone(),
                        vec![],
                        vec![budget.clone()],
                    )
                }
                .unwrap()
            };
            let task = prepared.submit(&executor).unwrap();
            if matches!(name, "cancel_cleanup" | "cancel_body") {
                let (threads, wake) = GATE.get().unwrap();
                let (threads, timeout) = wake
                    .wait_timeout_while(
                        threads.lock().unwrap(),
                        std::time::Duration::from_secs(5),
                        |v| v.len() < 2,
                    )
                    .unwrap();
                assert!(
                    !timeout.timed_out() || threads.len() == 2,
                    "children did not start"
                );
                drop(threads);
                task.request_cancel();
            }
            task.wait().unwrap();
            let result = task.take_result();
            if matches!(name, "cleanup_race" | "cancel_cleanup" | "cancel_body") {
                assert_eq!(
                    result
                        .err()
                        .expect("loser cleanup must fail race")
                        .message(),
                    "Division by zero"
                );
            } else {
                let TaskValue::Scalar(InstanceValue::Int32(value)) = result.unwrap().into_value()
                else {
                    panic!("scalar result")
                };
                match name {
                    "joined" => assert_eq!(value, 83),
                    "raced" => assert!([41, 42].contains(&value)),
                    _ => assert_eq!(value, 42),
                }
            }
            let threads = GATE.get().unwrap().0.lock().unwrap();
            assert_eq!(threads.len(), 2, "{mode:?} {name}");
            assert_ne!(threads[0], threads[1]);
            drop(threads);
            drop(task);
            drop(executor);
            assert_eq!(budget.used(), 0, "{mode:?} {name}");
        }
    }
}

#[test]
fn task_parallel_array_values_and_roots_on_all_backends() {
    use crate::vm::{parallel::array_tests, InstanceValue, VmEngine, VmMode};
    let names = ["root", "units", "capacity", "identity"];
    let (_scratch, entries, library) =
        linked_task_entries(array_tests::SOURCE, &names, "task-arrays");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(array_tests::SOURCE).unwrap();
        }
        for workers in [1, 3] {
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
            let mut completed = Vec::new();
            for (index, name) in names[..3].iter().enumerate() {
                let sig = vm
                    .as_ref()
                    .map(|vm| vm.parallel_signature(name).unwrap())
                    .unwrap_or_else(|| entries[index].signature().clone());
                for variant in 0..if *name == "capacity" { 1 } else { 2 } {
                    let (input, expected) = match *name {
                        "root" => {
                            let v = array_tests::input(&sig.params[0], variant);
                            (vec![v.clone()], array_tests::expected(&sig, v))
                        }
                        "units" => {
                            let v = TaskValue::Array {
                                ty: sig.params[0].clone(),
                                elements: vec![TaskValue::Scalar(InstanceValue::Unit); variant * 3],
                            };
                            (vec![v.clone()], v)
                        }
                        _ => (vec![], TaskValue::Scalar(InstanceValue::UInt64(16))),
                    };
                    let budget = TaskBudget::new(1 << 20);
                    let prepared = if let Some(vm) = &vm {
                        vm.prepare_parallel_task(name, input, vec![budget.clone()])
                            .unwrap()
                    } else {
                        unsafe {
                            entries[index].prepare(
                                *library
                                    .get::<CpsEntry>(entries[index].symbol().as_bytes())
                                    .unwrap(),
                                library.clone(),
                                input,
                                vec![budget.clone()],
                            )
                        }
                        .unwrap()
                    };
                    let task = prepared.submit(&pool).unwrap();
                    task.wait().unwrap();
                    let output = task.take_result().unwrap();
                    assert_eq!(output.value(), &expected, "{mode:?}, {name}, {variant}");
                    assert!(task.take_result().is_err());
                    drop((output, task));
                    completed.push((budget, *name, variant));
                }
            }
            // A completed task has released its language values, but the worker
            // may still retain the terminal record and its storage metadata.
            pool.wait_closed();
            for (budget, name, variant) in completed {
                assert_eq!(budget.used(), 0, "{mode:?}, {name}, {variant}");
            }
        }
    }
    let entry = entries.last().unwrap();
    let expected = array_tests::input(&entry.signature().params[0], 1);
    let budget = TaskBudget::new(1 << 20);
    let weak = Arc::downgrade(&library);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![expected.clone()],
            vec![budget.clone()],
        )
    }
    .unwrap();
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = prepared.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(output.value(), &expected);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn task_fixed_array_values_on_all_backends() {
    use crate::vm::{parallel::fixed_array_tests as cases, InstanceValue, VmEngine, VmMode};
    let names = ["root", "dynamic", "empty", "units", "parallel_local"];
    let (_scratch, entries, library) =
        linked_task_entries(cases::SOURCE, &names, "task-fixed-arrays");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(cases::SOURCE).unwrap();
        }
        for workers in [1, 3] {
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
            let mut budgets = Vec::new();
            for (index, name) in names.iter().enumerate() {
                let sig = vm
                    .as_ref()
                    .map(|vm| vm.parallel_signature(name).unwrap())
                    .unwrap_or_else(|| entries[index].signature().clone());
                for variant in 0..if *name == "root" { 2 } else { 1 } {
                    let (input, expected) = match *name {
                        "root" => {
                            let v = cases::input(&sig.params[0], variant);
                            (vec![v.clone()], v)
                        }
                        "dynamic" => {
                            let ty = &sig.params[0];
                            let row = ty.array_element_type().unwrap();
                            let v = TaskValue::Array {
                                ty: ty.clone(),
                                elements: vec![TaskValue::FixedArray {
                                    ty: row.clone(),
                                    elements: vec![TaskValue::Scalar(InstanceValue::Int32(42)); 3],
                                }],
                            };
                            (vec![v.clone()], v)
                        }
                        "empty" | "units" => {
                            let v = TaskValue::FixedArray {
                                ty: sig.params[0].clone(),
                                elements: vec![
                                    TaskValue::Scalar(InstanceValue::Unit);
                                    if *name == "units" { 3 } else { 0 }
                                ],
                            };
                            (vec![v.clone()], v)
                        }
                        _ => (vec![], TaskValue::Scalar(InstanceValue::Int32(42))),
                    };
                    let budget = TaskBudget::new(1 << 20);
                    let prepared = if let Some(vm) = &vm {
                        vm.prepare_parallel_task(name, input, vec![budget.clone()])
                            .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}"))
                    } else {
                        unsafe {
                            entries[index].prepare(
                                *library
                                    .get::<CpsEntry>(entries[index].symbol().as_bytes())
                                    .unwrap(),
                                library.clone(),
                                input,
                                vec![budget.clone()],
                            )
                        }
                        .unwrap()
                    };
                    let task = prepared.submit(&pool).unwrap();
                    task.wait()
                        .unwrap_or_else(|e| panic!("{mode:?} {name} N={workers}: {e:?}"));
                    let output = task.take_result().unwrap();
                    assert_eq!(output.value(), &expected, "{mode:?} {name} N={workers}");
                    drop((output, task));
                    budgets.push((name, budget));
                }
            }
            // Completion ends execution; the worker can retain its task record
            // until the polling turn unwinds. Join workers before checking metadata.
            drop(pool);
            for (name, budget) in budgets {
                assert_eq!(budget.used(), 0, "{mode:?} {name}");
            }
        }
    }
    let entry = &entries[0];
    let value = cases::input(&entry.signature().params[0], 1);
    let budget = TaskBudget::new(1 << 20);
    let weak = Arc::downgrade(&library);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![value.clone()],
            vec![budget.clone()],
        )
    }
    .unwrap();
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = prepared.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(output.value(), &value);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn task_parallel_list_values_and_roots_on_all_backends() {
    use crate::vm::{parallel::list_tests, InstanceValue, VmEngine, VmMode};
    let names = [
        "root",
        "units",
        "capacity",
        "topology",
        "emptied",
        "nested",
        "failed",
        "cancelled",
        "identity",
    ];
    let (_scratch, entries, library) =
        linked_task_entries(list_tests::SOURCE, &names, "task-lists");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(list_tests::SOURCE).unwrap();
        }
        for workers in [1, 3] {
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
            let mut budgets = Vec::new();
            for (index, name) in names[..8].iter().enumerate() {
                let sig = vm
                    .as_ref()
                    .map(|vm| vm.parallel_signature(name).unwrap())
                    .unwrap_or_else(|| entries[index].signature().clone());
                for variant in 0..if matches!(*name, "root" | "units" | "nested") {
                    2
                } else {
                    1
                } {
                    let (input, expected) = match *name {
                        "root" => {
                            let v = list_tests::input(&sig.params[0], variant);
                            (vec![v.clone()], list_tests::expected(&sig, v))
                        }
                        "units" => {
                            let v = TaskValue::List {
                                ty: sig.params[0].clone(),
                                elements: vec![TaskValue::Scalar(InstanceValue::Unit); variant * 3],
                            };
                            (vec![v.clone()], v)
                        }
                        "topology" => (vec![], TaskValue::Scalar(InstanceValue::Int32(42))),
                        "emptied" => (
                            vec![],
                            TaskValue::List {
                                ty: sig.ret.clone(),
                                elements: vec![],
                            },
                        ),
                        "nested" => {
                            let v = TaskValue::Array {
                                ty: sig.params[0].clone(),
                                elements: vec![TaskValue::List {
                                    ty: sig.params[0].array_element_type().unwrap().clone(),
                                    elements: vec![TaskValue::String("nested".into()); variant * 2],
                                }],
                            };
                            (vec![v.clone()], v)
                        }
                        "failed" | "cancelled" => (
                            vec![list_tests::input(&sig.params[0], 1)],
                            TaskValue::Scalar(InstanceValue::Unit),
                        ),
                        _ => (vec![], TaskValue::Scalar(InstanceValue::UInt64(16))),
                    };
                    let budget = TaskBudget::new(1 << 20);
                    let prepared = if let Some(vm) = &vm {
                        vm.prepare_parallel_task(name, input, vec![budget.clone()])
                            .unwrap()
                    } else {
                        unsafe {
                            entries[index].prepare(
                                *library
                                    .get::<CpsEntry>(entries[index].symbol().as_bytes())
                                    .unwrap(),
                                library.clone(),
                                input,
                                vec![budget.clone()],
                            )
                        }
                        .unwrap()
                    };
                    let task = prepared.submit(&pool).unwrap();
                    if *name == "cancelled" {
                        task.request_cancel();
                    }
                    task.wait().unwrap();
                    if matches!(*name, "failed" | "cancelled") {
                        let error = task.take_result().err().unwrap();
                        if *name == "cancelled" {
                            assert_eq!(error.code(), crate::ErrorCode::Cancelled);
                        } else {
                            assert!(error.to_string().contains("Division by zero"), "{error}");
                        }
                    } else {
                        let output = task.take_result().unwrap();
                        assert_eq!(output.value(), &expected, "{mode:?}, {name}, {variant}");
                    }
                    assert!(task.take_result().is_err());
                    drop(task);
                    budgets.push(budget);
                }
            }
            drop(pool);
            for budget in budgets {
                assert_eq!(budget.used(), 0);
            }
        }
    }
    let entry = entries.last().unwrap();
    let expected = list_tests::input(&entry.signature().params[0], 1);
    let budget = TaskBudget::new(1 << 20);
    let weak = Arc::downgrade(&library);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![expected.clone()],
            vec![budget.clone()],
        )
    }
    .unwrap();
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = prepared.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(output.value(), &expected);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn task_parallel_map_values_and_roots_on_all_backends() {
    use crate::vm::{parallel::map_tests as cases, InstanceValue, VmEngine, VmMode};
    let names = [
        "root",
        "roundtrip",
        "factory",
        "set_factory",
        "echo_map",
        "echo_set",
        "topology",
        "verify",
        "empty",
        "units",
        "nested",
        "failed",
        "cancelled",
        "emptied",
        "indices",
    ];
    let (_scratch, entries, library) = linked_task_entries(cases::SOURCE, &names, "task-maps");
    let mut aot_snapshot = None;
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(cases::SOURCE)
                .unwrap_or_else(|e| panic!("{e}"));
        }
        let signature = |name| {
            let index = names.iter().position(|n| *n == name).unwrap();
            vm.as_ref()
                .map(|vm| vm.parallel_signature(name).unwrap())
                .unwrap_or_else(|| entries[index].signature().clone())
        };
        for workers in [1, 3] {
            let pool = crate::runtime::parallel::ParallelExecutor::new(workers, 64).unwrap();
            let mut budgets = Vec::new();
            let mut run = |name: &str, input: Vec<TaskValue>| {
                let budget = TaskBudget::new(cases::BUDGET);
                let index = names.iter().position(|n| *n == name).unwrap();
                let prepared = if let Some(vm) = &vm {
                    vm.prepare_parallel_task(name, input, vec![budget.clone()])
                        .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}"))
                } else {
                    unsafe {
                        entries[index].prepare(
                            *library
                                .get::<CpsEntry>(entries[index].symbol().as_bytes())
                                .unwrap(),
                            library.clone(),
                            input,
                            vec![budget.clone()],
                        )
                    }
                    .unwrap()
                };
                let task = prepared.submit(&pool).unwrap();
                if name == "cancelled" {
                    task.request_cancel();
                }
                task.wait().unwrap();
                let result = task.take_result();
                let value = if name == "failed" || name == "cancelled" {
                    let error = result.err().unwrap();
                    if name == "cancelled" {
                        assert_eq!(error.code(), crate::ErrorCode::Cancelled);
                    } else {
                        assert!(error.to_string().contains("Division by zero"), "{error}");
                    }
                    None
                } else {
                    Some(
                        result
                            .unwrap_or_else(|e| panic!("{mode:?} {name} N={workers}: {e}"))
                            .into_value(),
                    )
                };
                assert!(task.take_result().is_err());
                drop(task);
                budgets.push(budget);
                value
            };
            let value = run("root", vec![]).unwrap();
            cases::check_bundle(&value);
            assert_eq!(run("roundtrip", vec![value.clone()]).unwrap(), value);
            for name in ["topology", "verify"] {
                assert_eq!(
                    run(
                        name,
                        if name == "verify" {
                            vec![value.clone()]
                        } else {
                            vec![]
                        }
                    )
                    .unwrap(),
                    TaskValue::Scalar(InstanceValue::Int32(42)),
                    "{mode:?} {name}"
                );
            }
            for (factory, echo, set) in [
                ("factory", "echo_map", false),
                ("set_factory", "echo_set", true),
            ] {
                let ty = signature(factory).params.remove(0);
                for populated in [false, true] {
                    let result = run(factory, vec![cases::input(&ty, populated, set)]).unwrap();
                    cases::check_factory(&result, populated, set);
                    assert_eq!(run(echo, vec![result.clone()]).unwrap(), result);
                }
            }
            for (name, length) in [("empty", 0), ("emptied", 0), ("units", 1)] {
                let result = run(name, vec![]).unwrap();
                let TaskValue::Tuple(parts) = result else {
                    panic!("expected tuple")
                };
                let TaskValue::Map(map) = &parts[0] else {
                    panic!("expected Map")
                };
                let TaskValue::Set(set) = &parts[1] else {
                    panic!("expected Set")
                };
                assert_eq!(map.len(), length);
                assert_eq!(set.len(), length);
            }
            let mixed = run("indices", vec![]).unwrap();
            let TaskValue::Tuple(parts) = mixed else {
                unreachable!()
            };
            let TaskValue::Map(map) = &parts[0] else {
                unreachable!()
            };
            let TaskValue::Set(set) = &parts[1] else {
                unreachable!()
            };
            assert_eq!(map.len(), 14);
            assert_eq!(set.len(), 14);
            for (key, value) in map.entries() {
                let TaskValue::Scalar(InstanceValue::Int32(i)) = value else {
                    unreachable!()
                };
                assert!(*i >= 0 && *i < 20 && (*i == 0 || *i % 3 != 0));
                assert_eq!(key, &TaskValue::Scalar(InstanceValue::Int32(i * 7)));
                assert!(set.elements().contains(key));
            }
            let ty = signature("nested").params.remove(0);
            let list = ty.array_element_type().unwrap().clone();
            let TaskValue::Tuple(parts) = &value else {
                unreachable!()
            };
            let nested = TaskValue::Array {
                ty,
                elements: vec![TaskValue::List {
                    ty: list,
                    elements: vec![parts[0].clone()],
                }],
            };
            assert_eq!(run("nested", vec![nested.clone()]).unwrap(), nested);
            run("failed", vec![value.clone()]);
            run("cancelled", vec![value.clone()]);
            if mode.is_none() {
                aot_snapshot = Some(value);
            }
            drop(pool);
            for budget in budgets {
                assert_eq!(budget.used(), 0, "{mode:?} N={workers}");
            }
        }
    }
    let expected = aot_snapshot.unwrap();
    let budget = TaskBudget::new(cases::BUDGET);
    let entry = &entries[1];
    let weak = Arc::downgrade(&library);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![expected.clone()],
            vec![budget.clone()],
        )
    }
    .unwrap();
    drop(library);
    let output = std::thread::spawn(move || {
        let mut task = prepared.start().unwrap();
        while !task.resume().unwrap() {}
        task.take_result().unwrap()
    })
    .join()
    .unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(output.value(), &expected);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn scoped_borrow_aot_failure_and_cancellation_release_storage() {
    use std::time::{Duration, Instant};
    const SOURCE: &str = include_str!("../../../../tests/fixtures/scoped_borrow_join.jk");
    let (_scratch, entries, library) = linked_task_entries(
        SOURCE,
        &[
            "failed",
            "parked",
            "forwarded_failed",
            "forwarded_parked",
            "dynamic_failed",
            "dynamic_parked",
            "aggregate_failed",
            "aggregate_parked",
        ],
        "scoped-stop",
    );
    for workers in [1, 3] {
        for (index, entry) in entries.iter().enumerate() {
            let budget = TaskBudget::new(4 << 20);
            let prepared = unsafe {
                entry.prepare(
                    *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                    library.clone(),
                    vec![],
                    vec![budget.clone()],
                )
            }
            .unwrap();
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 16).unwrap();
            let task = prepared.submit(&pool).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            if index % 2 == 1 {
                loop {
                    let observations =
                        crate::runtime::parallel::scoped::tree_observations(task.identity());
                    let readers = observations
                        .iter()
                        .filter(|o| !o.source && (index != 3 || o.parent != task.identity()))
                        .count();
                    if readers == 2 {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "AOT borrowed descendants did not start"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                task.request_cancel();
            }
            while !task.is_finished() {
                assert!(Instant::now() < deadline, "AOT borrowed task did not drain");
                std::thread::sleep(Duration::from_millis(1));
            }
            let error = task.take_result().err().unwrap();
            if index % 2 == 1 {
                assert_eq!(error.code(), crate::ErrorCode::Cancelled);
            } else {
                assert_eq!(error.code(), crate::ErrorCode::RuntimeError, "{error}");
            }
            drop(task);
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{workers}, {index}");
        }
    }
}

#[test]
fn scoped_borrow_nested_race_and_isolate_on_all_backends() {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    const SOURCE: &str = include_str!("../../../../tests/fixtures/scoped_borrow_join.jk");
    let cases = [
        ("nested_race", 91),
        ("borrowed_race", 68),
        ("nested_isolate", 42),
        ("isolated_results", 7),
        ("forwarded", 145),
        ("forwarded_race", 94),
        ("forwarded_isolate", 94),
        ("dynamic_shared", 42),
        ("dynamic_return", 26),
        ("dynamic_isolate", 42),
        ("dynamic_views", 12),
        ("dynamic_mixed", 9),
        ("dynamic_break_continue", 44),
        ("aggregate_readers", 109),
        ("aggregate_capture", 30),
        ("aggregate_zero", 9),
        ("aggregate_layouts", 12),
    ];
    let names: Vec<_> = cases.iter().map(|(name, _)| *name).collect();
    let (_scratch, entries, library) = linked_task_entries(SOURCE, &names, "scoped-nested");
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        }
        for workers in [1, 3] {
            let budget = TaskBudget::new(4 << 20);
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 32).unwrap();
            for (index, (name, expected)) in cases.iter().enumerate() {
                let prepared = if let Some(vm) = &vm {
                    vm.prepare_parallel_task(name, vec![], vec![budget.clone()])
                        .unwrap_or_else(|e| panic!("{e}"))
                } else {
                    let entry = &entries[index];
                    unsafe {
                        entry.prepare(
                            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                            library.clone(),
                            vec![],
                            vec![budget.clone()],
                        )
                    }
                    .unwrap()
                };
                let task = prepared.submit(&pool).unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while !task.is_finished() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{mode:?}/{workers}/{name}: stuck"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert_eq!(
                    task.take_result()
                        .unwrap_or_else(|e| panic!("{mode:?}/{workers}/{name}: {e}"))
                        .value(),
                    &TaskValue::Scalar(InstanceValue::Int32(*expected))
                );
                let observations = crate::runtime::parallel::scoped::observations(task.identity());
                assert_eq!(
                    observations.iter().filter(|o| o.source).count(),
                    match *name {
                        "forwarded" | "dynamic_views" => 3,
                        "dynamic_shared" => 6,
                        "aggregate_readers" => 5,
                        "aggregate_capture" => 1,
                        "dynamic_return" => 1,
                        _ => 2,
                    },
                    "{mode:?}/{workers}/{name}: group stayed local"
                );
                if name.starts_with("dynamic") || name.starts_with("aggregate") {
                    let tree = crate::runtime::parallel::scoped::tree_observations(task.identity());
                    for reader in tree.iter().filter(|o| !o.source) {
                        assert_eq!(reader.origin_parent, task.identity());
                        let sender = tree
                            .iter()
                            .find(|o| {
                                o.source
                                    && o.scope == reader.scope
                                    && o.data == reader.data
                                    && o.len == reader.len
                            })
                            .unwrap();
                        assert_ne!(sender.header, reader.header);
                    }
                    if *name == "dynamic_shared" {
                        assert!(
                            tree.iter().any(|o| o.source && o.parent != task.identity()),
                            "nested dynamic capture stayed local"
                        );
                    }
                }
                if name.starts_with("forwarded") {
                    let tree = crate::runtime::parallel::scoped::tree_observations(task.identity());
                    assert!(
                        tree.iter().any(|o| o.source && o.parent != task.identity()),
                        "{mode:?}/{name}: forwarding stayed local"
                    );
                    let original_scope = observations.iter().find(|o| o.source).unwrap().scope;
                    for reader in tree.iter().filter(|o| !o.source) {
                        assert_eq!(
                            (reader.origin_parent, reader.origin_scope),
                            (task.identity(), original_scope)
                        );
                        let sender = tree
                            .iter()
                            .find(|o| {
                                o.source
                                    && o.scope == reader.scope
                                    && o.data == reader.data
                                    && o.len == reader.len
                            })
                            .unwrap();
                        assert_ne!(reader.header, sender.header, "headers crossed workers");
                    }
                    if *name == "forwarded" {
                        let mut deepest = 0;
                        for reader in tree.iter().filter(|o| !o.source) {
                            let mut task = reader.task;
                            let mut depth = 0;
                            while let Some(input) =
                                tree.iter().find(|o| !o.source && o.task == task)
                            {
                                depth += 1;
                                task = input.parent;
                            }
                            deepest = deepest.max(depth);
                        }
                        assert!(deepest >= 4, "multi-level relay stayed local");
                        assert!(tree
                            .iter()
                            .any(|o| o.source && o.len == 0 && o.parent != task.identity()));
                    }
                }
                drop(task);
            }
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{workers}");
        }
    }
}

#[test]
fn scoped_borrow_callbacks_on_all_backends() {
    const SOURCE: &str = include_str!("../../../../tests/fixtures/scoped_borrow_callbacks.jk");
    // Expected value and number of borrowed inputs sent by the root.
    let cases = [
        ("main", 39, 2),
        ("dynamic", 42, 1),
        ("isolated", 10, 2),
        ("nested_indirect_candidates", 11, 2),
        ("indirect_group", 20, 2),
        ("failed", 0, 2),
        ("parked", 0, 1),
    ];
    scoped_borrow_cases(SOURCE, &cases, "scoped-callbacks");
}

#[test]
fn scoped_borrow_plain_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_plain.jk"),
        &[
            ("main", 33, 2),
            ("dynamic", 42, 1),
            ("arrays", 21, 2),
            ("zero", 1, 2),
            ("matrices", 42, 2),
            ("generic", 42, 2),
            ("owned_source", 42, 1),
            ("isolated", 7, 2),
            ("reuse", 56, 16),
            ("failed", 0, 2),
            ("parked", 0, 1),
        ],
        "scoped-plain",
    );
}

#[test]
fn scoped_borrow_owners_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_owners.jk"),
        &[
            ("main", 54, 2),
            ("dynamic", 42, 1),
            ("strings", 10, 2),
            ("transparent", 6, 2),
            ("tuples", 14, 2),
            ("capacities", 56, 2),
            ("failed", 0, 2),
            ("parked", 0, 1),
            ("isolated", 27, 2),
        ],
        "scoped-owners",
    );
}

fn scoped_borrow_cases(source: &str, cases: &[(&str, i32, usize)], label: &str) {
    use crate::vm::{InstanceValue, VmEngine, VmMode};
    use std::time::{Duration, Instant};
    let names: Vec<_> = cases.iter().map(|(name, _, _)| *name).collect();
    let (_scratch, entries, library) = linked_task_entries(source, &names, label);
    for mode in [Some(VmMode::Jit), Some(VmMode::Interpreter), None] {
        let mut vm = mode.map(VmEngine::with_mode);
        if let Some(vm) = &mut vm {
            vm.set_prelude_enabled(false);
            vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        }
        for workers in [1, 3] {
            let budget = TaskBudget::new(4 << 20);
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(workers, 32).unwrap();
            for (index, (name, expected, inputs)) in cases.iter().enumerate() {
                let prepared = if let Some(vm) = &vm {
                    vm.prepare_parallel_task(name, vec![], vec![budget.clone()])
                        .unwrap()
                } else {
                    let entry = &entries[index];
                    unsafe {
                        entry.prepare(
                            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                            library.clone(),
                            vec![],
                            vec![budget.clone()],
                        )
                    }
                    .unwrap()
                };
                let task = prepared.submit(&pool).unwrap();
                let deadline = Instant::now() + Duration::from_secs(20);
                if *name == "parked" {
                    while !crate::runtime::parallel::scoped::observations(task.identity())
                        .iter()
                        .any(|o| !o.source)
                    {
                        assert!(
                            Instant::now() < deadline,
                            "{mode:?}/{workers}: reader did not start"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    task.request_cancel();
                }
                while !task.is_finished() {
                    assert!(
                        Instant::now() < deadline,
                        "{mode:?}/{workers}/{name}: stuck"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                let result = task.take_result();
                if matches!(
                    *name,
                    "failed" | "enum_failed" | "partial_failure" | "parked"
                ) {
                    let error = result.err().unwrap();
                    assert_eq!(
                        error.code(),
                        if matches!(*name, "failed" | "enum_failed" | "partial_failure") {
                            crate::ErrorCode::RuntimeError
                        } else {
                            crate::ErrorCode::Cancelled
                        },
                        "{mode:?}/{workers}/{name}: {error}"
                    );
                } else {
                    assert_eq!(
                        result
                            .unwrap_or_else(|e| panic!("{mode:?}/{workers}/{name}: {e}"))
                            .value(),
                        &TaskValue::Scalar(InstanceValue::Int32(*expected))
                    );
                }
                let observations = crate::runtime::parallel::scoped::observations(task.identity());
                assert_eq!(
                    observations.iter().filter(|o| o.source).count(),
                    *inputs,
                    "{mode:?}/{workers}/{name}: admission differs"
                );
                let tree = crate::runtime::parallel::scoped::tree_observations(task.identity());
                if label == "scoped-owners" && *name == "main" {
                    assert_eq!(
                        tree.iter().filter(|o| o.source).count(),
                        18,
                        "nested owner or projected String stayed local"
                    );
                    let leaves: Vec<_> = tree
                        .iter()
                        .filter(|o| o.source && !o.copied && o.len == 3)
                        .collect();
                    assert_eq!(leaves.len(), 8);
                    assert!(
                        leaves.iter().all(|leaf| leaf.data == leaves[0].data),
                        "String bytes were copied with owner tables"
                    );
                    assert!(tree.iter().any(|o| !o.source && o.copied && o.plain));
                }
                for reader in tree.iter().filter(|o| !o.source) {
                    let sender = tree
                        .iter()
                        .find(|o| {
                            o.source
                                && o.scope == reader.scope
                                && o.data == reader.data
                                && o.len == reader.len
                        })
                        .unwrap();
                    if reader.plain && !reader.copied {
                        assert_eq!(
                            reader.header, sender.header,
                            "ordinary reference payload was copied"
                        );
                        assert_eq!(reader.header, reader.data);
                    } else {
                        assert_ne!(sender.header, reader.header);
                    }
                    assert_eq!(reader.origin_parent, sender.origin_parent);
                    if *name != "owned_source" {
                        assert_eq!(reader.origin_parent, task.identity());
                    }
                }
                if *name == "owned_source" {
                    assert_eq!(
                        tree.iter().filter(|o| o.source).count(),
                        3,
                        "moved array element stayed local"
                    );
                }
                if *name == "main" {
                    assert!(
                        tree.iter().any(|o| o.source && o.parent != task.identity()),
                        "nested reader stayed local"
                    );
                    let sender = observations.iter().find(|o| o.source).unwrap();
                    if workers > 1 {
                        assert!(
                            observations
                                .iter()
                                .any(|o| !o.source && o.thread != sender.thread),
                            "reader stayed on parent worker"
                        );
                    }
                }
                drop(task);
            }
            pool.wait_closed();
            assert_eq!(budget.used(), 0, "{mode:?}/{workers}");
        }
    }
}

#[test]
fn fixed_array_owned_elements_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/fixed_array_owned.jk"),
        &[
            ("suite", 150, 0),
            ("suspended", 10, 0),
            ("transferred", 20, 0),
            ("borrowed", 20, 2),
            ("dynamic", 10, 1),
            ("dynamic_moved", 10, 0),
            ("failed", 0, 0),
            ("enum_failed", 0, 0),
            ("parked", 0, 1),
        ],
        "fixed-owned",
    );
}

#[test]
fn scoped_borrow_enums_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_enums.jk"),
        &[
            ("main", 12, 2),
            ("arrays", 12, 2),
            ("results", 17, 2),
            ("plain_values", 42, 3),
            ("dynamic", 2, 1),
            ("fixed_borrow", 10, 2),
            ("fixed_move", 12, 0),
            ("fixed_buffered", 5, 0),
            ("failed", 0, 2),
            ("parked", 0, 1),
            ("isolated", 3, 2),
            ("raced", 4, 2),
        ],
        "scoped-enums",
    );
}

#[test]
fn scoped_borrow_collections_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_collections.jk"),
        &[
            ("main", 78, 2),
            ("maps", 20, 2),
            ("sets", 12, 2),
            ("nested", 110, 2),
            ("dynamic", 42, 1),
            ("empty", 0, 6),
            ("failed", 0, 2),
            ("parked", 0, 1),
            ("raced", 55, 2),
            ("isolated", 55, 4),
            ("local_cursor", 30, 0),
            ("swapped_views", 5, 2),
        ],
        "scoped-collections",
    );
}

#[test]
fn scoped_borrow_indirect_entries_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_indirect.jk"),
        &[
            ("main", 13, 2),
            ("forwarded", 13, 2),
            ("generic_forwarded", 12, 2),
            ("dynamic", 42, 1),
            ("failed", 0, 2),
            ("parked", 0, 1),
            ("raced", 6, 2),
            ("isolated", 3, 2),
            ("owned_result", 6, 2),
            ("mixed", 16, 1),
        ],
        "scoped-indirect",
    );
}

#[test]
fn scoped_borrow_indirect_unselected_candidate_keeps_group_local() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_indirect_local.jk"),
        &[("local", 6, 0)],
        "scoped-indirect-local",
    );
}

#[test]
fn fixed_array_collections_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/fixed_array_collections.jk"),
        &[
            ("suite", 42, 0),
            ("main", 42, 2),
            ("transferred", 42, 4),
            ("dynamic", 42, 1),
            ("dynamic_moved", 42, 0),
            ("failed", 0, 2),
            ("partial_failure", 0, 0),
            ("parked", 0, 1),
            ("raced", 21, 2),
            ("isolated", 42, 2),
        ],
        "fixed-collections",
    );
}

#[test]
fn scoped_borrow_custom_drop_on_all_backends() {
    scoped_borrow_cases(
        include_str!("../../../../tests/fixtures/scoped_borrow_drop.jk"),
        &[
            ("main", 42, 2),
            ("dynamic", 42, 1),
            ("containers", 12, 4),
            ("failed", 0, 2),
            ("parked", 0, 1),
            ("raced", 21, 2),
            ("isolated", 42, 2),
        ],
        "scoped-drop",
    );
}

#[test]
fn random_aot_shared_runtime_failures_and_parallel_ownership() {
    use crate::runtime::{arena::LIVE, buffer::random::testing::Guard};
    const SOURCE: &str = include_str!("../../../../tests/fixtures/random.jk");
    let (_scratch, entries, library) =
        linked_task_entries(SOURCE, &["random_parallel"], "random-shared");
    let call = |name: &[u8]| {
        let entry = unsafe { library.get::<unsafe extern "C" fn() -> i32>(name).unwrap() };
        let value = crate::runtime::error::checked(|| unsafe { entry() }).unwrap();
        assert_eq!(value, 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    };
    for successes in [0, 1] {
        let guard = Guard::new(successes);
        call(b"random_zero");
        assert_eq!(guard.calls(), 0);
        call(b"random_failure");
        assert_eq!(guard.calls(), successes + 1);
    }
    {
        let guard = Guard::new(usize::MAX);
        call(b"random_pattern");
        assert_eq!(guard.calls(), 2);
        struct RestoreLimit(usize);
        impl Drop for RestoreLimit {
            fn drop(&mut self) {
                crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
            }
        }
        let _restore = RestoreLimit(crate::runtime::arena::LIMIT.with(|n| n.replace(8192)));
        call(b"random_repeated");
        assert_eq!(guard.calls(), 130);
    }
    let entry = &entries[0];
    let budget = TaskBudget::new(128 << 10);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![],
            vec![budget.clone()],
        )
    }
    .unwrap();
    let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 8).unwrap();
    let task = prepared.submit(&pool).unwrap();
    task.wait().unwrap();
    let result = task.take_result().unwrap();
    assert_eq!(
        result.value(),
        &TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
    );
    drop(result);
    drop(task);
    pool.wait_closed();
    assert_eq!(budget.used(), 0);
}

#[test]
fn uuid_aot_shared_runtime_failures_and_parallel_ownership() {
    use crate::runtime::{arena::LIVE, buffer::random::testing::Guard};
    for (source, prefix, entropy_calls) in [
        (
            include_str!("../../../../tests/fixtures/uuid.jk"),
            "uuid",
            1,
        ),
        (
            include_str!("../../../../tests/fixtures/uuid_v7.jk"),
            "uuid_v7",
            2,
        ),
    ] {
        let (_scratch, entries, library) =
            linked_task_entries(source, &[&format!("{prefix}_parallel")], "uuid-shared");
        let call = |name: &[u8]| {
            let entry = unsafe { library.get::<unsafe extern "C" fn() -> i32>(name).unwrap() };
            let value = crate::runtime::error::checked(|| unsafe { entry() }).unwrap();
            assert_eq!(value, 42);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        };
        {
            let guard = Guard::new(0);
            if prefix == "uuid_v7" {
                call(b"uuid_v7_invalid_time");
                assert_eq!(guard.calls(), 0);
            }
            call(format!("{prefix}_failure").as_bytes());
            assert_eq!(guard.calls(), entropy_calls);
        }
        {
            let guard = Guard::new(usize::MAX);
            call(format!("{prefix}_pattern").as_bytes());
            assert_eq!(guard.calls(), entropy_calls);
            struct RestoreLimit(usize);
            impl Drop for RestoreLimit {
                fn drop(&mut self) {
                    crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
                }
            }
            let _restore = RestoreLimit(crate::runtime::arena::LIMIT.with(|n| n.replace(8192)));
            call(format!("{prefix}_repeated").as_bytes());
            assert_eq!(guard.calls(), entropy_calls + 128);
        }
        let entry = &entries[0];
        let budget = TaskBudget::new(128 << 10);
        let prepared = unsafe {
            entry.prepare(
                *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                library.clone(),
                vec![],
                vec![budget.clone()],
            )
        }
        .unwrap();
        let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 8).unwrap();
        let task = prepared.submit(&pool).unwrap();
        task.wait().unwrap();
        let result = task.take_result().unwrap();
        assert_eq!(
            result.value(),
            &TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
        );
        drop(result);
        drop(task);
        pool.wait_closed();
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn crypto_aot_shared_runtime_repeated_outputs_and_parallel_cleanup() {
    let (_scratch, entries, library) = linked_task_entries(
        include_str!("../../../../tests/fixtures/crypto.jk"),
        &[
            "crypto_stream_parallel",
            "crypto_stream_pending",
            "crypto_stream_failed",
        ],
        "crypto-shared",
    );
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::runtime::arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    {
        let _restore = Restore(crate::runtime::arena::LIMIT.with(|n| n.replace(8192)));
        let entry = unsafe {
            library
                .get::<unsafe extern "C" fn() -> i32>(b"crypto_stream_repeated")
                .unwrap()
        };
        assert_eq!(
            crate::runtime::error::checked(|| unsafe { entry() }).unwrap(),
            42
        );
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
    // Poll to an actual suspension before dropping, so cancellation exercises
    // live hash/MAC owners rather than only an unstarted frame.
    for (index, finish) in [(1, false), (1, true), (2, true)] {
        let entry = &entries[index];
        let budget = TaskBudget::new(128 << 10);
        let prepared = unsafe {
            entry.prepare(
                *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                library.clone(),
                vec![],
                vec![budget.clone()],
            )
        }
        .unwrap();
        let mut task = prepared.start().unwrap();
        assert!(!task.resume().unwrap());
        assert!(budget.used() > 0);
        if finish {
            if index == 2 {
                assert_eq!(
                    task.resume().unwrap_err().code(),
                    crate::ErrorCode::RuntimeError
                );
            } else {
                while !task.resume().unwrap() {}
                assert_eq!(
                    task.take_result().unwrap().value(),
                    &TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
                );
            }
        }
        drop(task);
        assert_eq!(budget.used(), 0);
    }
    let entry = &entries[0];
    let budget = TaskBudget::new(128 << 10);
    let prepared = unsafe {
        entry.prepare(
            *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
            library.clone(),
            vec![],
            vec![budget.clone()],
        )
    }
    .unwrap();
    let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 8).unwrap();
    let task = prepared.submit(&pool).unwrap();
    task.wait().unwrap();
    let result = task.take_result().unwrap();
    assert_eq!(
        result.value(),
        &TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
    );
    drop(result);
    drop(task);
    pool.wait_closed();
    assert_eq!(budget.used(), 0);
}

#[test]
fn password_aot_generated_jobs_cancel_fail_and_refund_budgets() {
    let _serial = crate::runtime::password::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_scratch, entries, library) = linked_task_entries(
        include_str!("../../../../tests/fixtures/password.jk"),
        &["password_suite", "password_once", "password_failed"],
        "password-shared",
    );
    let prepare = |index: usize, budget: TaskBudget| unsafe {
        let entry = &entries[index];
        entry
            .prepare(
                *library.get::<CpsEntry>(entry.symbol().as_bytes()).unwrap(),
                library.clone(),
                vec![],
                vec![budget],
            )
            .unwrap()
    };
    let budget = TaskBudget::new(64 << 20);
    let mut pool = crate::runtime::parallel::ParallelExecutor::new(2, 8).unwrap();
    let task = prepare(0, budget.clone()).submit(&pool).unwrap();
    task.wait().unwrap();
    assert_eq!(
        task.take_result().unwrap().value(),
        &TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
    );
    drop(task);
    let failed = prepare(2, budget.clone()).submit(&pool).unwrap();
    failed.wait().unwrap();
    assert_eq!(
        failed.take_result().err().unwrap().code(),
        crate::ErrorCode::RuntimeError
    );
    drop(failed);
    pool.wait_closed();
    assert_eq!(budget.used(), 0);
    // Hold the worker so the first poll cannot race with completion.
    let paused = crate::runtime::password::tests::pause_worker();
    let mut pending = prepare(1, budget.clone()).start().unwrap();
    assert!(!pending.resume().unwrap());
    assert!(budget.used() > 256 * 1024);
    drop(paused);
    drop(pending);
    assert_eq!(budget.used(), 0);
}
