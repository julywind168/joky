//! TP2: real linked code and owning storage travel independently of the compiler.
use super::{instances::shared_runtime_object, AotCompiler, ScratchDir};
use crate::{
    runtime::{cps::CpsEntry, parallel::TaskBudget},
    vm::TaskValue,
};
use std::{collections::HashMap, process::Command, sync::Arc};

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
    assert!(
        weak.upgrade().is_none(),
        "code can retire after task cleanup"
    );
    drop(output);
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
    let entries: Vec<_> = ["joined", "raced", "branched", "cancel_loop"]
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
        for (index, name) in ["joined", "raced", "branched", "cancel_loop"]
            .into_iter()
            .enumerate()
        {
            GATE.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()))
                .0
                .lock()
                .unwrap()
                .clear();
            let executor = ParallelExecutor::new(2, 64).unwrap();
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
            task.wait().unwrap();
            let result = task.take_result().unwrap().into_value();
            let TaskValue::Scalar(InstanceValue::Int32(value)) = result else {
                panic!("scalar result")
            };
            match name {
                "joined" => assert_eq!(value, 83),
                "raced" => assert!([41, 42].contains(&value)),
                _ => assert_eq!(value, 42),
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
