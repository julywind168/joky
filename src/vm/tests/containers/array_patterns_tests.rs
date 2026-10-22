//! Behavioral, rejection, and partial-construction coverage for sequence syntax.
use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, Type};
use std::cell::RefCell;

const SOURCE: &str = include_str!("../../../../tests/fixtures/array_patterns_cleanup.jk");
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
fn array_patterns_cleanup_and_budget_failures() {
    let _restore = Restore(arena::LIMIT.with(|n| n.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        for name in ["normal", "alternatives", "early"] {
            reset();
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?} {name}");
            cleaned();
        }
        for name in ["failed_init", "failed_arm", "fixed_failure"] {
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
fn array_patterns_cancellation_boundaries() {
    cancellation(super::support::CancellationSweep::ResourceBoundaries);
}
#[test]
#[ignore = "exhaustive: cancel array construction and consuming patterns at every instruction"]
fn exhaustive_array_patterns_cancellation() {
    cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn array_patterns_reject_invalid_types_coverage_and_borrows() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::default();
    let prefix = "import joky/array.{type Array} import joky/fixed_array.{type FixedArray} import joky/slice as slices.{type Slice}\n";
    for (source, code) in [
        ("fn bad() { let a = [] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: FixedArray<Int32, 2> = [1] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1, true] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: Array<UInt8> = [256] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a: FixedArray<String, 1> = [\"x\"] }", ErrorCode::TypeMismatch),
        ("fn bad() { let a = [1, 2]; let b: FixedArray<Int32, 2> = a }", ErrorCode::TypeMismatch),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [] => 0, [0, ..] => 1 } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [] => 0, [x] => x } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(a: FixedArray<Bool,2>) -> Int32 { match a { [true, true] => 1, [false, ..] => 2 } }", ErrorCode::NonExhaustiveMatch),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [..] => 0, [] => 1 } }", ErrorCode::UnreachableCode),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [x, ..] => x, [x] => x, [] => 0 } }", ErrorCode::UnreachableCode),
        ("fn bad(a: FixedArray<Int32,2>) -> Int32 { match a { [x] => x, _ => 0 } }", ErrorCode::InvalidPattern),
        ("fn bad(a: Array<Int32>) { let [x, y] = a }", ErrorCode::InvalidPattern),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [x, x] => x, _ => 0 } }", ErrorCode::DuplicateDefinition),
        ("fn bad(a: Array<Int32>) -> Int32 { match a { [x, ..x] => x, _ => 0 } }", ErrorCode::DuplicateDefinition),
        ("fn bad() -> Int32 { let a = [1]; match a { [x] => x, _ => 0 }; int32(a.len()) }", ErrorCode::InvalidAssignment),
        ("fn bad() -> Int32 { var a = [1,2]; match &a { [x, ..tail] => { a.push(3); int32(slices.len(tail)) }, _ => 0 } }", ErrorCode::InvalidAssignment),
        ("fn bad() -> &Slice<Int32> { let a = [1,2]; match &a { [..tail] => tail } }", ErrorCode::InvalidAssignment),
        ("fn bad() -> Int32 { var a = [1,2]; match &mut a { [x, y] => { a.push(3); *x + *y }, _ => 0 } }", ErrorCode::InvalidAssignment),
        ("fn bad() -> Int32 { var a = [1,2]; match &mut a { [x, y] => { let z = a.borrow_mut_ref(0); *x + *y + *z }, _ => 0 } }", ErrorCode::InvalidAssignment),
    ] {
        let source = format!("{prefix}{source}");
        let result = crate::parser::parse(&source).map_err(crate::Error::from)
            .and_then(|module| PreparedModule::new(&module, &types, &BindingRegistry::default()))
            .and_then(|prepared| prepared.plan(&Default::default()));
        let error = result.err().unwrap_or_else(|| panic!("accepted {source}"));
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn array_patterns_work_without_prelude_and_keep_imports_hygienic() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source("fn answer() -> Int32 { let array = 7; let values = [20, 22]; match values { [a, b] => a + b, _ => array } }").unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        assert!(vm.load_source("fn bad() -> Array<Int32> { [1] }").is_err());
    }
}

#[test]
fn array_patterns_reject_malformed_sequence_syntax() {
    for source in [
        "fn bad() { [1 2] }",
        "fn bad() { [,] }",
        "fn bad() { [1,,2] }",
        "fn bad() { [1; 2] }",
        "fn bad() { match [1] { [..a, ..b] => {} } }",
        "fn bad() { let [..a, ..b] = [1] }",
    ] {
        assert!(crate::parser::parse(source).is_err(), "accepted: {source}");
    }
}
