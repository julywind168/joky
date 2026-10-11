//! Generated-code coverage for the scalar domain foundation.
use super::*;

pub(crate) const SOURCE: &str = r#"
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
    assert_eq!(vm.poll_domains(0), 0);
    assert_eq!(vm.poll_domains(2), 2);
    value(&mut first, Int32(12));
    value(&mut second, Int32(12));
    assert_eq!(vm.domain_revision(read).unwrap(), 1);
    assert!(third.take_result().is_none());
    assert_eq!(vm.poll_domains(1), 1);
    value(&mut third, Int32(15));
    let mut scaled = vm.queue_domain_call(read, "scale", &[]).unwrap();
    let mut failed = vm
        .queue_domain_call(read, "fail_read", &[Int32(0)])
        .unwrap();
    let mut business = vm.queue_domain_call(write, "rejected", &[]).unwrap();
    assert_eq!(vm.poll_domains(10), 3);
    value(&mut scaled, Float64(0.5));
    assert!(matches!(
        failed.take_result(),
        Some(DomainOutcome::Failed(_))
    ));
    value(&mut business, Bool(false));
    assert_eq!(vm.domain_revision(read).unwrap(), 3);
    let mut peek = vm.queue_domain_call(read, "read", &[]).unwrap();
    vm.poll_domains(1);
    value(&mut peek, Int32(17));

    let mut cancelled = vm.queue_domain_call(write, "add", &[Int32(100)]).unwrap();
    cancelled.cancel();
    assert!(matches!(
        cancelled.take_result(),
        Some(DomainOutcome::Cancelled)
    ));
    drop(vm.queue_domain_call(write, "add", &[Int32(100)]).unwrap());
    assert_eq!(vm.poll_domains(10), 0);
    assert_eq!(vm.domain_revision(read).unwrap(), 3);

    vm.set_domain_limits(DomainLimits {
        max_requests: 1,
        ..Default::default()
    })
    .unwrap();
    let mut held = vm.queue_domain_call(read, "read", &[]).unwrap();
    assert!(vm.queue_domain_call(read, "read", &[]).is_err());
    vm.poll_domains(1);
    held.cancel(); // Completion wins over late cancellation.
    assert!(vm.queue_domain_call(read, "read", &[]).is_err());
    value(&mut held, Int32(17));
    let mut closed = vm.queue_domain_call(write, "add", &[Int32(1)]).unwrap();
    drop(owner);
    assert_eq!(vm.poll_domains(10), 0);
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
    assert_eq!(vm.poll_domains(10), 2);
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
        vm.load_source(SOURCE).unwrap();
        let owner = vm
            .create_domain("make", &[InstanceValue::Int32(5)])
            .unwrap();
        vm.load_source(&SOURCE.replace("state.count }", "state.count + 100 }"))
            .unwrap();
        let mut old = vm
            .queue_domain_call(owner.read_handle(), "read", &[])
            .unwrap();
        vm.poll_domains(1);
        value(&mut old, InstanceValue::Int32(5));
        let fresh = vm
            .create_domain("make", &[InstanceValue::Int32(5)])
            .unwrap();
        let mut new = vm
            .queue_domain_call(fresh.read_handle(), "read", &[])
            .unwrap();
        vm.poll_domains(1);
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
