//! Ownership, failure, and cancellation coverage for array spreading.
use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
use std::cell::RefCell;

const SOURCE: &str = include_str!("../../tests/fixtures/array_spread_cleanup.jk");
thread_local! { static EVENTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) }; }
extern "C" fn note(value: i32) {
    EVENTS.with(|events| events.borrow_mut().push(value));
}
fn reset() {
    EVENTS.with(|events| events.borrow_mut().clear());
}
fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.set_prelude_enabled(true);
    unsafe {
        vm.register_fn(
            "note",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            note as *const u8,
        )
        .unwrap();
    }
    vm.load_source(SOURCE)
        .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
    vm
}
fn cleaned() {
    arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    EVENTS.with(|events| {
        let events = events.borrow();
        for id in 1..=4 {
            let created = events.iter().filter(|&&n| n == id).count();
            let dropped = events.iter().filter(|&&n| n == -id).count();
            assert!(
                created <= 1 && dropped <= 1 && dropped >= created,
                "owner {id}: {events:?}"
            );
        }
    });
}
struct Restore(usize);
impl Drop for Restore {
    fn drop(&mut self) {
        arena::LIMIT.with(|n| n.set(self.0));
    }
}
#[test]
fn array_spread_cleanup_and_budget_failures() {
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for name in ["normal", "nested", "early"] {
            reset();
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?} {name}");
            cleaned();
        }
        for name in ["failed_source", "failed_after", "failed_drop"] {
            reset();
            assert!(vm.call_int32_0(name).is_err(), "{mode:?} {name}");
            cleaned();
        }
        let mut success = false;
        let mut partial = false;
        for limit in (0..32768).step_by(128) {
            arena::LIMIT.with(|n| n.set(limit));
            reset();
            match vm.call_int32_0("normal") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    success = true;
                }
                Err(e) => {
                    assert_eq!(e.code(), ErrorCode::ResourceLimit, "{mode:?} {limit}: {e}");
                    partial |= EVENTS.with(|events| events.borrow().contains(&1));
                }
            }
            cleaned();
            if success {
                break;
            }
        }
        assert!(success && partial);
        arena::LIMIT.with(|n| n.set(arena::MAX_BYTES));
    }
}
fn cancellation(sweep: super::support::CancellationSweep) {
    let mut vm = configured(VmMode::Interpreter);
    for entry in ["task", "nested_task"] {
        super::support::assert_cancellation(
            &mut vm,
            entry,
            20000,
            sweep,
            reset,
            || EVENTS.with(|events| events.borrow().clone()),
            cleaned,
        );
    }
}
#[test]
fn array_spread_cancellation_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel array spreading at every instruction"]
fn exhaustive_array_spread_cancellation() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn array_spread_rejects_invalid_sources_moves_lengths_and_loans() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    let prefix = "import joky/array.{type Array} import joky/fixed_array.{type FixedArray}\n";
    for (source, code) in [
        ("fn bad() { let a: Array<Int32> = [..(42)] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: Array<Int32> = [..[true]] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; let b = [..a]; a.len(); }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a: Array<Int32> = []; let b = [..a]; a.len(); }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [1]; let b = [..a, ..a] }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [\"owned\"]; let b = [..&a] }", ErrorCode::TypeMismatch),
        ("fn bad() { var a = [\"owned\"]; let b = [..&mut a] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [\"owned\"]; let b = [..a.slice(0, 1)] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; let b: FixedArray<Int32, 1> = [..a] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: FixedArray<Int32, 2> = [1,2]; let b: FixedArray<Int32, 2> = [0, ..a] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: FixedArray<Int32, 2> = [1,2]; let b: FixedArray<Int32, 2> = [..a.slice(0, 2)] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; let r = &a[0]; let b = [..a]; *r; }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [1]; let r = &a[0]; let refs = [r]; let b = [..&refs]; a.release(); *b[0]; }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [1]; let r = &a[0]; let refs = [r]; let b = [..refs]; a.release(); *b[0]; }", ErrorCode::InvalidAssignment),
    ] {
        let source = format!("{prefix}{source}");
        let result = crate::parser::parse(&source).map_err(crate::Error::from)
            .and_then(|module| PreparedModule::new(&module, &types, &BindingRegistry::default()))
            .and_then(|prepared| prepared.plan(&Default::default()));
        let error = result.err().unwrap_or_else(|| panic!("accepted {source}"));
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

#[test]
fn array_spread_syntax_and_hidden_import() {
    for source in [
        "fn bad() { [..] }",
        "fn bad() { [...a] }",
        "fn bad() { [..a ..b] }",
        "fn bad() { ..[1] }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(
            "fn answer() -> Int32 { let a = [20]; let b = [..a, ..[22],]; b[0] + b[1] }",
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
}
