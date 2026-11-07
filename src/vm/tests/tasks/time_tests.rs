//! Calendar values, clock domains and absolute waits execute through generated code.
use super::{support::*, VmMode};
use crate::runtime::{
    arena::LIVE,
    executor::{Clock, HostExecutor, TaskGroup},
};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};
const SOURCE: &str = include_str!("../../../../tests/fixtures/time_points.jk");
struct ManualClock(Cell<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}

#[test]
fn time_points_values_and_calendar_boundaries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let wall_micros = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64
        };
        let before = wall_micros();
        let reading = vm.call_int64_0("time_unix_micros").unwrap();
        let after = wall_micros();
        assert!((before..=after).contains(&reading));
        for name in [
            "timestamp_values",
            "timestamp_limits",
            "timestamp_formats",
            "timestamp_rejects",
            "time_wall",
            "time_instant",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42
            );
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
#[test]
fn time_points_absolute_wait_uses_virtual_clock_and_cleans_cancelled_values() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let clock = Rc::new(ManualClock(Cell::new(Instant::now())));
        let executor = HostExecutor::new(clock.clone(), 4);
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("time_wait", &[]).unwrap())
            .unwrap();
        let report = executor.poll(10).unwrap();
        assert_eq!(
            report.next_deadline,
            Some(clock.now() + Duration::from_micros(250))
        );
        for _ in 0..3 {
            assert_eq!(executor.poll(10).unwrap().polled, 0);
        }
        clock.0.set(clock.now() + Duration::from_nanos(249999));
        assert_eq!(executor.poll(10).unwrap().polled, 0);
        clock.0.set(clock.now() + Duration::from_nanos(1));
        let report = executor.poll(10).unwrap();
        assert_eq!(report.completed.len(), 1);
        assert_eq!(report.completed[0].result, Ok(42));
        executor
            .spawn(&group, vm.prepare_async("time_cancel", &[]).unwrap())
            .unwrap();
        executor.poll(10).unwrap();
        assert!(executor.cancel_group(&group));
        assert_eq!(executor.task_count(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn time_points_private_coordinates_and_distinct_types_are_checked() {
    for source in [
        "import joky/time fn f() @suspends { time.sleep_until(time.Instant.now()); }",
        "import joky/time fn f() @suspends { time.__sleep_until(time.Instant.now()); }",
        "import joky/time.{__sleep_until} fn f() {}",
        "import joky/internal/time fn f() {}",
        "import joky/internal/time as clock.{type Instant} fn f() {}",
        "import joky/time fn f() { time.scaled(1, 1000); }",
        "import joky/time fn f() { time.valid(time.Timestamp.now()); }",
        "import joky/time fn f(){ time.Instant { clock_id: 0, ticks: 0 }; }",
        "import joky/time fn f(){ time.Timestamp { seconds: 0, nanos: 0 }; }",
        "import joky/time import joky/task fn f() @suspends { task.sleep_until(time.Timestamp.now()); }",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
}

#[test]
fn time_points_reject_cross_clock_wait_after_adoption() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let task = vm.call_async("time_foreign", &[]).unwrap();
        assert!(!task.is_finished());
        let executor = HostExecutor::new(Rc::new(ManualClock(Cell::new(Instant::now()))), 4);
        let group = TaskGroup::new();
        executor.spawn(&group, task).unwrap();
        let report = executor.poll(10).unwrap();
        assert_eq!(report.completed.len(), 1);
        let error = report.completed[0].result.as_ref().unwrap_err();
        assert!(error.to_string().contains("different clock"), "{error}");
        assert_eq!(executor.task_count(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn time_points_execute_on_parallel_workers() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, TaskValue},
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        let prepared = vm
            .prepare_parallel_task("time_parallel", vec![], vec![])
            .unwrap();
        let mut pool = ParallelExecutor::new(2, 8).unwrap();
        let task = prepared.submit(&pool).unwrap();
        let limit = Instant::now() + Duration::from_secs(10);
        while !task.is_finished() {
            assert!(Instant::now() < limit);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            task.take_result().unwrap().value(),
            &TaskValue::Scalar(InstanceValue::Int32(42))
        );
        drop(task);
        pool.wait_closed();
    }
}
