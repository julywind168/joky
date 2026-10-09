use super::{VmEngine, VmMode};
use crate::runtime::{
    byte_request::ByteIoKind,
    executor::{HostExecutor, TaskGroup},
};
mod host {
    use crate as joky;
    include!("../../tests/fixtures/net1b_host.rs");
}
const SOURCE: &str = include_str!("../../tests/fixtures/net1b.jk");
fn vm(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.register_resource_type("Socket").unwrap();
    unsafe {
        vm.register_resource_factory("net1b_make", "Socket", host::FACTORY, None)
            .unwrap();
        vm.register_resource_io("net1b_read", "Socket", ByteIoKind::Read, host::READ, None)
            .unwrap();
    }
    vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
    vm
}
fn clean() {
    host::check();
    crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn net1b_reactor_drives_io_reuse_spurious_events_timers_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode);
        host::suite(|entry| vm.prepare_async(entry, &[]).unwrap());
        clean();
    }
}
#[test]
fn net1b_reactor_instance_close_and_code_reload_revoke_readiness() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        host::setup();
        let mut vm = vm(mode);
        let id = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "instance_read", &[]).unwrap();
        assert_eq!(vm.poll_tasks(1).unwrap().polled, 1);
        for _ in 0..16 {
            assert_eq!(vm.poll_tasks(1).unwrap().polled, 0);
        }
        vm.close_instance(id).unwrap();
        host::wake();
        assert_eq!(vm.poll_tasks(1).unwrap().polled, 0);
        assert_eq!(vm.instance_bytes(), 0);
        clean();
        for finish in [false, true] {
            host::setup();
            vm.load_source(SOURCE).unwrap();
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("receive", &[]).unwrap())
                .unwrap();
            executor.poll(1).unwrap();
            vm.load_source("fn unrelated() -> Int32 {7}").unwrap();
            if finish {
                host::publish(&[0, 128, 255, 42]);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    let report = executor.poll(1).unwrap();
                    if let Some(done) = report.completed.into_iter().next() {
                        assert_eq!(done.result, Ok(42));
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
            } else {
                executor.cancel_group(&group);
            }
            clean();
        }
    }
}
fn cancellation(exhaustive: bool) {
    let mut vm = vm(VmMode::Interpreter);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    for entry in ["receive", "reuse", "dynamic"] {
        host::setup();
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
            .unwrap();
        let mut steps = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while executor.task_count() != 0 {
            let report = executor.poll(1).unwrap();
            steps += report.polled;
            host::publish(&[0, 128, 255, 42]);
            assert!(steps < 1000 && std::time::Instant::now() < deadline);
        }
        clean();
        for cut in (0..=steps).filter(|n| exhaustive || n % 8 == 0 || *n == steps) {
            host::setup();
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let mut polled = 0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while polled < cut {
                polled += executor.poll(1).unwrap().polled;
                host::publish(&[0, 128, 255, 42]);
                assert!(std::time::Instant::now() < deadline);
            }
            executor.cancel_group(&group);
            clean();
            host::wake();
            assert_eq!(executor.poll(1).unwrap().polled, 0);
        }
    }
}
#[test]
fn net1b_reactor_cancel_at_instruction_boundaries() {
    cancellation(false);
}
#[test]
#[ignore = "exhaustive: readiness registration cancellation at every interpreter instruction"]
fn exhaustive_net1b_reactor_cancel_every_instruction() {
    cancellation(true);
}
