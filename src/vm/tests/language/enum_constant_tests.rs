//! Enum constants retain tags, inactive lanes and independent active owners.
use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, ModuleSources};

const SOURCE: &str = include_str!("../../../../tests/fixtures/enum_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/enum_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("enum_constants_values", VALUES).unwrap();
    modules
}

fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source_with_modules(SOURCE, &modules())
        .unwrap_or_else(|error| panic!("{mode:?}: {error} at {:?}", error.span()));
    vm
}

fn clean() {
    arena::LIVE.with(|live| assert_eq!(live.get(), 0));
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn enum_constants_allocate_only_active_fields_and_clean_partial_construction() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        arena::LIMIT.with(|limit| limit.set(0));
        let result = vm.call_int32_0("inactive");
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        assert_eq!(
            result.unwrap_or_else(|error| panic!("{mode:?}: {error}")),
            42
        );
        clean();
        let mut failed = false;
        let mut completed = false;
        for budget in (0..65536).step_by(64) {
            arena::LIMIT.with(|limit| limit.set(budget));
            let result = vm.call_int32_0("check");
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            match result {
                Ok(value) => {
                    assert_eq!(value, 42, "{mode:?}");
                    completed = true;
                }
                Err(error) => {
                    assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
                    failed = true;
                }
            }
            clean();
            if completed {
                break;
            }
        }
        assert!(failed && completed, "{mode:?}");
    }
}

#[test]
fn enum_constants_pending_values_survive_reload_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = configured(mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            arena::LIVE.with(|live| assert!(live.get() > 0));
            let error = vm
                .load_source("const BAD: Option<String> = Some(42)")
                .unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            vm.load_source("const NEW: Option<Int32> = None fn replacement() -> Int32 { 0 }")
                .unwrap_or_else(|error| panic!("{error}"));
            drop(vm);
            if finish {
                while !task.is_finished() {
                    task.resume().unwrap();
                }
                assert_eq!(task.cps_result(), Some(42));
            }
            drop(task);
            clean();
        }
    }
}

#[test]
fn enum_constants_cancel_during_active_payload_materialization() {
    let mut vm = configured(VmMode::Interpreter);
    super::support::assert_cancellation(
        &mut vm,
        "construction",
        10000,
        CancellationSweep::ResourceBoundaries,
        || {},
        || arena::LIVE.with(|live| live.get()),
        clean,
    );
}

#[test]
fn enum_constants_reject_invalid_constructors_payloads_and_ownership() {
    for (source, code, message) in [
        ("const BAD: Option<String> = Some(42)", ErrorCode::TypeMismatch, "expects String"),
        ("const BAD: Option<String> = Some", ErrorCode::InvalidCall, "exactly one payload"),
        ("const BAD: Option<String> = None()", ErrorCode::InvalidCall, "no parentheses"),
        ("const BAD: Option<String> = Some(\"a\", \"b\")", ErrorCode::InvalidCall, "exactly one payload"),
        ("enum E { A } const BAD: E = E.Missing", ErrorCode::UnknownName, "has no variant"),
        ("enum E { A } enum F { A } const BAD: E = A", ErrorCode::UnknownName, "Ambiguous"),
        ("enum E { A } type Alias = E const BAD: E = Alias.A", ErrorCode::UnknownName, "Unknown"),
        ("enum E { A } enum F { B } const BAD: E = F.B", ErrorCode::TypeMismatch, "mismatch"),
        ("enum E { Empty, Text(String) } const BAD: Bool = E.Empty == E.Empty", ErrorCode::TypeMismatch, "does not implement"),
        ("fn text() -> String { \"x\" } const BAD: Option<String> = Some(text())", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: Option<String> = Some(\"{42}\")", ErrorCode::InvalidState, "Constants allow"),
        ("fn Some(text: String) -> Option<String> { Option.Some(text) } const BAD: Option<String> = Some(\"x\")", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: Option<Int32> = if true { Some(1) } else { None }", ErrorCode::InvalidState, "Constants allow"),
        ("const A: Option<String> = Some(B) const B: String = A", ErrorCode::InvalidState, "Cyclic"),
        ("enum E<T> { Empty } impl<T> E<T> { const BAD: E<T> = Self.Empty }", ErrorCode::TypeMismatch, "constants require"),
        ("enum E { Empty, Borrowed(&Int32) } const BAD: E = E.Empty", ErrorCode::TypeMismatch, "constants require"),
        ("enum E { Empty, Pointer(Ptr) } const BAD: E = E.Empty", ErrorCode::TypeMismatch, "constants require"),
        ("import joky/set.{type Set} const BAD: Option<Set<Int32>> = None", ErrorCode::TypeMismatch, "constants require"),
        ("const VALUE: Option<String> = Some(\"a\") fn bad() { let value = VALUE; let moved = value; let again = value; }", ErrorCode::InvalidAssignment, "moved"),
        ("const VALUE: Option<String> = Some(\"a\") fn bad() -> &String { match VALUE { Some(text) => &text, None => panic(\"missing\") } }", ErrorCode::InvalidAssignment, "borrowed input"),
    ] {
        let error = super::support::frontend_error(source, &ModuleSources::new());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

#[test]
fn enum_constants_bound_inactive_payload_layouts_and_nesting() {
    // All variants count toward the stored layout, even when Empty is selected.
    let mut source = "enum E0 { Empty }\n".to_owned();
    for level in 1..=11 {
        source += &format!(
            "enum E{level} {{ Empty, Left(E{}), Right(E{}) }}\n",
            level - 1,
            level - 1
        );
    }
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&format!(
        "{source} const VALUE: E11 = E11.Empty fn check() -> Int32 {{ 42 }}"
    ))
    .unwrap_or_else(|error| panic!("4095 components must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    source += "enum E12 { Empty, Left(E11), Right(E11) } const BAD: E12 = E12.Empty";
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("4096 components"), "{error}");

    let mut source = "enum E0 { Empty }\n".to_owned();
    for level in 1..=33 {
        source += &format!("enum E{level} {{ Empty, Next(E{}) }}\n", level - 1);
    }
    source += "const BAD: E33 = E33.Empty";
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("32 levels"), "{error}");
}
