use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
use std::time::{Duration, Instant};
const SOURCE: &str = include_str!("../../tests/fixtures/tcp.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn clean() {
    crate::runtime::tcp::LIVE.with(|count| assert_eq!(count.get(), 0));
    crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
}
#[test]
fn net2_tcp_roundtrips_errors_and_reuse_in_both_vm_modes() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        for entry in ["ipv4", "ipv6", "errors", "reuse"] {
            assert_eq!(
                block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(),
                42,
                "{mode:?}: {entry}"
            );
            clean();
        }
    }
}
#[test]
fn net2_tcp_pending_cancel_reload_and_vm_drop_release_resources() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("pending", &[]).unwrap())
            .unwrap();
        assert_eq!(executor.poll(1).unwrap().polled, 1);
        crate::runtime::tcp::LIVE.with(|count| assert_eq!(count.get(), 1));
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        vm.load_source("fn unrelated() -> Int32 { 7 }").unwrap();
        drop(vm);
        executor.cancel_group(&group);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clean();
    }
}
fn cancel_cuts(exhaustive: bool) {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for entry in ["ipv4", "errors", "reuse"] {
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
            .unwrap();
        let mut steps = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while executor.task_count() != 0 {
            let report = executor.poll(1).unwrap();
            steps += report.polled;
            for done in report.completed {
                assert_eq!(done.result, Ok(42));
            }
            assert!(
                steps < 5000 && Instant::now() < deadline,
                "{entry}: {steps} steps"
            );
        }
        clean();
        for cut in (0..=steps).filter(|cut| exhaustive || cut % 32 == 0 || *cut == steps) {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
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
}
#[test]
fn net2_tcp_cancel_at_instruction_boundaries() {
    cancel_cuts(false);
}
#[test]
#[ignore = "exhaustive: cancel TCP ownership handoff and output places at every instruction"]
fn exhaustive_net2_tcp_cancel_every_instruction() {
    cancel_cuts(true);
}
#[test]
fn net2_tcp_rejects_forged_signatures_owners_and_aliases() {
    let invalid = [
        "extern fn __tcp_read(stream: UInt64) @suspends -> UInt64 fn main() @suspends -> UInt64 { __tcp_read(1) }",
        "fn __tcp_close_stream(input: UInt64) @suspends -> UInt64 {input}",
        "import joky/tcp.{type Address, type TcpListener} fn main() @suspends { let a=Address.ipv4(0x7f000001,0) let listener=TcpListener.bind(a) let copied=listener let twice=listener }",
        "import joky/tcp.{type TcpListener} fn wait(listener: &mut TcpListener) @suspends {listener.accept();} fn both(listener: &mut TcpListener) @suspends { parallel { | wait(listener) | wait(listener) } }",
        "import joky/tcp.{type TcpStream} fn fabricate() -> TcpStream {TcpStream { owner: 1 }}",
        "import joky/tcp.{status} fn main() {status(0);}",
        "import joky/tcp.{stream_addr} fn main() {}",
    ];
    unsafe extern "C" fn empty(_: u64, output: *mut *mut std::ffi::c_void) -> i32 {
        *output = std::ptr::null_mut();
        0
    }
    unsafe extern "C" fn release(_: *mut std::ffi::c_void) {}
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for source in invalid {
            let mut vm = VmEngine::with_mode(mode);
            assert!(vm.load_source(source).is_err(), "{source}");
        }
        let mut vm = VmEngine::with_mode(mode);
        assert!(unsafe {
            vm.register_resource_factory(
                "forge",
                "tcp.StreamOwner",
                crate::runtime::resource::ResourceFactoryV1 {
                    abi_version: 1,
                    create: Some(empty),
                    release: Some(release),
                },
                None,
            )
        }
        .is_err());
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn net2_tcp_connection_refused_is_a_business_result() {
    mod endpoint {
        include!("../../tests/support/tcp.rs");
    }
    let (_reserved, port) = endpoint::refused_endpoint();
    let source =
        include_str!("../../tests/fixtures/tcp_refused.jk").replace("PORT", &port.to_string());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        assert_eq!(block_on(vm.prepare_async("main", &[]).unwrap()).unwrap(), 0);
        clean();
    }
}
