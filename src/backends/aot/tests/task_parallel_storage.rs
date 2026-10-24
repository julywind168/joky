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
