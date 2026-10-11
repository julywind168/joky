use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
const SOURCE: &str = include_str!("../../tests/fixtures/tcp_split.jk");
fn clean() {
    crate::runtime::tcp::LIVE.with(|n| assert_eq!(n.get(), 0));
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e} {:?}", e.span().and_then(|s| s.slice(SOURCE))));
    vm
}
#[test]
fn net5b_split_duplex_shutdown_drop_and_cancel_in_both_vm_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for entry in [
            "duplex",
            "eof_reply",
            "writer_drop",
            "reader_shutdown",
            "state_owners",
            "main",
        ] {
            assert_eq!(
                block_on(vm(mode).prepare_async(entry, &[]).unwrap())
                    .unwrap_or_else(|e| panic!("{mode:?} {entry}: {e}")),
                0
            );
            clean();
        }
    }
}

fn cancellation(exhaustive: bool) {
    use std::time::{Duration, Instant};
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let executor = HostExecutor::default();
    let group = TaskGroup::new();
    executor
        .spawn(&group, vm.prepare_async("handoff", &[]).unwrap())
        .unwrap();
    let mut steps = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while executor.task_count() != 0 {
        let report = executor.poll(1).unwrap();
        steps += report.polled;
        for done in report.completed {
            assert_eq!(done.result.unwrap(), 42);
        }
        assert!(steps < 3000 && Instant::now() < deadline);
    }
    clean();
    for cut in (0..=steps).filter(|n| exhaustive || n % 8 == 0 || *n == steps) {
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("handoff", &[]).unwrap())
            .unwrap();
        let mut polls = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls < cut && executor.task_count() != 0 {
            polls += executor.poll(1).unwrap().polled;
            assert!(Instant::now() < deadline);
        }
        executor.cancel_group(&group);
        clean();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
    }
}
#[test]
fn net5b_split_publication_cancel_at_resource_boundaries() {
    cancellation(false);
}
#[test]
#[ignore = "exhaustive: cancel split publication and direction owners at every instruction"]
fn exhaustive_net5b_split_publication_cancel_every_instruction() {
    cancellation(true);
}

#[test]
fn net5b_split_pending_reload_and_vm_drop_keep_or_release_owners() {
    use std::time::{Duration, Instant};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let report = executor.poll(1).unwrap();
            assert!(report.completed.is_empty());
            // The sleep starts only after connect, accept and split finish.
            // Socket readiness may require more than one executor poll.
            if report.next_deadline.is_some() && report.polled == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "{mode:?}: split did not settle");
            std::thread::yield_now();
        }
        assert_eq!(executor.task_count(), 1);
        vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
        drop(vm);
        executor.cancel_group(&group);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clean();
    }
}
#[test]
fn net5b_split_rejects_forged_and_conflicting_directions_in_all_backends() {
    let invalid = [
        "extern fn __tcp_split_reader(stream: UInt64) @suspends -> UInt64",
        "extern fn __tcp_write_half(writer: &mut tcp.ReaderOwner, bytes: UInt64, error: &mut UInt64) @suspends -> UInt64",
        "import joky/tcp.{type TcpReader} fn fake() -> TcpReader { TcpReader { owner: 1 } }",
        "import joky/tcp.{type TcpReader} fn copy(reader: TcpReader) { let left = reader let right = reader }",
        "import joky/tcp.{type TcpReader} fn read(reader: &mut TcpReader) @suspends { reader.read(1); } fn bad(reader: &mut TcpReader) @suspends { parallel { | read(reader) | read(reader) } }",
        "import joky/tcp.{type TcpWriter} import joky/array.{type Array} fn write(writer: &mut TcpWriter) @suspends { writer.write(Array.new::<UInt8>()); } fn bad(writer: &mut TcpWriter) @suspends { parallel { | write(writer) | write(writer) } }",
        "import joky/tcp.{type TcpStream} fn bad(stream: TcpStream) @suspends { stream.split(); stream.close(); }",
    ];
    for source in invalid {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(
                VmEngine::with_mode(mode).load_source(source).is_err(),
                "{source}"
            );
        }
        let ast = crate::parser::parse(source).unwrap();
        assert!(
            crate::aot::AotCompiler::new("invalid-tcp-split")
                .unwrap()
                .compile_module(&ast)
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn net5b_split_half_resources_reject_external_factories() {
    unsafe extern "C" fn create(_: u64, output: *mut *mut std::ffi::c_void) -> i32 {
        *output = std::ptr::null_mut();
        0
    }
    unsafe extern "C" fn release(_: *mut std::ffi::c_void) {}
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for resource in ["tcp.ReaderOwner", "tcp.WriterOwner"] {
            let mut vm = VmEngine::with_mode(mode);
            assert!(unsafe {
                vm.register_resource_factory(
                    "forge-half",
                    resource,
                    crate::runtime::resource::ResourceFactoryV1 {
                        abi_version: 1,
                        create: Some(create),
                        release: Some(release),
                    },
                    None,
                )
            }
            .is_err());
        }
    }
}
