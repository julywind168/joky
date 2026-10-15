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
fn request(delta: Int32) @suspends -> Int32 { domain.write(add, delta).unwrap() }
fn query() @suspends -> Int32 { domain.read(read).unwrap() }
fn broken(zero: Int32) @suspends -> Int32 { 1 / zero }
fn idle() @suspends -> Int32 { task.sleep_ms(10000) 1 }
fn request_unit() @suspends { request(1000); }
fn ready_unit() @suspends {}
fn broken_unit() @suspends { broken(0); }
fn raced() @suspends -> Int32 { race { | request_unit() | ready_unit() } 0 }
fn faulty() @suspends -> Int32 { parallel { | request_unit() | broken_unit() } 0 }
fn doubled(delta: Int32) @suspends -> Int32 {
    let first = domain.write(add, delta).unwrap()
    task.yield_now()
    first + domain.read(read).unwrap()
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
fn state_domains_reject_generic_and_host_entries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            import joky/io
            struct State { count: Int32 }
            struct Owning { value: (String, String) }
            struct Nested { value: State }
            struct Generic<T> { value: T }
            fn make() -> State { State { count: 1 } }
            fn owning() -> Owning { Owning { value: ("bad", "nested") } }
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
        for name in ["generic", "host_make"] {
            assert!(vm.create_domain(name, &[]).is_err(), "{name}");
        }
        let owner = vm.create_domain("make", &[]).unwrap();
        assert!(vm
            .queue_domain_call(owner.read_handle(), "host", &[])
            .is_err());
        assert!(vm
            .queue_domain_call(owner.read_handle(), "indirect", &[InstanceValue::Int32(2)])
            .is_err());
        for name in ["owning", "nested"] {
            vm.create_domain(name, &[]).unwrap().close();
        }
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
            let source = format!("import joky/domain import joky/io struct State {{ count: Int32 }} {operation} fn run() @suspends -> Int32 {{ domain.read(bad).unwrap() }}");
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
                "{prefix} fn run() @suspends -> Int32 {{ domain.read(bad).unwrap() }}"
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
                "{prefix} fn run() @suspends -> Int32 {{ let f = read domain.read(f).unwrap() }}"
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
        vm.load_source(&format!("{SOURCE} extern fn nested() -> Int32 fn outer() @suspends -> Int32 {{ if nested() != 1 {{ return -1 }} domain.write(add, 1).unwrap() }}")).unwrap_or_else(|e| panic!("{e}"));
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

#[test]
fn state_domains_source_owners_handles_and_requests_share_one_executor() {
    let source = format!(
        "{SOURCE}\n{}",
        r#"
        fn source_main() @suspends -> Int32 {
            var owner = domain.create(make, 10)
            let handle = owner.handle()
            let reader = handle.read_only()
            let first = domain.write_to(handle, add, 2).unwrap()
            let second = domain.request(handle, doubled, 3).unwrap_or(-100)
            let failed = domain.request(handle, broken, 0).unwrap_or(-1)
            let last = domain.read_from(reader, read).unwrap()
            owner.close()
            first + second + failed + last
        }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let before = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let task = vm.prepare_async("source_main", &[]).unwrap();
        assert_eq!(
            crate::runtime::driver::block_on(task).unwrap_or_else(|e| panic!("{mode:?}: {e}")),
            56
        );
        drop(vm);
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
    }
}

fn source_cancellation_boundaries(
    source: &str,
    boundaries: impl Clone + IntoIterator<Item = usize>,
    exhaustive: bool,
) {
    use crate::runtime::executor::{HostExecutor, TaskGroup};
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap_or_else(|e| panic!("{e}"));
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut completed = false;
        for boundary in boundaries.clone() {
            let executor = HostExecutor::default();
            let group = TaskGroup::new();
            let id = executor
                .spawn(&group, vm.prepare_async("lifecycle", &[]).unwrap())
                .unwrap();
            let mut done = false;
            for _ in 0..boundary {
                let report = executor.poll(1).unwrap();
                if let Some(result) = report.completed.into_iter().find(|r| r.id == id) {
                    assert_eq!(result.result.unwrap(), 24);
                    done = true;
                    break;
                }
            }
            executor.cancel_group(&group);
            assert_eq!(executor.task_count(), 0, "{mode:?}, boundary {boundary}");
            assert_eq!(
                executor.domains().borrow().usage(),
                (0, 0, 0, 0),
                "{mode:?}, boundary {boundary}"
            );
            assert_eq!(
                crate::runtime::arena::LIVE.with(|n| n.get()),
                baseline,
                "{mode:?}, boundary {boundary}"
            );
            if done {
                completed = true;
                if exhaustive {
                    break;
                }
            }
        }
        assert!(
            completed,
            "{mode:?}: cancellation range must reach completion"
        );
    }
}
#[test]
fn state_domains_source_cancellation_releases_owners_requests_and_receipts() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_source.jk"),
        [0, 1, 2, 3, 4, 5, 10, 20, 50, 100, 200, 400, 800, 1600],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel source ownership handoff and requests at every poll/instruction"]
fn exhaustive_state_domains_source_owner_and_request_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_source.jk"),
        0..=2048,
        true,
    );
}
#[test]
fn state_domains_source_limits_and_code_versions_stay_bound_to_the_executor() {
    use crate::runtime::executor::{SystemClock, TaskGroup};
    use std::rc::Rc;
    let source = format!(
        "{SOURCE}\n{}",
        r#"
        fn limited_request(n:Int32) @suspends -> Int32 {
            match domain.write(add,n) { Err(domain.DomainAccessError.ResourceLimit) => -1, _ => -100 }
        }
        fn source_limit() @suspends -> Int32 {
            let owner = domain.create(make, 10)
            let handle = owner.handle()
            let failed = domain.request(handle, limited_request, 100).unwrap_or(-100)
            failed + domain.read_from(handle.read_only(), read).unwrap()
        }
        fn task_limit() @suspends -> Int32 {
            let owner = domain.create(make, 10)
            match domain.request(owner.handle(), request, 100) { Err(domain.RequestError.ResourceLimit) => 9, _ => -100 }
        }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        vm.set_domain_limits(DomainLimits {
            max_requests: 1,
            ..Default::default()
        })
        .unwrap();
        let executor = vm.executor.clone();
        let id = executor
            .spawn(
                &TaskGroup::new(),
                vm.prepare_async("source_limit", &[]).unwrap(),
            )
            .unwrap();
        executor.poll(1).unwrap();
        vm.load_source("fn replacement() -> Int32 { 77 }").unwrap();
        let mut result = None;
        for _ in 0..128 {
            for done in executor.poll(8).unwrap().completed {
                if done.id == id {
                    result = Some(done.result.unwrap())
                }
            }
            if result.is_some() {
                break;
            }
        }
        assert_eq!(result, Some(9));
        assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
        drop(executor);
        vm.configure_tasks(Rc::new(SystemClock), 1).unwrap();
        assert_eq!(vm.domains.borrow().limits().max_requests, 1);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let executor = vm.executor.clone();
        executor
            .spawn(
                &TaskGroup::new(),
                vm.prepare_async("task_limit", &[]).unwrap(),
            )
            .unwrap();
        let mut result = None;
        for _ in 0..16 {
            for done in executor.poll(8).unwrap().completed {
                result = Some(done.result.unwrap())
            }
            if result.is_some() {
                break;
            }
        }
        assert_eq!(result, Some(9));
        assert_eq!(executor.domains().borrow().usage(), (0, 0, 0, 0));
        drop(executor);
        drop(vm);
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
    }
}
#[test]
fn state_domains_source_capabilities_reject_forgery_and_ownership_errors() {
    let cases=[
        "fn bad() @suspends { let o=domain.create(make,1) let copy=o o.close() copy.close() }",
        "fn bad() @suspends { let o=domain.create(make,1) domain.write_to(o.read_handle(),add,1).unwrap(); }",
        "fn bad() @suspends { let o=domain.create(make,1) let token=o.handle().token }",
        "fn bad() @suspends { let f=make let o=domain.create(f,1) }",
        "fn owned(value: String) @suspends -> String { value } fn bad() @suspends { let o=domain.create(make,1) domain.request(o.handle(),owned,\"text\"); }",
        "fn pair(value: Int32) @suspends -> (Int32, Int32) { (value, value) } fn bad() @suspends { let o=domain.create(make,1) domain.request(o.handle(),pair,1); }",
        "fn data(state: &State) -> String { \"text\" } fn bad() @suspends { let o=domain.create(make,1) domain.read_from(o.read_handle(),data).unwrap(); }",
        "extern fn __domain_create(module: UInt64, name: String, input: Int32) @suspends -> domain.StateOwner fn bad() {}",
    ];
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for case in cases {
            let mut vm = VmEngine::with_mode(mode);
            assert!(
                vm.load_source(&format!("{SOURCE}\n{case}")).is_err(),
                "{mode:?} accepted {case}"
            );
        }
    }
}

#[test]
fn state_domains_scalar_reply_cancellation_reclaims_resources() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_scalars.jk"),
        [0, 1, 2, 3, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel scalar replies, Unit inputs and owners at every instruction"]
fn exhaustive_state_domains_scalar_reply_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_scalars.jk"),
        0..=4096,
        true,
    );
}

#[test]
fn state_domains_snapshot_cancellation_reclaims_captured_versions() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_snapshots.jk"),
        [0, 1, 2, 3, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel snapshot queue, publication and owning handoff at every instruction"]
fn exhaustive_state_domains_snapshot_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_snapshots.jk"),
        0..=8192,
        true,
    );
}

#[test]
fn state_domains_cancelled_source_request_publishes_after_cleanup() {
    use crate::runtime::executor::TaskGroup;
    let source = format!(
        "{SOURCE}\n{}",
        r#"
        fn delayed_input(input: UInt64) @suspends -> UInt64 { task.sleep_ms(10000) input }
        fn controller() @suspends -> Int32 {
            let owner = domain.create(make, 1)
            match domain.request(owner.handle(), delayed_input, 18446744073709551615) {
                Err(domain.RequestError.Cancelled) => 2
                _ => 99
            }
        }
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
        let before = crate::runtime::arena::LIVE.with(|n| n.get());
        let executor = vm.executor.clone();
        let id = executor
            .spawn(
                &TaskGroup::new(),
                vm.prepare_async("controller", &[]).unwrap(),
            )
            .unwrap();
        let mut request = None;
        for _ in 0..16 {
            executor.poll(1).unwrap();
            request = vm.domains.borrow().request_tasks().first().copied();
            if request.is_some() {
                break;
            }
        }
        assert!(vm.cancel_domain_task(request.expect("request admitted")));
        let mut result = None;
        for _ in 0..16 {
            for completed in executor.poll(1).unwrap().completed {
                if completed.id == id {
                    result = Some(completed.result.unwrap());
                }
            }
            if result.is_some() {
                break;
            }
        }
        assert_eq!(result, Some(2));
        assert_eq!(executor.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), before);
    }
}

#[test]
fn state_domains_sc1_repeated_source_lifecycles_restore_resource_baselines() {
    use crate::runtime::executor::TaskGroup;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../tests/fixtures/state_domain_acceptance.jk"
        ))
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        if mode == VmMode::Interpreter {
            vm.set_interpreter_limits(InterpreterLimits {
                poll_budget: Some(1),
                ..Default::default()
            })
            .unwrap();
        }
        let executor = vm.executor.clone();
        for _ in 0..8 {
            let id = executor
                .spawn(
                    &TaskGroup::new(),
                    vm.prepare_async("scenario", &[]).unwrap(),
                )
                .unwrap();
            let mut result = None;
            for step in 0..10000 {
                let report = executor.poll(1).unwrap();
                assert!(
                    report.polled > 0,
                    "{mode:?}: stalled at {step}, tasks {}, usage {:?}",
                    executor.task_count(),
                    vm.domains.borrow().usage()
                );
                for done in report.completed {
                    assert_eq!(done.id, id);
                    result = Some(done.result.unwrap_or_else(|e| panic!("{mode:?}: {e}")));
                }
                if result.is_some() {
                    break;
                }
            }
            assert_eq!(result, Some(42), "{mode:?}");
            assert_eq!(executor.task_count(), 0);
            assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
            assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
        }
    }
}

#[test]
fn state_domains_commit_source_cancellation_reclaims_candidates_and_receipts() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_commits.jk"),
        [
            0, 1, 2, 3, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel candidate construction, commit admission, publication and receipt delivery at every instruction"]
fn exhaustive_state_domains_commit_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_commits.jk"),
        0..=16384,
        true,
    );
}

#[test]
fn state_domains_owned_graph_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_owned.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel owned domain construction, graph publication and receipts at each instruction"]
fn exhaustive_state_domains_owned_graph_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_owned.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_deadline_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_deadlines.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel deadline creation, timed publication and receipts at each instruction"]
fn exhaustive_state_domains_deadline_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_deadlines.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_sc2_repeated_lifecycles_restore_resource_baselines() {
    use crate::runtime::executor::TaskGroup;
    for source in [
        include_str!("../../tests/fixtures/state_domain_sc2_acceptance.jk"),
        include_str!("../../tests/fixtures/state_domain_sc2_arrays.jk"),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
            let mut vm = VmEngine::with_mode(mode);
            vm.load_source(source)
                .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
            if mode == VmMode::Interpreter {
                vm.set_interpreter_limits(InterpreterLimits {
                    poll_budget: Some(1),
                    ..Default::default()
                })
                .unwrap();
            }
            let executor = vm.executor.clone();
            for _ in 0..8 {
                let id = executor
                    .spawn(
                        &TaskGroup::new(),
                        vm.prepare_async("lifecycle", &[]).unwrap(),
                    )
                    .unwrap();
                let mut result = None;
                for _ in 0..100000 {
                    let report = executor.poll(1).unwrap();
                    assert!(report.polled > 0, "{mode:?}: lifecycle stalled");
                    for done in report.completed {
                        assert_eq!(done.id, id);
                        result = Some(done.result.unwrap_or_else(|e| panic!("{mode:?}: {e}")));
                    }
                    if result.is_some() {
                        break;
                    }
                }
                assert_eq!(result, Some(24));
                assert_eq!(executor.task_count(), 0);
                assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
                assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
            }
        }
    }
}
#[test]
fn state_domains_sc2_acceptance_cancellation_reclaims_storage() {
    for source in [
        include_str!("../../tests/fixtures/state_domain_sc2_acceptance.jk"),
        include_str!("../../tests/fixtures/state_domain_sc2_arrays.jk"),
    ] {
        source_cancellation_boundaries(
            source,
            [
                0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
            ],
            false,
        );
    }
}
#[test]
#[ignore = "exhaustive: cancel the SC2 acceptance lifecycles at each instruction"]
fn exhaustive_state_domains_sc2_acceptance_instruction_cancellation() {
    for source in [
        include_str!("../../tests/fixtures/state_domain_sc2_acceptance.jk"),
        include_str!("../../tests/fixtures/state_domain_sc2_arrays.jk"),
    ] {
        source_cancellation_boundaries(source, 0..=32768, true);
    }
}

#[test]
fn state_domains_pair_cancellation_reclaims_complete_access_sets() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_pairs.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel two-domain operations at every instruction"]
fn exhaustive_state_domains_pair_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_pairs.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_joint_snapshot_cancellation_reclaims_full_credentials() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_joint_snapshots.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel joint snapshots and dependent commits at every instruction"]
fn exhaustive_state_domains_joint_snapshot_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_joint_snapshots.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_pair_commit_cancellation_reclaims_both_candidates() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_pair_commits.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel pair candidate construction and atomic publication at every instruction"]
fn exhaustive_state_domains_pair_commit_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_pair_commits.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_many_cancellation_reclaims_full_access_and_write_sets() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_many.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel dynamic access, snapshot, candidate and publication sets at every instruction"]
fn exhaustive_state_domains_many_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_many.jk"),
        0..=65536,
        true,
    );
}

#[test]
fn admission_cancellation_reclaims_pools_waits_and_permits() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/admission.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel admission creation, queued waits and permit handoff at every instruction"]
fn exhaustive_admission_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/admission.jk"),
        0..=65536,
        true,
    );
}

#[test]
fn state_domains_nested_graph_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_nested.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000, 32000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel nested record, Array and Map candidates at every instruction"]
fn exhaustive_state_domains_nested_graph_instruction_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_nested.jk"),
        0..=131072,
        true,
    );
}

#[test]
fn state_domains_value_packet_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_values.jk"),
        [
            0, 1, 2, 3, 5, 10, 30, 60, 120, 240, 480, 960, 1920, 4096, 8192,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel pure domain packets at every poll/instruction"]
fn exhaustive_state_domains_value_packet_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_values.jk"),
        0..=8192,
        true,
    );
}

#[test]
fn state_domains_queries_cancellation_reclaims_resources() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_queries.jk"),
        [0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel scalar and packet queries at every instruction"]
fn exhaustive_state_domains_queries_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_queries.jk"),
        0..=8192,
        true,
    );
}

#[test]
fn state_domains_create_values_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_create_values.jk"),
        [0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel value constructor packets and owning roots at every instruction"]
fn exhaustive_state_domains_create_values_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_create_values.jk"),
        0..=8192,
        true,
    );
}

#[test]
fn state_domains_create_values_failures_restore_budgets_and_registry() {
    use crate::runtime::{arena, executor::TaskGroup};
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../tests/fixtures/state_domain_create_values.jk"
        ))
        .unwrap();
        let baseline = arena::LIVE.with(|n| n.get());
        // The host InstanceValue API intentionally remains scalar-only.
        assert!(vm
            .create_domain("make", &[InstanceValue::Int32(24)])
            .is_err());
        let mut failures = 0;
        let mut successes = 0;
        for (entry, domain_limit, arena_limit) in (0..=4096)
            .step_by(32)
            .map(|n| ("construct", usize::MAX, n))
            .chain([
                ("construct", 1, arena::MAX_BYTES),
                ("failure", usize::MAX, arena::MAX_BYTES),
            ])
        {
            vm.set_domain_limits(DomainLimits {
                max_state_bytes: domain_limit,
                ..Default::default()
            })
            .unwrap();
            arena::LIMIT.with(|limit| limit.set(arena_limit));
            let group = TaskGroup::new();
            let result = (|| {
                let id = vm.executor.spawn(&group, vm.prepare_async(entry, &[])?)?;
                for _ in 0..1000 {
                    for done in vm.poll_tasks(1)?.completed {
                        if done.id == id {
                            return done.result;
                        }
                    }
                }
                panic!("constructor stalled");
            })();
            match result {
                Ok(value) => {
                    assert_eq!(value, 24);
                    successes += 1;
                }
                Err(error) => {
                    if entry == "construct" {
                        assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                    } else {
                        assert!(error.message().contains("Division by zero"), "{error}");
                    }
                    failures += 1;
                }
            }
            vm.executor.cancel_group(&group);
            assert_eq!(vm.task_count(), 0);
            assert_eq!(
                vm.domains.borrow().usage(),
                (0, 0, 0, 0),
                "{mode:?} {entry} {arena_limit}"
            );
            assert_eq!(
                arena::LIVE.with(|n| n.get()),
                baseline,
                "{mode:?} {entry} {arena_limit}"
            );
        }
        assert!(failures > 0 && successes > 0);
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}

#[test]
fn state_domains_create_values_publish_constructor_only_state_schemas() {
    use crate::runtime::executor::TaskGroup;
    let source = r#"import joky/domain import joky/task
struct Init {n:Int32} struct State {n:Int32}
fn make(p:Init)->State {State{n:p.n}}
fn hold() @suspends {
    let owner=domain.create(make,Init{n:7})
    while true {task.yield_now()}
}"#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let group = TaskGroup::new();
        vm.executor
            .spawn(&group, vm.prepare_async("hold", &[]).unwrap())
            .unwrap();
        for _ in 0..1000 {
            vm.poll_tasks(1).unwrap();
            if vm.domains.borrow().usage().0 == 1 {
                break;
            }
        }
        assert_eq!(vm.domains.borrow().usage().0, 1);
        // Constructor input changes do not change an existing root's schema.
        vm.load_source("struct Init {extra:Int32,n:Int32} struct State {n:Int32} fn make(p:Init)->State{State{n:p.n+1}}") .unwrap();
        assert_eq!(vm.publish_domain_code().unwrap(), 1);
        vm.load_source(
            "struct Init {n:Int64} struct State {n:Int64} fn make(p:Init)->State{State{n:p.n}}",
        )
        .unwrap();
        assert!(vm.publish_domain_code().is_err());
        vm.executor.cancel_group(&group);
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
    }
}

#[test]
fn state_domains_candidate_values_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_candidate_values.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel candidate value packets, roots and receipts at every instruction"]
fn exhaustive_state_domains_candidate_values_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_candidate_values.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_candidate_values_restore_task_and_state_budgets() {
    source_value_budget_boundaries(include_str!(
        "../../tests/fixtures/state_domain_candidate_values.jk"
    ));
}
#[test]
fn state_domains_snapshot_values_restore_task_and_state_budgets() {
    source_value_budget_boundaries(include_str!(
        "../../tests/fixtures/state_domain_snapshot_values.jk"
    ));
}
fn source_value_budget_boundaries(source: &str) {
    use crate::runtime::{arena, executor::TaskGroup};
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|limit| limit.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        let baseline = arena::LIVE.with(|n| n.get());
        for entry in ["construct_single", "construct_pair", "construct_set"] {
            let mut failures = 0;
            let mut successes = 0;
            for (task_limit, state_limit) in (0..=8192)
                .step_by(128)
                .map(|n| (n, usize::MAX))
                .chain((1..=8193).step_by(128).map(|n| (arena::MAX_BYTES, n)))
            {
                vm.set_domain_limits(DomainLimits {
                    max_state_bytes: state_limit,
                    ..Default::default()
                })
                .unwrap();
                arena::LIMIT.with(|limit| limit.set(task_limit));
                let group = TaskGroup::new();
                let result = (|| {
                    let id = vm.executor.spawn(&group, vm.prepare_async(entry, &[])?)?;
                    for _ in 0..1000 {
                        for done in vm.poll_tasks(1)?.completed {
                            if done.id == id {
                                return done.result;
                            }
                        }
                    }
                    panic!("domain operation stalled");
                })();
                match result {
                    Ok(value) => {
                        assert_eq!(value, 24);
                        successes += 1;
                    }
                    Err(error) => {
                        assert_eq!(
                            error.code(),
                            crate::ErrorCode::ResourceLimit,
                            "{mode:?} {entry}: {error}"
                        );
                        failures += 1;
                    }
                }
                vm.executor.cancel_group(&group);
                assert_eq!(vm.task_count(), 0);
                assert_eq!(
                    vm.domains.borrow().usage(),
                    (0, 0, 0, 0),
                    "{mode:?} {entry} {task_limit} {state_limit}"
                );
                assert_eq!(
                    arena::LIVE.with(|n| n.get()),
                    baseline,
                    "{mode:?} {entry} {task_limit} {state_limit}"
                );
            }
            assert!(failures > 0 && successes > 0, "{mode:?} {entry}");
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        }
    }
}

#[test]
fn state_domains_snapshot_values_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_snapshot_values.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000, 16000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel snapshot result packets and credentials at every instruction"]
fn exhaustive_state_domains_snapshot_values_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_snapshot_values.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_request_values_cancellation_reclaims_storage() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_request_values.jk"),
        [
            0, 1, 2, 5, 10, 30, 60, 100, 200, 500, 1000, 2000, 4000, 8000,
        ],
        false,
    );
}
#[test]
#[ignore = "exhaustive: cancel record request input, child task and result delivery at every instruction"]
fn exhaustive_state_domains_request_values_cancellation() {
    source_cancellation_boundaries(
        include_str!("../../tests/fixtures/state_domain_request_values.jk"),
        0..=32768,
        true,
    );
}

#[test]
fn state_domains_request_values_cancel_after_admission() {
    use crate::runtime::executor::TaskGroup;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../tests/fixtures/state_domain_request_values.jk"
        ))
        .unwrap();
        let baseline = crate::runtime::arena::LIVE.with(|n| n.get());
        let id = vm
            .executor
            .spawn(
                &TaskGroup::new(),
                vm.prepare_async("cancelled", &[]).unwrap(),
            )
            .unwrap();
        let mut child = None;
        for _ in 0..100 {
            vm.poll_tasks(1).unwrap();
            child = vm.domains.borrow().request_tasks().first().copied();
            if child.is_some() {
                break;
            }
        }
        assert!(vm.cancel_domain_task(child.expect("request admitted")));
        let mut result = None;
        for _ in 0..100 {
            for done in vm.poll_tasks(1).unwrap().completed {
                if done.id == id {
                    result = Some(done.result.unwrap());
                }
            }
            if result.is_some() {
                break;
            }
        }
        assert_eq!(result, Some(2));
        assert_eq!(vm.task_count(), 0);
        assert_eq!(vm.domains.borrow().usage(), (0, 0, 0, 0));
        assert_eq!(crate::runtime::arena::LIVE.with(|n| n.get()), baseline);
    }
}

#[test]
fn state_domains_request_values_refund_budget_failures() {
    use crate::runtime::{arena, executor::TaskGroup};
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            arena::LIMIT.with(|n| n.set(self.0));
        }
    }
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!(
            "../../tests/fixtures/state_domain_request_values.jk"
        ))
        .unwrap();
        let baseline = arena::LIVE.with(|n| n.get());
        let mut successes = 0;
        let mut failures = 0;
        for (task_limit, count_limit, byte_limit) in (0..=8192)
            .step_by(128)
            .map(|n| (n, 4096, 4 * 1024 * 1024))
            .chain((1..=2049).step_by(64).map(|n| (arena::MAX_BYTES, 4096, n)))
        {
            arena::LIMIT.with(|n| n.set(task_limit));
            vm.set_domain_limits(DomainLimits {
                max_requests: count_limit,
                max_request_bytes: byte_limit,
                ..Default::default()
            })
            .unwrap();
            let group = TaskGroup::new();
            let result = (|| {
                let id = vm
                    .executor
                    .spawn(&group, vm.prepare_async("budget", &[])?)?;
                for _ in 0..1000 {
                    for done in vm.poll_tasks(1)?.completed {
                        if done.id == id {
                            return done.result;
                        }
                    }
                }
                panic!("request stalled");
            })();
            match result {
                Ok(24) => successes += 1,
                Ok(4) => failures += 1,
                Err(e) => {
                    assert_eq!(e.code(), crate::ErrorCode::ResourceLimit, "{mode:?}: {e}");
                    failures += 1;
                }
                other => panic!("unexpected request budget result: {other:?}"),
            }
            vm.executor.cancel_group(&group);
            assert_eq!(vm.task_count(), 0);
            assert_eq!(
                vm.domains.borrow().usage(),
                (0, 0, 0, 0),
                "{mode:?} {task_limit} {byte_limit}"
            );
            assert_eq!(arena::LIVE.with(|n| n.get()), baseline);
        }
        assert!(successes > 0 && failures > 0);
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
