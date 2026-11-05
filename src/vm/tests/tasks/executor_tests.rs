use super::{call_current_instance, InstanceStatus, InstanceValue as V, VmEngine, VmMode};
use crate::{
    runtime::{
        arena::LIVE,
        cps::{CpsFrame, DONE},
        executor::{current_waker, Clock, HostExecutor, TaskGroup, TaskWaker},
    },
    FunctionSignature, Type,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

struct ManualClock(Cell<Instant>);
impl ManualClock {
    fn new() -> Rc<Self> {
        Rc::new(Self(Cell::new(Instant::now())))
    }
    fn advance(&self, ms: u64) {
        self.0.set(self.0.get() + Duration::from_millis(ms));
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
thread_local! {
    static TOKEN: RefCell<Option<TaskWaker>> = const { RefCell::new(None) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static POLLS: Cell<usize> = const { Cell::new(0) };
    static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    static SHUTDOWN_PROBE: Cell<bool> = const { Cell::new(false) };
    static SHUTDOWN_DENIED: Cell<bool> = const { Cell::new(false) };
    static CLOSE_IN_STOP: Cell<bool> = const { Cell::new(false) };
    static ACTIVE_VM: Cell<*const VmEngine> = const { Cell::new(std::ptr::null()) };
    static ACTIVE_ID: Cell<Option<super::InstanceId>> = const { Cell::new(None) };
    static FAILED_OWNER: Cell<Option<super::InstanceId>> = const { Cell::new(None) };
}
unsafe extern "C" fn wait(frame: *mut CpsFrame) -> i32 {
    POLLS.with(|v| v.set(v.get() + 1));
    TOKEN.with(|v| *v.borrow_mut() = current_waker());
    if READY.with(Cell::get) {
        (*frame).state = DONE;
        0
    } else {
        1
    }
}
unsafe extern "C" fn stop_hold(frame: *mut CpsFrame, reason: u32) {
    assert_eq!(reason, crate::runtime::borrowed_host::STOP_CANCELLED);
    assert_eq!(*((&(*frame).slots)[0] as *const i32), 42);
    event(90);
    if CLOSE_IN_STOP.with(Cell::get) {
        ACTIVE_VM.with(|v| {
            ACTIVE_ID.with(|id| {
                let vm = &*v.get();
                vm.close_instance(id.get().unwrap()).unwrap();
                assert!(
                    vm.instance_bytes() > 0,
                    "stop must finish before State drops"
                );
            })
        });
        assert_eq!(*((&(*frame).slots)[0] as *const i32), 42);
    }
}
unsafe extern "C" fn event(n: i32) {
    EVENTS.with(|v| v.borrow_mut().push(n));
    if n == 99 && SHUTDOWN_PROBE.with(Cell::get) {
        ACTIVE_VM.with(|v| {
            let result = (&*v.get()).create_instance("create", &[]);
            SHUTDOWN_DENIED
                .with(|d| d.set(result.is_err_and(|e| e.message().contains("shutting down"))));
        });
    }
}
unsafe extern "C" fn add(n: i32) -> i32 {
    let method = match n {
        -100 => "fail_state",
        -200 => "close_state",
        _ => "add",
    };
    match call_current_instance(method, &[V::Int32(n)]) {
        Ok(V::Int32(n)) => n,
        Ok(_) => unreachable!(),
        Err(e) => {
            crate::runtime::error::record_error(e);
            0
        }
    }
}
unsafe extern "C" fn close_owner() {
    ACTIVE_VM.with(|vm| {
        ACTIVE_ID.with(|id| {
            let vm = &*vm.get();
            if let Some(failed) = FAILED_OWNER.with(Cell::get) {
                assert_eq!(vm.instance_status(failed).unwrap(), InstanceStatus::Failed);
                assert!(vm.call_instance(failed, "read", &[]).is_err());
            }
            vm.close_instance(id.get().unwrap()).unwrap();
            assert!(vm.poll_tasks(1).is_err());
            assert!(
                vm.instance_bytes() > 0,
                "active task must retain state until stop"
            );
        })
    });
}
fn reset() {
    TOKEN.with(|v| v.borrow_mut().take());
    READY.with(|v| v.set(false));
    POLLS.with(|v| v.set(0));
    EVENTS.with(|v| v.borrow_mut().clear());
}
fn vm(mode: VmMode, source: &str) -> VmEngine {
    reset();
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_borrowing_suspending_fn(
            "hold",
            FunctionSignature::new(vec![Type::reference(Type::Int32, true)], Type::Unit),
            crate::runtime::borrowed_host::BorrowingHostV1 {
                abi_version: 1,
                poll: Some(wait),
                stop: Some(stop_hold),
            },
            None,
        )
        .unwrap();
        vm.register_suspending_fn("wait", FunctionSignature::new(vec![], Type::Unit), wait)
            .unwrap();
        vm.register_fn(
            "event",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            event as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "change",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            add as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "close_owner",
            FunctionSignature::new(vec![], Type::Unit),
            close_owner as *const u8,
        )
        .unwrap();
    }
    vm.load_source(source)
        .unwrap_or_else(|e| panic!("{}", e.message()));
    vm
}
const SOURCE: &str = include_str!("../../../../tests/fixtures/host_executor.jk");

#[test]
fn host_executor_wakes_timers_without_polling_pending_roots() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        let clock = ManualClock::new();
        let executor = HostExecutor::new(clock.clone(), 4);
        let group = TaskGroup::new();
        let id = executor
            .spawn(&group, vm.prepare_async("work", &[]).unwrap())
            .unwrap();
        assert_eq!(POLLS.with(Cell::get), 0);
        assert_eq!(executor.poll(0).unwrap().polled, 0);
        assert_eq!(executor.poll(1).unwrap().polled, 1);
        let token = TOKEN.with(|v| v.borrow().clone().unwrap());
        for _ in 0..10 {
            assert_eq!(executor.poll(20).unwrap().polled, 0);
        }
        assert_eq!(POLLS.with(Cell::get), 1);
        READY.with(|v| v.set(true));
        std::thread::spawn({
            let token = token.clone();
            move || {
                token.wake();
                token.wake();
            }
        })
        .join()
        .unwrap();
        let report = executor.poll(20).unwrap();
        assert_eq!(report.polled, 1);
        assert_eq!(
            report.next_deadline,
            Some(clock.now() + Duration::from_millis(10))
        );
        clock.advance(9);
        assert_eq!(executor.poll(20).unwrap().polled, 0);
        clock.advance(1);
        assert_eq!(executor.poll(20).unwrap().polled, 1);
        let report = executor.poll(20).unwrap();
        assert_eq!(report.completed.len(), 1);
        assert_eq!(report.completed[0].id, id);
        assert_eq!(report.completed[0].result, Ok(42));
        token.wake();
        assert_eq!(executor.poll(20).unwrap().polled, 0);
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 2, 3]);
        assert_eq!(executor.task_count(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn host_executor_budget_is_fair_and_groups_are_not_reusable_after_close() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        for n in 1..=3 {
            executor
                .spawn(&group, vm.prepare_async("fair", &[V::Int32(n)]).unwrap())
                .unwrap();
        }
        for _ in 0..6 {
            assert_eq!(executor.poll(1).unwrap().polled, 1);
        }
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 2, 3, 1, 2, 3]);
        assert!(executor.cancel_group(&group));
        assert!(executor
            .spawn(&group, vm.prepare_async("work", &[]).unwrap())
            .is_err());
        let other = TaskGroup::new();
        executor
            .spawn(&other, vm.prepare_async("work", &[]).unwrap())
            .unwrap();
        assert!(HostExecutor::default()
            .spawn(&other, vm.prepare_async("work", &[]).unwrap())
            .is_err());
        executor.poll(1).unwrap();
        let token = TOKEN.with(|v| v.borrow().clone().unwrap());
        executor.cancel_group(&other);
        token.wake();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
    }
}

const INSTANCE: &str = r#"import joky/mem

import joky/task
import joky/drop.{type Drop}
struct State { count: Int32, text: String }
fn create() -> State { State { count: 0, text: "state" } }
fn add(self: &mut State, n: Int32) @noescape -> Int32 { self.count = self.count + n; self.count }
fn read(self: &State) @noescape -> Int32 { self.count }
fn fail_state(self: &mut State, n: Int32) @noescape -> Int32 {
    mem.replace(&mut self.text, "changed");
    n / 0
}
fn close_state(self: &mut State, n: Int32) @noescape -> Int32 {
    close_owner()
    mem.replace(&mut self.text, "changed");
    self.count + n
}
fn fail_flow() @suspends { change(-100); }
fn closing_method() @suspends { change(-200); }
extern fn change(n: Int32) -> Int32
extern fn wait() @suspends
extern fn event(n: Int32)
extern fn close_owner()
extern fn hold(value: &mut Int32) @suspends
fn held() @suspends { change(1); let temp = "task"; let value = alloc(42); hold(value); temp.release() }
impl Drop for State { fn drop(&mut self) { event(99) } }
fn flow() @suspends -> Int32 { let n = change(1); wait(); change(n) }
fn closing() @suspends { scope { branch { wait() }; close_owner(); task.yield_now() } }
fn bad() @suspends { task.yield_now(); let n = 1 / 0 }
"#;
#[test]
fn instance_tasks_use_short_access_and_pinned_modules_after_reload() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = vm(mode, INSTANCE);
        let id = vm.create_instance("create", &[]).unwrap();
        let other = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "flow", &[]).unwrap();
        vm.poll_tasks(10).unwrap();
        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Ready);
        assert_eq!(
            vm.call_instance(id, "add", &[V::Int32(10)]).unwrap(),
            V::Int32(11)
        );
        assert_eq!(vm.call_instance(other, "read", &[]).unwrap(), V::Int32(0));
        vm.load_source(&INSTANCE.replace("self.count + n", "self.count + n + 100"))
            .unwrap();
        READY.with(|v| v.set(true));
        TOKEN.with(|v| v.borrow().as_ref().unwrap().wake());
        assert_eq!(vm.poll_tasks(10).unwrap().completed[0].result, Ok(12));
        // Even new tasks on an old instance use its old code version.
        vm.spawn_instance_task(id, "flow", &[]).unwrap();
        assert_eq!(vm.poll_tasks(10).unwrap().completed[0].result, Ok(26));
        vm.close_instance(id).unwrap();
        vm.close_instance(other).unwrap();
        assert_eq!(vm.instance_bytes(), 0);
        assert_eq!(vm.instance_task_count(), 0);
        assert!(vm.spawn_instance_task(id, "flow", &[]).is_err());
        assert!(call_current_instance("read", &[]).is_err());
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn instance_tasks_close_reentrantly_and_fail_without_leaking_state() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, INSTANCE);
        let id = vm.create_instance("create", &[]).unwrap();
        ACTIVE_VM.with(|v| v.set(&vm));
        ACTIVE_ID.with(|v| v.set(Some(id)));
        vm.spawn_instance_task(id, "closing", &[]).unwrap();
        vm.poll_tasks(10).unwrap();
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        assert_eq!(vm.instance_task_count(), 0);
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [99]);
        let id = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "flow", &[]).unwrap();
        vm.spawn_instance_task(id, "bad", &[]).unwrap();
        vm.poll_tasks(10).unwrap();
        let stale = TOKEN.with(|v| v.borrow().clone().unwrap());
        let report = vm.poll_tasks(10).unwrap();
        assert!(report.completed.iter().any(|r| r.result.is_err()));
        assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
        assert_eq!(vm.instance_task_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        stale.wake();
        assert_eq!(vm.poll_tasks(10).unwrap().polled, 0);
        vm.close_instance(id).unwrap();
        let id = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "flow", &[]).unwrap();
        vm.poll_tasks(10).unwrap();
        drop(vm);
        ACTIVE_VM.with(|v| v.set(std::ptr::null()));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn instance_tasks_stop_hosts_before_dropping_task_and_instance_owners() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for reentrant in [false, true] {
            let vm = vm(mode, INSTANCE);
            let id = vm.create_instance("create", &[]).unwrap();
            ACTIVE_VM.with(|v| v.set(&vm));
            ACTIVE_ID.with(|v| v.set(Some(id)));
            vm.spawn_instance_task(id, "held", &[]).unwrap();
            vm.poll_tasks(1).unwrap();
            let token = TOKEN.with(|v| v.borrow().clone().unwrap());
            if reentrant {
                vm.spawn_instance_task(id, "closing", &[]).unwrap();
                vm.poll_tasks(10).unwrap();
            } else {
                vm.close_instance(id).unwrap();
            }
            assert_eq!(EVENTS.with(|v| v.borrow().clone()), [90, 99]);
            token.wake();
            assert_eq!(vm.poll_tasks(10).unwrap().polled, 0);
            assert_eq!(vm.instance_bytes(), 0);
            assert_eq!(vm.instance_task_count(), 0);
            LIVE.with(|n| assert_eq!(n.get(), 0));
            ACTIVE_VM.with(|v| v.set(std::ptr::null()));
        }
    }
}

#[test]
fn host_executor_checks_mixed_scalar_inputs_and_does_not_run_rejected_roots() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(
            mode,
            r#"
            import joky/task
            extern fn event(n: Int32)
            fn mixed(a: Int8, u: Unit, b: UInt64, c: Float64, d: Bool, r: Rune) @suspends -> Float64 {
                event(if r == 'A' { 65 } else { 64 }); task.yield_now(); if d { c + float64(a) + float64(b) } else { 0.0 }
            }
        "#,
        );
        let executor = HostExecutor::new(ManualClock::new(), 1);
        let group = TaskGroup::new();
        let args = [
            V::Int8(-2),
            V::Unit,
            V::UInt64(3),
            V::Float64(1.5),
            V::Bool(true),
            V::Rune('A'),
        ];
        assert!(vm.prepare_async("mixed", &args[..5]).is_err());
        executor
            .spawn(&group, vm.prepare_async("mixed", &args).unwrap())
            .unwrap();
        assert!(executor
            .spawn(&group, vm.prepare_async("mixed", &args).unwrap())
            .is_err());
        assert!(EVENTS.with(|v| v.borrow().is_empty()));
        executor.poll(10).unwrap();
        assert_eq!(
            executor.poll(10).unwrap().completed[0].result,
            Ok(2.5f64.to_bits())
        );
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [65]);
    }
}

fn cancel_instance_steps(exhaustive: bool) {
    let mut vm = vm(VmMode::Interpreter, INSTANCE);
    vm.set_interpreter_limits(super::InterpreterLimits {
        poll_budget: Some(1),
        ..Default::default()
    })
    .unwrap();
    let cuts: Vec<_> = if exhaustive {
        (0..=96).collect()
    } else {
        vec![0, 1, 2, 4, 8, 16, 32, 64, 96]
    };
    for cut in cuts {
        reset();
        let id = vm.create_instance("create", &[]).unwrap();
        vm.spawn_instance_task(id, "held", &[]).unwrap();
        for _ in 0..cut {
            vm.poll_tasks(1).unwrap();
        }
        let token = TOKEN.with(|v| v.borrow().clone());
        vm.close_instance(id).unwrap();
        if let Some(token) = token {
            token.wake();
        }
        assert_eq!(vm.poll_tasks(10).unwrap().polled, 0);
        assert_eq!(vm.instance_task_count(), 0);
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0, "cut {cut}"));
    }
    assert!(
        POLLS.with(Cell::get) > 0,
        "sweep must reach the pending host"
    );
}
#[test]
fn instance_tasks_cancel_at_instruction_boundaries() {
    cancel_instance_steps(false);
}
#[test]
#[ignore = "exhaustive: cancel managed instance tasks at each instruction"]
fn exhaustive_instance_tasks_cancel_at_every_instruction() {
    cancel_instance_steps(true);
}

#[test]
fn host_executor_failure_cancels_ready_siblings_before_their_next_poll() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(
            mode,
            r#"
            import joky/task
            extern fn event(n: Int32)
            fn bad() @suspends { task.yield_now(); let n = 1 / 0 }
            fn sibling() @suspends { task.yield_now(); event(99) }
        "#,
        );
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("bad", &[]).unwrap())
            .unwrap();
        executor
            .spawn(&group, vm.prepare_async("sibling", &[]).unwrap())
            .unwrap();
        executor.poll(2).unwrap();
        let report = executor.poll(2).unwrap();
        assert_eq!(report.polled, 1);
        assert_eq!(report.completed.len(), 1);
        assert!(report.completed[0].result.is_err());
        assert!(group.is_closed());
        assert_eq!(executor.task_count(), 0);
        assert!(EVENTS.with(|v| v.borrow().is_empty()));
    }
}

#[test]
fn instance_task_failure_keeps_its_owner_during_reentrant_batch_cleanup() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let source = format!("{INSTANCE}\nfn close_other() @suspends {{ close_owner() }}");
        let vm = vm(mode, &source);
        let failed = vm.create_instance("create", &[]).unwrap();
        let caller = vm.create_instance("create", &[]).unwrap();
        let closed = vm.create_instance("create", &[]).unwrap();
        FAILED_OWNER.with(|v| v.set(Some(failed)));
        ACTIVE_VM.with(|v| v.set(&vm));
        ACTIVE_ID.with(|v| v.set(Some(closed)));
        let bad = vm.spawn_instance_task(failed, "bad", &[]).unwrap();
        vm.poll_tasks(1).unwrap(); // bad is ready to fail in the next batch.
        vm.spawn_instance_task(caller, "close_other", &[]).unwrap();
        let report = vm.poll_tasks(8).unwrap();
        assert_eq!(report.completed.len(), 2);
        assert_eq!(report.completed[0].id, bad);
        assert!(report.completed[0].result.is_err());
        assert!(report.completed[1].result.is_ok());
        assert_eq!(vm.instance_status(failed).unwrap(), InstanceStatus::Failed);
        assert!(vm.instance_error(failed).unwrap().is_some());
        assert!(vm.call_instance(failed, "read", &[]).is_err());
        assert_eq!(vm.call_instance(caller, "read", &[]).unwrap(), V::Int32(0));
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [99, 99]);
        vm.close_instance(failed).unwrap(); // failed State must not drop twice.
        vm.close_instance(caller).unwrap();
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [99, 99, 99]);
        assert_eq!(vm.instance_bytes(), 0);
        assert_eq!(vm.instance_task_count(), 0);
        FAILED_OWNER.with(|v| v.set(None));
        ACTIVE_VM.with(|v| v.set(std::ptr::null()));
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn instance_tasks_failed_or_closed_state_access_keeps_arena_until_hosts_stop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for method in ["fail_flow", "closing_method"] {
            let vm = vm(mode, INSTANCE);
            let id = vm.create_instance("create", &[]).unwrap();
            ACTIVE_VM.with(|v| v.set(&vm));
            ACTIVE_ID.with(|v| v.set(Some(id)));
            vm.spawn_instance_task(id, "held", &[]).unwrap();
            vm.poll_tasks(1).unwrap();
            vm.spawn_instance_task(id, method, &[]).unwrap();
            vm.poll_tasks(1).unwrap();
            assert_eq!(EVENTS.with(|v| v.borrow().clone()), [90, 99]);
            assert_eq!(vm.instance_bytes(), 0);
            assert_eq!(vm.instance_task_count(), 0);
            if method == "fail_flow" {
                assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
                vm.close_instance(id).unwrap();
            }
            LIVE.with(|n| assert_eq!(n.get(), 0));
            ACTIVE_VM.with(|v| v.set(std::ptr::null()));
        }
    }
}

fn instance_task_cycles(rounds: usize) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, INSTANCE);
        let id = vm.create_instance("create", &[]).unwrap();
        let baseline = vm.instance_bytes();
        let mut oldest = None;
        READY.with(|v| v.set(true));
        for _ in 0..rounds {
            vm.spawn_instance_task(id, "flow", &[]).unwrap();
            let report = vm.poll_tasks(1).unwrap();
            assert_eq!(report.completed.len(), 1);
            assert!(report.completed[0].result.is_ok());
            if oldest.is_none() {
                oldest = TOKEN.with(|t| t.borrow().clone());
            }
            oldest.as_ref().unwrap().wake();
            assert_eq!(vm.poll_tasks(1).unwrap().polled, 0);
            assert_eq!(vm.instance_bytes(), baseline);
            assert_eq!(vm.instance_task_count(), 0);
        }
        vm.close_instance(id).unwrap();
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn instance_tasks_reclaim_completed_roots_and_ignore_old_notifications() {
    instance_task_cycles(32);
}
#[test]
#[ignore = "exhaustive: repeated instance task completion and stale notifications"]
fn exhaustive_instance_tasks_reclaim_completed_roots() {
    instance_task_cycles(2000);
}

#[test]
fn host_executor_structured_children_keep_their_wake_sources() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, SOURCE);
        let clock = ManualClock::new();
        let executor = HostExecutor::new(clock.clone(), 2);
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("branches", &[]).unwrap())
            .unwrap();
        executor.poll(1).unwrap();
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        clock.advance(5);
        executor.poll(1).unwrap();
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [1, 5]);
        executor.cancel_group(&group);
        TOKEN.with(|v| v.borrow().as_ref().unwrap().wake());
        assert_eq!(executor.poll(1).unwrap().polled, 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}

#[test]
fn host_executor_preserves_external_wakes_across_sliced_siblings_without_spinning() {
    let source = r#"
        extern fn wait() @suspends
        extern fn event(value: Int32)
        fn sibling() @suspends {
            var value = 0
            while value < 20 { value = value + 1 }
            event(value)
            wait()
        }
        fn run() @suspends -> Int32 { join { wait(), sibling() }; 42 }
    "#;
    for (mode, budget) in [
        (VmMode::Jit, None),
        (VmMode::Interpreter, Some(1)),
        (VmMode::Interpreter, Some(2)),
        (VmMode::Interpreter, Some(7)),
    ] {
        for during_round in [true, false] {
            let mut vm = vm(mode, source);
            if mode == VmMode::Interpreter {
                vm.set_interpreter_limits(super::InterpreterLimits {
                    poll_budget: budget,
                    ..Default::default()
                })
                .unwrap();
            }
            let executor = HostExecutor::default();
            let id = executor
                .spawn(&TaskGroup::new(), vm.prepare_async("run", &[]).unwrap())
                .unwrap();
            let mut reached = false;
            for _ in 0..2000 {
                let report = executor.poll(1).unwrap();
                assert!(report.completed.is_empty());
                if if during_round {
                    POLLS.with(Cell::get) > 0
                } else {
                    report.polled == 0
                } {
                    reached = true;
                    break;
                }
            }
            assert!(reached, "{mode:?} {budget:?}: pending hosts did not settle");
            if !during_round {
                assert_eq!(POLLS.with(Cell::get), 2);
                for _ in 0..8 {
                    assert_eq!(executor.poll(1).unwrap().polled, 0);
                }
                assert_eq!(POLLS.with(Cell::get), 2);
            }
            let token = TOKEN.with(|v| v.borrow().clone().unwrap());
            READY.with(|v| v.set(true));
            std::thread::spawn({
                let token = token.clone();
                move || {
                    token.wake();
                    token.wake();
                }
            })
            .join()
            .unwrap();
            let mut completed = false;
            for _ in 0..2000 {
                let report = executor.poll(1).unwrap();
                assert!(report.polled > 0, "{mode:?} {budget:?}: notification lost");
                if let Some(done) = report.completed.into_iter().next() {
                    assert_eq!(done.id, id);
                    assert_eq!(done.result, Ok(42));
                    completed = true;
                    break;
                }
            }
            assert!(completed);
            assert_eq!(EVENTS.with(|v| v.borrow().clone()), [20]);
            token.wake();
            assert_eq!(executor.poll(1).unwrap().polled, 0);
            assert_eq!(executor.task_count(), 0);
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
fn instance_tasks_defer_owner_destruction_requested_inside_stop() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = vm(mode, INSTANCE);
        let id = vm.create_instance("create", &[]).unwrap();
        ACTIVE_VM.with(|v| v.set(&vm));
        ACTIVE_ID.with(|v| v.set(Some(id)));
        let task = vm.spawn_instance_task(id, "held", &[]).unwrap();
        vm.poll_tasks(1).unwrap();
        CLOSE_IN_STOP.with(|v| v.set(true));
        assert!(vm.cancel_instance_task(task));
        CLOSE_IN_STOP.with(|v| v.set(false));
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [90, 99]);
        assert_eq!(vm.instance_count(), 0);
        assert_eq!(vm.instance_bytes(), 0);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        ACTIVE_VM.with(|v| v.set(std::ptr::null()));
    }
}

#[test]
fn instance_tasks_vm_shutdown_rejects_new_roots_from_destructors() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        // Keep the address stable while Drop calls the native probe.
        let vm = Box::new(vm(mode, INSTANCE));
        let id = vm.create_instance("create", &[]).unwrap();
        ACTIVE_VM.with(|v| v.set(&*vm));
        vm.spawn_instance_task(id, "held", &[]).unwrap();
        vm.poll_tasks(1).unwrap();
        SHUTDOWN_PROBE.with(|v| v.set(true));
        SHUTDOWN_DENIED.with(|v| v.set(false));
        drop(vm);
        SHUTDOWN_PROBE.with(|v| v.set(false));
        assert!(SHUTDOWN_DENIED.with(Cell::get));
        assert_eq!(EVENTS.with(|v| v.borrow().clone()), [90, 99]);
        LIVE.with(|n| assert_eq!(n.get(), 0));
        ACTIVE_VM.with(|v| v.set(std::ptr::null()));
    }
}

thread_local! { static IDENTITIES: RefCell<Vec<Option<super::InstanceId>>> = const { RefCell::new(Vec::new()) }; }
extern "C" fn identity_cleanup() {
    IDENTITIES.with(|ids| ids.borrow_mut().push(super::current_instance_id()));
}
unsafe extern "C" fn identity_poll(_: *mut CpsFrame) -> i32 {
    IDENTITIES.with(|ids| ids.borrow_mut().push(super::current_instance_id()));
    1
}
unsafe extern "C" fn identity_stop(_: *mut CpsFrame, _: u32) {
    IDENTITIES.with(|ids| ids.borrow_mut().push(super::current_instance_id()));
}
#[test]
fn instance_identity_is_scoped_to_poll_and_disposal_for_each_owner() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        IDENTITIES.with(|ids| ids.borrow_mut().clear());
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "identity_cleanup",
                FunctionSignature::new(vec![], Type::Unit),
                identity_cleanup as *const u8,
            )
            .unwrap();
            vm.register_borrowing_suspending_fn(
                "probe",
                FunctionSignature::new(vec![Type::reference(Type::Int32, false)], Type::Unit),
                crate::runtime::borrowed_host::BorrowingHostV1 {
                    abi_version: 1,
                    poll: Some(identity_poll),
                    stop: Some(identity_stop),
                },
                None,
            )
            .unwrap();
        }
        vm.load_source("import joky/drop.{type Drop} struct State { n: Int32, text: String } fn create() -> State { State { n: 0, text: \"owned\" } } extern fn identity_cleanup() impl Drop for State { fn drop(&mut self) { identity_cleanup() } } extern fn probe(n: &Int32) @suspends fn work() @suspends { let n = alloc(42); probe(n) }").unwrap();
        let a = vm.create_instance("create", &[]).unwrap();
        let b = vm.create_instance("create", &[]).unwrap();
        let ta = vm.spawn_instance_task(a, "work", &[]).unwrap();
        vm.spawn_instance_task(b, "work", &[]).unwrap();
        assert_eq!(super::current_instance_id(), None);
        vm.poll_tasks(8).unwrap();
        assert_eq!(super::current_instance_id(), None);
        vm.cancel_instance_task(ta);
        assert_eq!(super::current_instance_id(), None);
        vm.close_instance(b).unwrap();
        assert_eq!(super::current_instance_id(), None);
        assert_eq!(
            IDENTITIES.with(|ids| ids.borrow().clone()),
            [Some(a), Some(b), Some(a), Some(b), Some(b)]
        );
        vm.close_instance(a).unwrap();
    }
}

unsafe extern "C" fn limited_state_access() -> i32 {
    match call_current_instance("spin", &[]) {
        Ok(V::Int32(v)) => v,
        Ok(_) => unreachable!(),
        Err(error) => {
            crate::runtime::error::record_error(error);
            0
        }
    }
}
#[test]
fn instance_task_poll_limits_do_not_relax_synchronous_state_limits() {
    let mut vm = VmEngine::new_interpreter();
    vm.set_interpreter_limits(super::InterpreterLimits {
        max_frames: 16,
        instruction_budget: Some(128),
        poll_budget: None,
        ..Default::default()
    })
    .unwrap();
    unsafe {
        vm.register_fn(
            "limited_state_access",
            FunctionSignature::new(vec![], Type::Int32),
            limited_state_access as *const u8,
        )
        .unwrap();
    }
    vm.load_source("struct State { n: Int32 } fn create() -> State { State { n: 0 } } fn spin(s: &mut State) @noescape -> Int32 { var n = 0; while n < 1000 { n = n + 1 }; s.n = n; n } extern fn limited_state_access() -> Int32 fn long() @suspends -> Int32 { var n = 0; while n < 1000 { n = n + 1 }; n } fn bad() @suspends -> Int32 { limited_state_access() }").unwrap();
    let id = vm.create_instance("create", &[]).unwrap();
    let limits = super::InterpreterLimits {
        max_frames: 16,
        instruction_budget: None,
        poll_budget: Some(32),
        ..Default::default()
    };
    assert!(vm
        .spawn_instance_task_with_limits(
            id,
            "long",
            &[],
            super::InterpreterLimits {
                poll_budget: Some(0),
                ..limits
            }
        )
        .is_err());
    vm.spawn_instance_task_with_limits(id, "long", &[], limits)
        .unwrap();
    let mut complete = None;
    for _ in 0..2000 {
        let report = vm.poll_tasks(1).unwrap();
        assert_eq!(report.polled, 1);
        if let Some(result) = report.completed.into_iter().next() {
            complete = Some(result.result);
            break;
        }
    }
    assert_eq!(complete, Some(Ok(1000)));
    vm.spawn_instance_task_with_limits(id, "bad", &[], limits)
        .unwrap();
    let report = vm.poll_tasks(1).unwrap();
    assert!(report.completed[0].result.is_err());
    assert_eq!(vm.instance_status(id).unwrap(), InstanceStatus::Failed);
    vm.close_instance(id).unwrap();
    assert_eq!(vm.instance_task_count(), 0);
    assert_eq!(vm.instance_bytes(), 0);
}

unsafe fn check_scalar_request(frame: *mut CpsFrame) {
    assert_eq!((&(*frame).slots)[0], 4);
    assert_eq!((&(*frame).slots)[1], 42);
    assert_eq!(f64::from_bits((&(*frame).slots)[2]), 0.125);
}
unsafe extern "C" fn scalar_request_poll(frame: *mut CpsFrame) -> i32 {
    check_scalar_request(frame);
    if (*frame).state == 0 {
        TOKEN.with(|v| *v.borrow_mut() = current_waker());
        (*frame).state = 1;
        1
    } else {
        (*frame).state = DONE;
        0
    }
}
unsafe extern "C" fn scalar_request_stop(frame: *mut CpsFrame, _: u32) {
    check_scalar_request(frame);
    POLLS.with(|n| n.set(n.get() + 1));
}
unsafe extern "C" fn scalar_request_wait(frame: *mut CpsFrame) -> i32 {
    static HOST: crate::runtime::borrowed_host::BorrowingHostV1 =
        crate::runtime::borrowed_host::BorrowingHostV1 {
            abi_version: 1,
            poll: Some(scalar_request_poll),
            stop: Some(scalar_request_stop),
        };
    crate::runtime::borrowed_host::joky_cps_poll_borrowing_v1(frame, &HOST)
}
#[test]
fn repeated_scalar_wait_requests_do_not_accumulate_arena_storage() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        reset();
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_suspending_fn(
                "wait_request",
                FunctionSignature::new(vec![Type::Int32, Type::Int32, Type::Float64], Type::Unit),
                scalar_request_wait,
            )
            .unwrap();
        }
        vm.load_source(include_str!(
            "../../../../tests/fixtures/scalar_wait_requests.jk"
        ))
        .unwrap_or_else(|e| panic!("{}", e.message()));
        let executor = HostExecutor::default();
        let group = TaskGroup::new();
        executor
            .spawn(&group, vm.prepare_async("wait_loop", &[]).unwrap())
            .unwrap();
        let mut result = None;
        for _ in 0..2001 {
            let report = executor.poll(1).unwrap();
            assert_eq!(
                LIVE.with(Cell::get),
                0,
                "scalar waits allocated arena storage"
            );
            if let Some(done) = report.completed.into_iter().next() {
                result = Some(done.result);
                break;
            }
            TOKEN.with(|v| v.borrow().as_ref().unwrap().wake());
        }
        assert_eq!(result, Some(Ok(2000)));
        assert_eq!(POLLS.with(Cell::get), 2000);
        assert_eq!(LIVE.with(Cell::get), 0);
    }
}

#[path = "controlled_tests.rs"]
mod controlled_tests;
