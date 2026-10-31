//! Embedders receive no ambient startup inputs or stdin capability.
use joky::{
    runtime::{driver::block_on, fs::FileService, process::ProcessContext},
    vm::{VmEngine, VmMode},
};
use std::{collections::BTreeMap, ffi::OsString};

#[test]
fn process_snapshots_are_explicit_and_isolated_between_engines() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = r#"
            import joky/process
            import joky/io
            fn read() @suspends -> Int32 {
                assert(io.stdin().is_err(), "ordinary file capability does not grant stdin")
                match process.env("VALUE") {
                    Ok(Some(value)) => if value == "first" { 1 } else { 2 }
                    _ => 0
                }
            }
        "#;
        let mut engines = Vec::new();
        for value in [None, Some("first"), Some("second")] {
            let mut vm = VmEngine::with_mode(mode);
            let mut service = FileService::native(".", Default::default()).unwrap();
            if let Some(value) = value {
                service = service.with_process_context(
                    ProcessContext::new(
                        vec![],
                        BTreeMap::from([(OsString::from("VALUE"), OsString::from(value))]),
                        std::env::current_dir().unwrap(),
                    )
                    .unwrap(),
                );
            }
            vm.set_file_service(service).unwrap();
            vm.load_source(source).unwrap();
            engines.push(vm);
        }
        for (expected, vm) in engines.iter_mut().enumerate().rev() {
            assert_eq!(
                block_on(vm.prepare_async("read", &[]).unwrap()).unwrap(),
                expected as u64
            );
        }
    }
}
