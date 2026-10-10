use super::{InterpreterLimits, VmEngine, VmMode};
use crate::runtime::{
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
mod endpoint {
    include!("../../tests/support/json_tcp.rs");
}
thread_local! { static SENT: Cell<usize> = const { Cell::new(0) }; }
extern "C" fn sent() {
    SENT.with(|count| count.set(count.get() + 1));
}
fn clean() {
    crate::runtime::tcp::LIVE.with(|count| assert_eq!(count.get(), 0));
    crate::runtime::arena::LIVE.with(|count| assert_eq!(count.get(), 0));
}
fn vm(mode: VmMode, source: &str) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    // SAFETY: The callback has the declared zero-input Unit scalar ABI.
    unsafe {
        vm.register_fn(
            "sent",
            crate::FunctionSignature::new(vec![], crate::Type::Unit),
            sent as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source)
        .unwrap_or_else(|error| panic!("{error} at {:?}", error.span()));
    vm
}
#[test]
fn json3_native_frames_and_business_recovery_release_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for case in 0..3 {
            let peer = endpoint::peer(case);
            let source = format!(
                "{}\nfn probe() @suspends -> Int32 {{ native({}, {}) }}",
                endpoint::source(),
                peer.port,
                if case == 0 { 2 } else { -2 }
            );
            let vm = vm(mode, &source);
            assert_eq!(
                block_on(vm.prepare_async("probe", &[]).unwrap())
                    .unwrap_or_else(|error| panic!("{mode:?}: {error}")),
                42
            );
            assert_eq!(peer.finish(), 0);
            clean();
        }
    }
}
#[test]
fn json3_borrowed_encoded_tree_survives_backpressure_reload_drop_and_cancel() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for cancel in [false, true] {
            let mut peer = endpoint::peer(3);
            let source = format!(
                "{}\nfn probe() @suspends -> Int32 {{ pressure({}) }}",
                endpoint::source(),
                peer.port
            );
            let mut vm = vm(mode, &source);
            SENT.with(|count| count.set(0));
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async("probe", &[]).unwrap())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut idle = 0;
            while idle < 16 {
                let report = executor.poll(1).unwrap();
                idle = if report.polled == 0 && SENT.with(|count| count.get() > 0) {
                    idle + 1
                } else {
                    0
                };
                assert!(Instant::now() < deadline, "{mode:?}: cancel={cancel}");
                assert_eq!(executor.task_count(), 1);
            }
            let before = SENT.with(Cell::get);
            assert!(
                before > 0 && before < 512,
                "writes must actually hit backpressure: {before}"
            );
            vm.load_source("fn replacement() -> Int32 { 7 }").unwrap();
            drop(vm);
            if cancel {
                executor.cancel_group(&group);
                assert_eq!(executor.poll(1).unwrap().polled, 0);
                assert_eq!(SENT.with(Cell::get), before);
                assert!(peer.finish() < 512);
            } else {
                peer.release();
                while executor.task_count() != 0 {
                    let report = executor.poll(32).unwrap();
                    for completed in report.completed {
                        assert_eq!(completed.result.unwrap(), 42);
                    }
                    assert!(Instant::now() < deadline, "{mode:?}: cancel={cancel}");
                }
                assert_eq!(SENT.with(Cell::get), 512);
                assert_eq!(peer.finish(), 512);
            }
            clean();
        }
    }
}
#[test]
fn json3_interpreter_cancellation_samples_codec_and_socket_ownership() {
    let source =
        include_str!("../../examples/tcp_json.jk").replace("println(kind)", "task.yield_now()");
    let mut vm = vm(VmMode::Interpreter, &source);
    // Increasing slices cover partial tree construction, encoding and socket handoffs.
    for budget in [1, 16, 128, 512, 2048, 8192, 32768] {
        vm.set_interpreter_limits(InterpreterLimits {
            poll_budget: Some(budget),
            ..Default::default()
        })
        .unwrap();
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("main", &[]).unwrap())
            .unwrap();
        for _ in 0..4 {
            for completed in executor.poll(1).unwrap().completed {
                assert_eq!(completed.result.unwrap(), 0);
            }
        }
        executor.cancel_group(&group);
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clean();
    }
}
