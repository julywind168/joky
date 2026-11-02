//! Generated notification operations and structured cancellation share one runtime.
use super::{support::*, VmMode};
use crate::runtime::{
    arena::{self, LIVE},
    driver::block_on,
    executor::{HostExecutor, TaskGroup},
};
const SOURCE: &str = include_str!("../../../../tests/fixtures/shutdown.jk");

#[test]
fn shutdown_notifications_broadcast_latch_and_remove_cancelled_waiters() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for entry in [
            "shutdown_broadcast",
            "shutdown_cancel_waiter",
            "shutdown_shared_owner",
        ] {
            assert_eq!(block_on(vm.prepare_async(entry, &[]).unwrap()).unwrap(), 42);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("shutdown_pending", &[]).unwrap())
            .unwrap();
        executor.poll(10).unwrap();
        assert!(executor.cancel_group(&group));
        assert_eq!(executor.task_count(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn shutdown_allocation_failure_refunds_before_retry() {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let mut completed = false;
        for limit in (0..65536).step_by(512) {
            arena::LIMIT.with(|n| n.set(limit));
            match vm.prepare_async("shutdown_once", &[]).and_then(block_on) {
                Ok(value) => {
                    assert_eq!(value, 42);
                    completed = true;
                }
                Err(error) => assert_eq!(error.code(), crate::ErrorCode::ResourceLimit, "{error}"),
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if completed {
                break;
            }
        }
        assert!(completed);
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
#[test]
fn shutdown_signatures_and_local_ownership_are_checked() {
    for source in [
        "import joky/shutdown fn f() { shutdown.create(); }",
        "extern fn __shutdown_wait() @suspends -> UInt64",
        "fn __shutdown_create() {}",
        "import joky/shutdown import joky/mem fn f() @suspends { let s = shutdown.create(); mem.drop(s); s.wait(); }",
        "import joky/shutdown fn f() @suspends { let s = shutdown.create(); s.storage; }",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}

#[test]
fn shutdown_deadline_starts_after_notification_on_the_executor_clock() {
    use crate::runtime::executor::Clock;
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };
    struct ManualClock(Cell<Instant>);
    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
    }
    fn settle(executor: &HostExecutor) -> crate::runtime::executor::PollReport {
        // poll consumes only its initial ready batch; a sibling notification
        // schedules another turn without advancing the virtual clock.
        for _ in 0..100 {
            let report = executor.poll(10).unwrap();
            if !report.ready || !report.completed.is_empty() {
                return report;
            }
        }
        panic!("shutdown task did not settle");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let start = Instant::now();
        let clock = Rc::new(ManualClock(Cell::new(start)));
        let executor = HostExecutor::new(clock.clone(), 4);
        let group = TaskGroup::new();
        executor
            .spawn(
                &group,
                vm.prepare_async("shutdown_deadline_start", &[]).unwrap(),
            )
            .unwrap();
        let report = settle(&executor);
        assert_eq!(
            report.next_deadline,
            Some(start + Duration::from_millis(100))
        );
        clock.0.set(start + Duration::from_millis(100));
        let report = settle(&executor);
        assert!(report.completed.is_empty());
        assert_eq!(
            report.next_deadline,
            Some(start + Duration::from_millis(110))
        );
        clock.0.set(start + Duration::from_millis(110));
        let report = settle(&executor);
        assert_eq!(report.completed.len(), 1);
        assert_eq!(report.completed[0].result, Ok(42));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn shutdown_parallel_owned_controllers_broadcast_and_cancel_waiters() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        for entry in [
            "shutdown_broadcast",
            "shutdown_cancel_waiter",
            "shutdown_parallel_transfer",
            "shutdown_branch_wakes_parent",
        ] {
            let prepared = vm
                .prepare_parallel_task(entry, vec![], vec![])
                .unwrap_or_else(|e| panic!("{mode:?} {entry}: {e}"));
            let mut pool = crate::runtime::parallel::ParallelExecutor::new(3, 8).unwrap();
            let task = prepared.submit(&pool).unwrap();
            task.wait().unwrap();
            assert_eq!(
                task.take_result().unwrap().value(),
                &crate::vm::TaskValue::Scalar(crate::vm::InstanceValue::Int32(42))
            );
            drop(task);
            pool.wait_closed();
        }
    }
}
