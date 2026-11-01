//! Test output authority follows engines, frozen tasks and service clones.
use joky::{
    runtime::{driver::block_on, fs::FileService, parallel::ParallelExecutor},
    vm::{VmEngine, VmMode},
};

#[path = "support/scratch.rs"]
mod scratch;

#[test]
fn test_output_captures_sync_roots_and_keeps_engine_snapshots_isolated() {
    let directory = scratch::ScratchDir::new("test-output");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = r#"
import joky/testing
fn unit() { print("unit ") }
fn scalar() -> Int32 { eprint("error "); 42 }
fn text() -> String { println("中\0🙂"); "result" }
fn read() @suspends -> String { testing.stdout()! }
fn errors() @suspends -> String { testing.stderr()! }
"#;
        let service = FileService::testing(directory.path(), Default::default()).unwrap();
        let mut writer = VmEngine::with_mode(mode);
        writer.set_file_service(service.clone()).unwrap();
        writer.load_source(source).unwrap();
        writer.call_unit_0("unit").unwrap();
        assert_eq!(writer.call_int32_0("scalar").unwrap(), 42);
        assert_eq!(writer.call_string_0("text").unwrap(), "result");
        drop(writer);
        let mut engines = Vec::new();
        for service in [
            service,
            FileService::testing(directory.path(), Default::default()).unwrap(),
        ] {
            let mut reader = VmEngine::with_mode(mode);
            reader.set_file_service(service).unwrap();
            reader.load_source(source).unwrap();
            engines.push(reader);
        }
        for (index, reader) in engines.iter_mut().enumerate().rev() {
            let mut output = reader.call_async_string_0("read").unwrap();
            while !output.is_finished() {
                output.resume().unwrap();
            }
            assert_eq!(
                output.take_result().unwrap(),
                if index == 0 { "unit 中\0🙂\n" } else { "" }
            );
            let mut errors = reader.call_async_string_0("errors").unwrap();
            while !errors.is_finished() {
                errors.resume().unwrap();
            }
            assert_eq!(
                errors.take_result().unwrap(),
                if index == 0 { "error " } else { "" }
            );
        }
    }
}

#[test]
fn test_output_follows_parallel_workers_and_retained_code() {
    let directory = scratch::ScratchDir::new("parallel-test-output");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let service = FileService::testing(directory.path(), Default::default()).unwrap();
        let mut vm = VmEngine::with_mode(mode);
        vm.set_file_service(service.clone()).unwrap();
        vm.load_source(r#"
import joky/testing
import joky/task
fn emit() @suspends { var n = 0; while n < 8 { print("中\0"); eprint("🙂"); task.yield_now(); n += 1 } }
fn write() @suspends {
    scope {
        branch { emit() }
        branch { emit() }
    }
    assert_eq(testing.stdout()!.byte_length(), 64)
    assert_eq(testing.stderr()!.byte_length(), 64)
}
"#).unwrap();
        let task = vm.prepare_parallel_task("write", vec![], vec![]).unwrap();
        drop(vm);
        let executor = ParallelExecutor::new(2, 4096).unwrap();
        let task = task.submit(&executor).unwrap();
        task.wait().unwrap();
        task.take_result().unwrap();
        let mut reader = VmEngine::with_mode(mode);
        reader.set_file_service(service).unwrap();
        reader
            .load_source(
                r#"
import joky/testing
fn check() @suspends {
    let text = testing.stdout()!
    assert_eq(text.byte_length(), 64)
    assert_eq(testing.stdout()!, text)
    assert_eq(testing.stderr()!.byte_length(), 64)
}
"#,
            )
            .unwrap();
        block_on(reader.prepare_async("check", &[]).unwrap()).unwrap();
    }
}
