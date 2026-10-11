//! Generated-code coverage for the scalar domain foundation.
use super::*;

pub(crate) const SOURCE: &str = r#"
import joky/domain
import joky/task

struct State { count: Int32, scale: Float64, flag: Bool }
fn make(value: Int32) -> State { State { count: value, scale: 0.5, flag: true } }
fn add_impl(value: Int32, delta: Int32) -> Int32 { value + delta }
fn add(state: &mut State, delta: Int32) @noescape -> Int32 {
    state.count = add_impl(state.count, delta)
    state.count
}
fn read(state: &State) @noescape -> Int32 { state.count }
fn scale(state: &State) @noescape -> Float64 { state.scale }
fn fail_read(state: &State, zero: Int32) @noescape -> Int32 { state.count / zero }
fn fail_write(state: &mut State, zero: Int32) @noescape -> Int32 {
    state.count = 99
    state.count / zero
}
fn rejected(state: &mut State) @noescape -> Bool { state.count = 17 false }
struct Other { count: Int32 }
fn wrong(state: &Other) @noescape -> Int32 { state.count }
fn pending() @suspends -> Int32 { 1 }
fn request(delta: Int32) @suspends -> Int32 { domain.write(add, delta) }
fn query() @suspends -> Int32 { domain.read(read) }
fn broken(zero: Int32) @suspends -> Int32 { 1 / zero }
fn idle() @suspends -> Int32 { task.sleep_ms(10000) 1 }
fn request_unit() @suspends { request(1000); }
fn ready_unit() @suspends {}
fn broken_unit() @suspends { broken(0); }
fn raced() @suspends -> Int32 { race { | request_unit() | ready_unit() } 0 }
fn faulty() @suspends -> Int32 { parallel { | request_unit() | broken_unit() } 0 }
fn doubled(delta: Int32) @suspends -> Int32 {
    let first = domain.write(add, delta)
    task.yield_now()
    first + domain.read(read)
}

"#;

fn value(request: &mut DomainRequest, expected: InstanceValue) {
    match request.take_result() {
        Some(DomainOutcome::Value(actual)) => assert_eq!(actual, expected),
        other => panic!("unexpected domain outcome: {other:?}"),
    }
    assert!(request.take_result().is_none());
}

pub(crate) fn exercise(vm: &mut VmEngine) {
    use InstanceValue::{Bool, Float64, Int32};
    let before = crate::runtime::arena::LIVE.with(|n| n.get());
    let owner = vm.create_domain("make", &[Int32(10)]).unwrap();
    let read = owner.read_handle();
    let write = owner.write_handle();
    assert!(vm.queue_domain_call(read, "add", &[Int32(2)]).is_err());
    assert!(vm
        .queue_domain_call(write.read_only(), "add", &[Int32(2)])
        .is_err());
    assert!(vm.queue_domain_call(write, "wrong", &[]).is_err());
    assert!(vm.queue_domain_call(write, "add", &[Float64(2.0)]).is_err());
    assert!(vm.queue_domain_call(write, "pending", &[]).is_err());
    assert!(VmEngine::new().domain_revision(read).is_err());
    let mut first = vm.queue_domain_call(write, "add", &[Int32(2)]).unwrap();
    let mut second = vm.queue_domain_call(read, "read", &[]).unwrap();
    let mut third = vm.queue_domain_call(write, "add", &[Int32(3)]).unwrap();
    assert!(first.take_result().is_none());
    assert_eq!(vm.domain_revision(read).unwrap(), 0);
    assert_eq!(vm.poll_tasks(0).unwrap().polled, 0);
    assert_eq!(vm.poll_tasks(2).unwrap().polled, 2);
    value(&mut first, Int32(12));
    value(&mut second, Int32(12));
    assert_eq!(vm.domain_revision(read).unwrap(), 1);
    assert!(third.take_result().is_none());
    assert_eq!(vm.poll_tasks(1).unwrap().polled, 1);
    value(&mut third, Int32(15));
    let mut scaled = vm.queue_domain_call(read, "scale", &[]).unwrap();
    let mut failed = vm
        .queue_domain_call(read, "fail_read", &[Int32(0)])
        .unwrap();
    let mut business = vm.queue_domain_call(write, "rejected", &[]).unwrap();
    assert_eq!(vm.poll_tasks(10).unwrap().polled, 3);
    value(&mut scaled, Float64(0.5));
    assert!(matches!(
        failed.take_result(),
        Some(DomainOutcome::Failed(_))
    ));
    value(&mut business, Bool(false));
    assert_eq!(vm.domain_revision(read).unwrap(), 3);
    let mut peek = vm.queue_domain_call(read, "read", &[]).unwrap();
    vm.poll_tasks(1).unwrap();
    value(&mut peek, Int32(17));

    let mut cancelled = vm.queue_domain_call(write, "add", &[Int32(100)]).unwrap();
    cancelled.cancel();
    assert!(matches!(
        cancelled.take_result(),
        Some(DomainOutcome::Cancelled)
    ));
    drop(vm.queue_domain_call(write, "add", &[Int32(100)]).unwrap());
    assert_eq!(vm.poll_tasks(10).unwrap().polled, 0);
    assert_eq!(vm.domain_revision(read).unwrap(), 3);

    vm.set_domain_limits(DomainLimits {
        max_requests: 1,
        ..Default::default()
    })
    .unwrap();
    let mut held = vm.queue_domain_call(read, "read", &[]).unwrap();
    assert!(vm.queue_domain_call(read, "read", &[]).is_err());
    vm.poll_tasks(1).unwrap();
    held.cancel(); // Completion wins over late cancellation.
    assert!(vm.queue_domain_call(read, "read", &[]).is_err());
    value(&mut held, Int32(17));
    let mut closed = vm.queue_domain_call(write, "add", &[Int32(1)]).unwrap();
    drop(owner);
    assert_eq!(vm.poll_tasks(10).unwrap().polled, 0);
    assert!(matches!(closed.take_result(), Some(DomainOutcome::Closed)));
    assert!(vm.domain_revision(read).is_err());
    assert!(vm.queue_domain_call(read, "read", &[]).is_err());
    vm.set_domain_limits(Default::default()).unwrap();
    let replacement = vm.create_domain("make", &[Int32(8)]).unwrap();
    assert_ne!(replacement.read_handle(), read);
    let healthy = vm.create_domain("make", &[Int32(42)]).unwrap();
    let mut poison = vm
        .queue_domain_call(replacement.write_handle(), "fail_write", &[Int32(0)])
        .unwrap();
    let mut blocked = vm
        .queue_domain_call(replacement.read_handle(), "read", &[])
        .unwrap();
    let mut independent = vm
        .queue_domain_call(healthy.read_handle(), "read", &[])
        .unwrap();
    assert_eq!(vm.poll_tasks(10).unwrap().polled, 2);
    assert!(matches!(
        poison.take_result(),
        Some(DomainOutcome::Failed(_))
    ));
    assert!(matches!(
        blocked.take_result(),
        Some(DomainOutcome::Failed(_))
    ));
    value(&mut independent, Int32(42));
    assert!(vm.domain_revision(replacement.read_handle()).is_err());
    drop(replacement);
    drop(healthy);
    assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
}

#[test]
fn state_domains_execute_checked_fifo_turns_and_isolate_failures() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        exercise(&mut vm);
    }
}

#[test]
fn state_domains_reject_owning_generic_nested_and_host_entries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            import joky/io
            struct State { count: Int32 }
            struct Owning { text: String }
            struct Nested { value: State }
            struct Generic<T> { value: T }
            fn make() -> State { State { count: 1 } }
            fn owning() -> Owning { Owning { text: "bad" } }
            fn nested() -> Nested { Nested { value: make() } }
            fn generic() -> Generic<Int32> { Generic { value: 1 } }
            fn host_helper(value: Int32) { io.println(&value) }
            fn host(state: &State) @noescape -> Int32 { host_helper(state.count) state.count }
            fn host_make() -> State { host_helper(1) make() }
            fn indirect(state: &State, value: Int32) @noescape -> Int32 {
                let f: fn(Int32) -> Int32 = identity
                f(value) + state.count
            }
            fn identity(value: Int32) -> Int32 { value }
        "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for name in ["owning", "nested", "generic", "host_make"] {
            assert!(vm.create_domain(name, &[]).is_err(), "{name}");
        }
        let owner = vm.create_domain("make", &[]).unwrap();
        assert!(vm
            .queue_domain_call(owner.read_handle(), "host", &[])
            .is_err());
        assert!(vm
            .queue_domain_call(owner.read_handle(), "indirect", &[InstanceValue::Int32(2)])
            .is_err());
        // Existing instances retain their broader storage eligibility.
        let instance = vm.create_instance("owning", &[]).unwrap();
        vm.close_instance(instance).unwrap();
    }
}

#[test]
fn state_domains_pin_type_versions_and_do_not_keep_a_vm_alive() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let before = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(5)])
            .unwrap();
        vm.load_source(&SOURCE.replace("state.count }", "state.count + 100 }"))
            .unwrap();
        let mut old = vm
            .queue_domain_call(owner.read_handle(), "read", &[])
            .unwrap();
        vm.poll_tasks(1).unwrap();
        value(&mut old, InstanceValue::Int32(5));
        let fresh = vm
            .create_domain("make", &[InstanceValue::Int32(5)])
            .unwrap();
        let mut new = vm
            .queue_domain_call(fresh.read_handle(), "read", &[])
            .unwrap();
        vm.poll_tasks(1).unwrap();
        value(&mut new, InstanceValue::Int32(105));
        let mut pending = vm
            .queue_domain_call(owner.write_handle(), "add", &[InstanceValue::Int32(1)])
            .unwrap();
        drop(vm);
        assert!(matches!(pending.take_result(), Some(DomainOutcome::Closed)));
        drop(owner);
        drop(fresh);
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
    }
}

// Shared unchanged by the VM and independently linked AOT fixture.
pub(crate) fn exercise_tasks(vm: &mut VmEngine) {
    use InstanceValue::Int32;
    let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
    let owner = vm.create_domain("make", &[Int32(10)]).unwrap();
    let first = vm
        .spawn_domain_task(owner.write_handle(), "request", &[Int32(2)])
        .unwrap();
    let second = vm
        .spawn_domain_task(owner.write_handle(), "request", &[Int32(100)])
        .unwrap();
    assert_eq!(vm.domain_task_count(owner.read_handle()).unwrap(), 2);
    let admitted = vm.poll_tasks(2).unwrap();
    assert_eq!(admitted.polled, 2);
    assert!(admitted.completed.is_empty());
    assert!(admitted.ready);
    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
    assert!(vm.cancel_domain_task(second));
    assert_eq!(vm.task_count(), 2); // One waiting caller and its ready short turn.
    let turn = vm.poll_tasks(10).unwrap();
    assert_eq!(turn.polled, 1);
    assert!(turn.completed.is_empty()); // Internal jobs never escape as user results.
    assert!(turn.ready);
    let receipt = vm.poll_tasks(10).unwrap();
    assert_eq!(receipt.completed.len(), 1);
    assert_eq!(receipt.completed[0].id, first);
    assert_eq!(receipt.completed[0].result.as_ref().unwrap(), &12);
    assert!(!receipt.ready);
    assert_eq!(vm.domain_task_count(owner.read_handle()).unwrap(), 0);
    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
    assert_eq!(vm.task_count(), 0);

    let bad = vm
        .spawn_domain_task(owner.read_handle(), "broken", &[Int32(0)])
        .unwrap();
    let good = vm
        .spawn_domain_task(owner.read_handle(), "query", &[])
        .unwrap();
    let failed = vm.poll_tasks(10).unwrap();
    assert_eq!(failed.completed.len(), 1);
    assert_eq!(failed.completed[0].id, bad);
    assert!(failed.completed[0].result.is_err());
    vm.poll_tasks(10).unwrap();
    let success = vm.poll_tasks(10).unwrap();
    assert_eq!(success.completed[0].id, good);
    assert_eq!(success.completed[0].result.as_ref().unwrap(), &12);
    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);

    for name in ["raced", "faulty"] {
        let task = vm
            .spawn_domain_task(owner.write_handle(), name, &[])
            .unwrap();
        let report = vm.poll_tasks(1).unwrap();
        assert_eq!(report.completed.len(), 1);
        assert_eq!(report.completed[0].id, task);
        assert_eq!(report.completed[0].result.is_ok(), name == "raced");
        assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
        assert_eq!(vm.task_count(), 0);
    }
    let denied = vm
        .spawn_domain_task(owner.read_handle(), "request", &[Int32(50)])
        .unwrap();
    let denied = vm
        .poll_tasks(1)
        .unwrap()
        .completed
        .into_iter()
        .find(|r| r.id == denied)
        .unwrap();
    assert!(denied.result.is_err());
    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
    let task = vm
        .spawn_domain_task(owner.write_handle(), "doubled", &[Int32(3)])
        .unwrap();
    let mut answer = None;
    for _ in 0..10 {
        let report = vm.poll_tasks(1).unwrap();
        for result in report.completed {
            if result.id == task {
                answer = Some(result.result.unwrap());
            }
        }
        if answer.is_some() {
            break;
        }
    }
    assert_eq!(answer, Some(30));
    assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 2);
    vm.spawn_domain_task(owner.write_handle(), "request", &[Int32(90)])
        .unwrap();
    vm.spawn_domain_task(owner.read_handle(), "idle", &[])
        .unwrap();
    vm.poll_tasks(2).unwrap();
    assert!(owner.close());
    assert!(owner.is_closed());
    assert_eq!(vm.task_count(), 0);
    assert!(!vm.poll_tasks(10).unwrap().ready);
    assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
}

#[test]
fn state_domains_source_requests_cancel_without_poisoning_the_room() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        exercise_tasks(&mut vm);
    }
}

#[test]
fn state_domains_source_operations_require_checked_direct_entries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for operation in [
            "fn bad(s: &State) -> Int32 { io.println(&s.count) s.count }",
            "fn bad(s: &State) -> &Int32 { &s.count }",
            "fn bad(s: &State) @suspends -> Int32 { s.count }",
        ] {
            let mut vm = VmEngine::with_mode(mode);
            let source = format!("import joky/domain import joky/io struct State {{ count: Int32 }} {operation} fn run() @suspends -> Int32 {{ domain.read(bad) }}");
            assert!(vm.load_source(&source).is_err(), "{operation}");
        }
    }
}

#[test]
fn state_domains_reentrant_close_waits_for_active_poll_and_stop() {
    use crate::{runtime::cps::CpsFrame, FunctionSignature, Type};
    use std::{cell::RefCell, rc::Rc};
    thread_local! { static OWNER: RefCell<Option<Rc<DomainOwner>>> = const { RefCell::new(None) }; }
    unsafe extern "C" fn close_owner() {
        let owner = OWNER.with(|owner| owner.borrow().as_ref().unwrap().clone());
        assert!(!owner.close());
        assert!(!owner.is_closed());
    }
    unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
        (*frame).host_stop = None;
        assert!(queue_current_domain_call("read", &[]).is_err());
        close_owner();
    }
    unsafe extern "C" fn hold(frame: *mut CpsFrame) -> i32 {
        (*frame).host_stop = Some(stop);
        (*frame).wake = None;
        1
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "close_owner",
                FunctionSignature::new(vec![], Type::Unit),
                close_owner as *const u8,
            )
            .unwrap();
            vm.register_suspending_fn("hold", FunctionSignature::new(vec![], Type::Unit), hold)
                .unwrap();
        }
        vm.load_source(&format!("{SOURCE} extern fn close_owner() extern fn hold() @suspends fn close_in_poll() @suspends -> Int32 {{ close_owner() 1 }} fn close_in_stop() @suspends -> Int32 {{ hold() 1 }}")).unwrap();
        for name in ["close_in_poll", "close_in_stop"] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let owner = Rc::new(
                vm.create_domain("make", &[InstanceValue::Int32(1)])
                    .unwrap(),
            );
            OWNER.with(|slot| {
                slot.replace(Some(owner.clone()));
            });
            let task = vm
                .spawn_domain_task(owner.write_handle(), name, &[])
                .unwrap();
            assert!(vm.poll_tasks(1).unwrap().completed.is_empty());
            if name == "close_in_stop" {
                assert!(vm.cancel_domain_task(task));
            }
            assert!(owner.is_closed());
            assert_eq!(vm.task_count(), 0);
            OWNER.with(|slot| slot.borrow_mut().take());
            drop(owner);
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

#[test]
fn state_domains_executor_limits_reject_before_admission_and_bind_handles() {
    use crate::runtime::executor::SystemClock;
    use std::rc::Rc;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.configure_tasks(Rc::new(SystemClock), 1).unwrap();
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(1)])
            .unwrap();
        assert!(vm.configure_tasks(Rc::new(SystemClock), 10).is_err());
        vm.spawn_domain_task(owner.write_handle(), "request", &[InstanceValue::Int32(4)])
            .unwrap();
        let report = vm.poll_tasks(1).unwrap();
        assert_eq!(report.completed.len(), 1);
        assert!(report.completed[0].result.is_err());
        assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 0);
        assert_eq!(vm.task_count(), 0);
        assert!(owner.close());
        vm.configure_tasks(Rc::new(SystemClock), 10).unwrap();
    }
}

#[test]
fn state_domains_request_cancellation_releases_frame_owned_inputs() {
    cancellation_boundaries(false);
}

#[test]
#[ignore = "exhaustive: cancel domain requests at every interpreter instruction"]
fn exhaustive_state_domains_request_instruction_cancellation() {
    cancellation_boundaries(true);
}

fn cancellation_boundaries(exhaustive: bool) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        vm.load_source(SOURCE).unwrap_or_else(|e| panic!("{e}"));
        for close_owner in [false, true] {
            let mut saw_completion = false;
            for boundary in 0..if exhaustive { 256 } else { 8 } {
                let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
                let owner = vm
                    .create_domain("make", &[InstanceValue::Int32(1)])
                    .unwrap();
                let task = vm
                    .spawn_domain_task(owner.write_handle(), "doubled", &[InstanceValue::Int32(2)])
                    .unwrap();
                for _ in 0..boundary {
                    let report = vm.poll_tasks(1).unwrap();
                    if let Some(result) = report
                        .completed
                        .into_iter()
                        .find(|result| result.id == task)
                    {
                        assert_eq!(result.result.unwrap(), 6);
                        saw_completion = true;
                        break;
                    }
                }
                if !close_owner {
                    vm.cancel_domain_task(task);
                }
                assert!(owner.close());
                assert_eq!(vm.task_count(), 0);
                assert!(!vm.poll_tasks(10).unwrap().ready);
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
                if saw_completion {
                    break;
                }
            }
            if exhaustive {
                assert!(saw_completion, "instruction boundary limit was too small");
            }
        }
    }
}

#[test]
fn state_domains_effect_proof_and_intrinsic_identity_cannot_be_bypassed() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let prefix = r#"
            import joky/domain
            import joky/io
            struct State { count: Int32 }
            fn read(s: &State) -> Int32 { s.count }
            fn helper(n: Int32) { io.println(&n) }
            fn bad(s: &State) -> Int32 { helper(s.count) s.count }
        "#;
        vm.load_source(prefix).unwrap_or_else(|e| panic!("{e}"));
        let error = vm
            .load_source(&format!(
                "{prefix} fn run() @suspends -> Int32 {{ domain.read(bad) }}"
            ))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Domain operation must be a proved"),
            "{error}"
        );
        let error = vm
            .load_source(&format!(
                "{prefix} fn run() @suspends -> Int32 {{ let f = domain.read::<State> f(read) }}"
            ))
            .unwrap_err();
        assert!(
            error.to_string().contains("must be called directly"),
            "{error}"
        );
        assert!(vm
            .load_source(&format!(
                "{prefix} fn run() @suspends -> Int32 {{ let f = read domain.read(f) }}"
            ))
            .is_err());
    }
}

#[test]
fn state_domains_nested_continuations_cannot_inherit_a_supervised_capability() {
    use crate::{runtime::suspension::Continuation, FunctionSignature, Type};
    use std::cell::RefCell;
    thread_local! { static INNER: RefCell<Option<Continuation>> = const { RefCell::new(None) }; }
    unsafe extern "C" fn nested() -> i32 {
        INNER.with(|task| i32::from(task.borrow_mut().as_mut().unwrap().resume().is_err()))
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "nested",
                FunctionSignature::new(vec![], Type::Int32),
                nested as *const u8,
            )
            .unwrap();
        }
        vm.load_source(&format!("{SOURCE} extern fn nested() -> Int32 fn outer() @suspends -> Int32 {{ if nested() != 1 {{ return -1 }} domain.write(add, 1) }}")).unwrap_or_else(|e| panic!("{e}"));
        for foreign in [false, true] {
            let mut other = VmEngine::with_mode(mode);
            other.load_source(SOURCE).unwrap();
            let nested_vm = if foreign { &other } else { &vm };
            INNER.with(|slot| {
                slot.replace(Some(
                    nested_vm
                        .prepare_async("request", &[InstanceValue::Int32(100)])
                        .unwrap(),
                ));
            });
            let owner = vm
                .create_domain("make", &[InstanceValue::Int32(10)])
                .unwrap();
            let task = vm
                .spawn_domain_task(owner.write_handle(), "outer", &[])
                .unwrap();
            let mut result = None;
            for _ in 0..4 {
                for completion in vm.poll_tasks(10).unwrap().completed {
                    if completion.id == task {
                        result = Some(completion.result.unwrap());
                    }
                }
            }
            assert_eq!(result, Some(11));
            assert_eq!(vm.domain_revision(owner.read_handle()).unwrap(), 1);
            assert_eq!(vm.task_count(), 0);
            assert!(owner.close());
            INNER.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
}
