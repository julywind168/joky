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

#[path = "support/scratch.rs"]
mod scratch;

#[test]
fn injected_test_inputs_are_shared_by_service_clones_and_isolated_between_engines() {
    let directory = scratch::ScratchDir::new("test-inputs");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = r#"
            import joky/testing
            import joky/process
            import joky/io
            fn configure(value: Int32) @suspends {
                testing.set_args([if value == 1 { "first" } else { "second" }])!
                testing.set_env("VALUE", if value == 1 { "first" } else { "second" })!
                testing.set_stdin(if value == 1 { "first" } else { "second" })!
            }
            fn read() @suspends -> Int32 {
                var args = process.args()!
                let environment = process.env("VALUE")!!
                assert(args.take_last() == environment, "arguments use the injected context")
                var input = io.stdin()!
                let text = input.read_text_limited(10)!
                input.close()!
                assert(text == environment, "stdin and environment belong to the same context")
                if text == "first" { 1 } else { 2 }
            }
        "#;
        let mut engines = Vec::new();
        for value in [1, 2] {
            let service = FileService::testing(directory.path(), Default::default()).unwrap();
            let mut setup = VmEngine::with_mode(mode);
            setup.set_file_service(service.clone()).unwrap();
            setup.load_source(source).unwrap();
            if value == 1 {
                let prepared = setup
                    .prepare_parallel_task(
                        "configure",
                        vec![joky::vm::TaskValue::Scalar(joky::vm::InstanceValue::Int32(
                            value,
                        ))],
                        vec![],
                    )
                    .unwrap();
                let executor = joky::runtime::parallel::ParallelExecutor::new(1, 4096).unwrap();
                let task = prepared.submit(&executor).unwrap();
                task.wait().unwrap();
                task.take_result().unwrap();
            } else {
                block_on(
                    setup
                        .prepare_async("configure", &[joky::vm::InstanceValue::Int32(value)])
                        .unwrap(),
                )
                .unwrap();
            }
            drop(setup);
            let mut reader = VmEngine::with_mode(mode);
            reader.set_file_service(service).unwrap();
            reader.load_source(source).unwrap();
            engines.push(reader);
        }
        for (index, vm) in engines.iter_mut().enumerate().rev() {
            assert_eq!(
                block_on(vm.prepare_async("read", &[]).unwrap()).unwrap(),
                index as u64 + 1
            );
        }
    }
}
