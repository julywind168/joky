//! Direct JIT-versus-interpreter parity checks.

use super::support::*;
use super::VmMode;
use crate::{ErrorCode, ErrorPhase};

#[test]
fn jit_and_interpreter_match_sync_results_and_host_side_effects() {
    let source = r#"
        extern fn record(x: Int32) -> Int32
        fn compute(x: Int32) -> Int32 {
            let doubled = x * 2
            record((doubled + 3) * 4)
        }
    "#;
    let mut results = Vec::new();
    let mut effects = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        results.push(vm.call_int32_1("compute", 5).unwrap());
        effects.push(EVENTS.with(|events| events.borrow().clone()));
    }
    assert_eq!(results, [52, 52]);
    assert_eq!(effects, [vec![52], vec![52]]);
}

#[test]
fn jit_and_interpreter_match_cps_scope_results_and_poll_order() {
    let source = r#"
        extern fn delayed_value(x: Int32) @suspends -> Int32
        extern fn record(x: Int32) -> Int32
        fn left(value: Int32) @suspends { delayed_value(value) {} }
        fn right(value: Int32) @suspends { delayed_value(value) {} }
        fn all(x: Int32) @suspends -> Int32 {
            let y = delayed_value(x)
            scope { | left(y) | right(y + 1) }
            record(y * 2)
        }
    "#;
    let mut results = Vec::new();
    let mut effects = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut task = vm.call_async("all", &[4]).unwrap();
        READY.with(|ready| ready.set(true));
        for _ in 0..8 {
            if task.is_finished() {
                break;
            }
            task.resume().unwrap();
        }
        assert!(task.is_finished());
        results.push(task.cps_result().unwrap() as i32);
        effects.push(EVENTS.with(|events| events.borrow().clone()));
    }
    assert_eq!(results, [10, 10]);
    assert_eq!(effects, [vec![4, 5, 6, 10], vec![4, 5, 6, 10]]);
}

#[test]
fn jit_and_interpreter_match_cps_failure_status() {
    let source = "extern fn fail_now() @suspends fn f() @suspends { fail_now() }";
    let mut statuses = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let error = vm.call_async("f", &[]).unwrap_err();
        statuses.push(error);
    }
    // Both backends must fail on the host status before any result exists,
    // with the same structured classification.
    for error in &statuses {
        assert_eq!(error.phase(), ErrorPhase::Runtime);
        assert_eq!(error.code(), ErrorCode::HostError);
        assert!(error.to_string().contains("failure status 7"), "{error}");
    }
    // The interpreter knows the failing binding's name; the native path only
    // sees the raw status through the C entry.
    assert!(
        statuses[1].to_string().contains("`fail_now`"),
        "{}",
        statuses[1]
    );
}

#[test]
fn jit_and_interpreter_match_module_reload_results() {
    let mut results = Vec::new();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = parity_vm("fn value(x: Int32) -> Int32 { x + 1 }", mode);
        let before = vm.call_int32_1("value", 41).unwrap();
        vm.load_source("fn value(x: Int32) -> Int32 { x + 10 }")
            .unwrap();
        let after = vm.call_int32_1("value", 41).unwrap();
        results.push((before, after, vm.module_version()));
    }
    assert_eq!(results[0].0, 42);
    assert_eq!(results[1].0, 42);
    assert_eq!(results[0].1, 51);
    assert_eq!(results[1].1, 51);
    assert_eq!(results[0].2, results[1].2);
}
