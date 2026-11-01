//! Typed time values, exact timer deadlines and structured timeout cleanup.
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
const SOURCE: &str = include_str!("../../../../tests/fixtures/durations.jk");
struct ManualClock(Cell<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
fn cleaned() {
    EVENTS.with(|e| {
        let e = e.borrow();
        assert!(e.iter().filter(|&&v| v == 1).count() <= 1, "{e:?}");
        if e.contains(&-1) {
            assert_eq!(e.iter().filter(|&&v| v == 1).count(), 1, "{e:?}");
        }
    });
    LIVE.with(|n| assert_eq!(n.get(), 0));
}
#[test]
fn duration_values_and_timeout_ownership() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        assert_eq!(vm.call_int32_0("duration_values").unwrap(), 42);
        for entry in [
            "duration_success",
            "duration_owned_result",
            "duration_place_result",
        ] {
            for cancel in [false, true] {
                READY.with(|r| r.set(false));
                EVENTS.with(|e| e.borrow_mut().clear());
                let mut task = vm.call_async(entry, &[]).unwrap();
                let trace = EVENTS.with(|e| e.borrow().clone());
                for _ in 0..3 {
                    assert!(!task.is_finished());
                    task.resume().unwrap();
                    EVENTS.with(|e| assert_eq!(*e.borrow(), trace));
                }
                if !cancel {
                    READY.with(|r| r.set(true));
                    for _ in 0..20 {
                        if task.is_finished() {
                            break;
                        }
                        task.resume().unwrap();
                    }
                    assert_eq!(task.cps_result(), Some(42), "{mode:?} {entry}");
                }
                drop(task);
                cleaned();
            }
        }
        READY.with(|r| r.set(false));
        EVENTS.with(|e| e.borrow_mut().clear());
        let task = vm.call_async("duration_elapsed", &[]).unwrap();
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        cleaned();
        EVENTS.with(|e| e.borrow_mut().clear());
        let mut task = vm.call_async("duration_failure", &[]).unwrap();
        READY.with(|r| r.set(true));
        assert!(task.resume().is_err());
        drop(task);
        cleaned();
    }
}
#[test]
fn duration_timers_use_exact_executor_deadlines() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for entry in ["duration_sleep", "duration_pending"] {
            READY.with(|r| r.set(false));
            EVENTS.with(|e| e.borrow_mut().clear());
            let clock = Rc::new(ManualClock(Cell::new(Instant::now())));
            let executor = HostExecutor::new(clock.clone(), 4);
            let group = TaskGroup::new();
            executor
                .spawn(&group, vm.prepare_async(entry, &[]).unwrap())
                .unwrap();
            let report = executor.poll(10).unwrap();
            assert_eq!(
                report.next_deadline,
                Some(clock.now() + Duration::from_micros(250))
            );
            let trace = EVENTS.with(|e| e.borrow().clone());
            clock.0.set(clock.now() + Duration::from_nanos(249999));
            assert_eq!(executor.poll(10).unwrap().polled, 0);
            EVENTS.with(|e| assert_eq!(*e.borrow(), trace));
            clock.0.set(clock.now() + Duration::from_nanos(1));
            let report = executor.poll(10).unwrap();
            assert_eq!(report.completed.len(), 1);
            assert_eq!(report.completed[0].result, Ok(42));
            assert_eq!(executor.task_count(), 0);
            cleaned();
        }
    }
}
#[test]
fn duration_literals_reject_integer_mixing_and_negative_values() {
    for source in [
        "fn f() -> UInt64 { 250ms }",
        "fn f() { let d = -1ms; }",
        "import joky/task fn f() @suspends { task.sleep(250); }",
        "import joky/task fn f() { task.sleep(1s); }",
    ] {
        frontend_error(source, &crate::ModuleSources::new());
    }
}
#[test]
fn duration_literals_are_hygienic_without_prelude() {
    for source in [
        "fn f() -> Int32 { int32(250ms.as_nanos()) }",
        "import joky/time as clock; fn f() -> Int32 { int32(250ms.as_nanos()) }",
        "import joky/time.{type Duration as Delay}; fn f() -> Int32 { let d: Delay = 250ms; int32(d.as_nanos()) }",
        "fn f() -> Int32 { let time = 1; let Duration = 2; int32(250ms.as_nanos()) }",
    ] {
        let vm = parity_vm_with_prelude(source, VmMode::Interpreter, false);
        assert_eq!(vm.call_int32_0("f").unwrap(), 250000000);
    }
    let vm = parity_vm_with_prelude(
        "fn f() -> Int32 { let text = \"{250ms}\"; int32(text.byte_length()) }",
        VmMode::Interpreter,
        false,
    );
    assert_eq!(vm.call_int32_0("f").unwrap(), 5);
}
fn cancellation(sweep: CancellationSweep) {
    let mut vm = parity_vm(SOURCE, VmMode::Interpreter);
    for entry in [
        "duration_success",
        "duration_elapsed",
        "duration_owned_result",
        "duration_place_result",
    ] {
        assert_cancellation(
            &mut vm,
            entry,
            100000,
            sweep,
            || {
                READY.with(|r| r.set(true));
                EVENTS.with(|e| e.borrow_mut().clear());
            },
            || EVENTS.with(|e| e.borrow().clone()),
            cleaned,
        );
    }
}
#[test]
fn duration_timeout_cancellation_at_resource_boundaries() {
    cancellation(CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel timeout captures and results at every instruction"]
fn exhaustive_duration_timeout_cancellation() {
    cancellation(CancellationSweep::EveryInstruction);
}

#[test]
fn duration_sleep_and_timeout_execute_on_parallel_workers() {
    use crate::{
        runtime::parallel::ParallelExecutor,
        vm::{InstanceValue, TaskValue},
    };
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let prepared = vm
            .prepare_parallel_task("duration_parallel", vec![], vec![])
            .unwrap_or_else(|e| panic!("{e}"));
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

#[test]
fn duration_entry_points_reject_untyped_and_negative_delays() {
    for source in [
        "import joky/domain fn f() @suspends { let d = domain.deadline_after(1); }",
        "import joky/domain fn f() @suspends { let d = domain.deadline_after(-1ms); }",
        "import joky/session fn f() { var options = session.defaults(); options.idle_timeout = 1000; }",
        "import joky/session fn f() { var options = session.defaults(); options.idle_ms = 1000; }",
    ] { frontend_error(source, &crate::ModuleSources::new()); }
    for (operation, delay) in [("after", "1"), ("after", "-1ms"), ("every", "1")] {
        let source = format!("import joky/domain struct S {{ n:Int32 }} fn tick(s:&mut S,n:Int32) {{s.n+=n}} fn f(t:domain.TimerHandle,h:domain.WriteHandle<S>) @suspends {{domain.{operation}(t,h,{delay},tick,1);}}");
        frontend_error(&source, &crate::ModuleSources::new());
    }
}
