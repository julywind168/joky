//! Custom borrowing-host I/O lifecycle and opaque-type ABI regressions.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};

mod host {
    use crate as joky;
    include!("../../../../tests/fixtures/borrowed_host_io_host.rs");
}
pub(crate) const SOURCE: &str = include_str!("../../../../tests/fixtures/borrowed_host_io.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(false);
    // SAFETY: Static callbacks obey the declared scalar and exclusive C-record
    // ABIs. stop unregisters readiness and closes the socket before request cleanup.
    unsafe {
        vm.register_borrowing_suspending_fn(
            "borrowed_host_io_read",
            FunctionSignature::new(
                vec![Type::reference(Type::Named("NetRequest".into()), true)],
                Type::Int32,
            ),
            host::READ_HOST,
            None,
        )
        .unwrap();
        vm.register_fn(
            "borrowed_host_io_event",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            host::borrowed_host_io_event as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(SOURCE, &crate::ModuleSources::new())
        .unwrap_or_else(|e| {
            panic!(
                "{mode:?}: {e} {:?}",
                e.span().and_then(|s| e
                    .sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text()))))
            )
        });
    vm
}
fn no_allocations() {
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn borrowed_host_io_loopback_bytes_results_errors_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        host::check_suite(|entry| vm.prepare_async(entry, &[]).unwrap());
        no_allocations();
    }
}
#[test]
fn borrowed_host_io_instance_close_stops_io_before_task_and_state_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        host::setup();
        let vm = vm(mode);
        let id = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "instance_read", &[]).unwrap();
        assert_eq!(vm.poll_tasks(64).unwrap().polled, 1);
        let stale = host::token();
        for _ in 0..4 {
            assert_eq!(vm.poll_tasks(64).unwrap().polled, 0);
        }
        host::send(&[0, 128]);
        host::await_partial(|| {
            vm.poll_tasks(64).unwrap();
        });
        vm.close_instance(id).unwrap();
        host::assert_cleanup(4, false, None);
        let events = host::events();
        assert_eq!(events.last(), Some(&30), "{events:?}");
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_task_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        stale.wake();
        host::deliver_readiness();
        assert_eq!(vm.poll_tasks(64).unwrap().polled, 0);
        host::teardown();
        no_allocations();
    }
}
#[test]
fn borrowed_host_io_pending_io_survives_reload_and_vm_drop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            host::setup();
            let mut vm = vm(mode);
            let executor = crate::runtime::executor::HostExecutor::default();
            let group = crate::runtime::executor::TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("data", &[]).unwrap())
                .unwrap();
            executor.poll(1).unwrap();
            host::send(&[0, 128]);
            host::await_partial(|| {
                executor.poll(1).unwrap();
            });
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if finish {
                host::send(&[255, 42]);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    host::deliver_readiness();
                    let report = executor.poll(1).unwrap();
                    if let Some(done) = report.completed.into_iter().next() {
                        assert_eq!(done.result, Ok(42));
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                host::assert_cleanup(2, true, Some(10));
            } else {
                executor.cancel_group(&group);
                host::assert_cleanup(4, false, None);
            }
            host::teardown();
            no_allocations();
        }
    }
}
#[test]
fn opaque_types_reject_ownership_and_buffer_host_abis() {
    for (source, message) in [
        ("import joky/drop.{type Drop} struct SocketOwner { token: UInt64 } impl Drop for SocketOwner { fn drop(&mut self) {} }", "Drop requires an owning record"),
        ("import joky/result.{type Result, Ok} fn wrap(socket: Socket) -> Result<Socket, Int32> { Ok(socket) }", "Generic enum argument must be a Copy type or an owner"),
        ("struct Holder { socket: Socket }", "Record fields and enum payloads require a Copy value or an owner"),
        ("import joky/array.{type Array} extern fn read(bytes: &mut Array<UInt8>) @suspends -> Int32", "Buffer owners and views are not supported by the host ABI"),
        ("import joky/slice.{type Slice} extern fn read(bytes: &mut Slice<UInt8>) @suspends -> Int32", "Buffer owners and views are not supported by the host ABI"),
    ] {
        for mode in [VmMode::Jit,VmMode::Interpreter] {
            let mut vm=VmEngine::with_mode(mode);
            vm.register_opaque_type("Socket").unwrap();
            let error=vm.load_source(source).expect_err(source);
            assert!(error.message().contains(message),"{mode:?}: {source}: {error}");
            assert!(error.span().is_some(),"{mode:?}: {source}: {error}");
        }
        let mut aot=crate::aot::AotCompiler::new("borrowed_host_io-gaps").unwrap();
        aot.register_opaque_type("Socket").unwrap();
        let error=aot.compile_module(&crate::parser::parse(source).unwrap()).expect_err(source);
        assert!(error.span().is_some() && error.message().contains(message),"AOT: {source}: {error}");
    }
}
