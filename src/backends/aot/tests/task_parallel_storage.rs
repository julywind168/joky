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
        .unwrap();
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
        "__joky_task_yield_now".into(),
        crate::stdlib::task::yield_internal as *const u8,
    );
    symbols.insert(
        "__joky_task_sleep_ms".into(),
        crate::stdlib::task::sleep_internal as *const u8,
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
                    assert_eq!(budget.used(), 0, "{mode:?}, {name}");
                }
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
