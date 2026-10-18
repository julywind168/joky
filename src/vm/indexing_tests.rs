//! Ownership, failure, and cancellation coverage for indexed replacement.
use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
use std::cell::RefCell;

const SOURCE: &str = include_str!("../../tests/fixtures/indexing_cleanup.jk");
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
fn indexing_cleanup_and_budget_failures() {
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for name in ["normal", "nested", "early"] {
            reset();
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?} {name}");
            cleaned();
        }
        for name in ["failed_rhs", "failed_index", "failed_drop"] {
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
    super::support::assert_cancellation(
        &mut vm,
        "task",
        20000,
        sweep,
        reset,
        || EVENTS.with(|events| events.borrow().clone()),
        cleaned,
    );
}
#[test]
fn indexing_cancellation_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel indexed replacement at every instruction"]
fn exhaustive_indexing_cancellation() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn indexing_rejects_invalid_types_moves_and_borrows() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    let prefix = "import joky/array.{type Array} import joky/fixed_array.{type FixedArray}\n";
    for (source, code) in [
        ("fn bad() { let a = [1]; a[0] = 2 }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [1]; let r = &mut a[0] }", ErrorCode::InvalidAssignment),
        ("fn bad(a: &Array<Int32>) { a[0] = 2 }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [1]; let i = 0; a[i]; }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; a[-1]; }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; a[true]; }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1]; a[0] = true; }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = 1; a[0]; }", ErrorCode::TypeMismatch),
        ("fn bad() { var a = [1]; a[0] = true; }", ErrorCode::TypeMismatch),
        ("fn bad() { var a = [1]; a[0] = { a.push(2); 3 }; }", ErrorCode::InvalidAssignment),
        ("fn bad() { var a = [1]; a[0] = { a[0] = 2; 3 }; }", ErrorCode::InvalidAssignment),
        ("fn bad() { let a = [\"owned\"]; let moved = a[0] }", ErrorCode::InvalidAssignment),
        ("fn bad() { var a = [\"owned\"]; a[0] = a[0] }", ErrorCode::InvalidAssignment),
        ("fn bad() -> &Int32 { let a = [1]; &a[0] }", ErrorCode::InvalidAssignment),
        ("fn bad() -> &Int32 { &[1][0] }", ErrorCode::InvalidAssignment),
        ("fn bad() { var a = [1,2]; let x = &mut a[0]; let y = &mut a[1]; *x + *y; }", ErrorCode::InvalidAssignment),
        ("fn bad() { var a = [1]; let r = &a[0]; a.push(2); *r; }", ErrorCode::InvalidAssignment),
        ("fn bad() { var a = [1]; let r = &a[0]; a[0] = 2; *r; }", ErrorCode::InvalidAssignment),
        ("fn bad() { let value = &*(alloc(1)); let a = [value]; let r = &a[0] }", ErrorCode::TypeMismatch),
        ("fn bad() { let value = &*(alloc(1)); var a = [value]; let r = &mut a[0] }", ErrorCode::TypeMismatch),
        ("fn pause() @suspends {} fn bad() @suspends -> Int32 { let a = [1]; let r = &a[0]; pause(); *r }", ErrorCode::InvalidCall),
        ("fn pause() @suspends -> Int32 { 42 } fn bad() @suspends { var a = [1]; a[0] = pause() }", ErrorCode::InvalidCall),
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
fn indexing_requires_one_index_and_no_implicit_line_continuation() {
    for source in [
        "fn bad() { [1][] }",
        "fn bad() { [1][0, 1] }",
        "fn bad() { [1][..] }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "{source}");
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source("fn answer() -> Int32 { let a = [42]\n[1]\na[0] }")
            .unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
    }
}
