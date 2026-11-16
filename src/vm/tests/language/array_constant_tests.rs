//! Array templates are recreated as independent task-owned storage.
use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, ModuleSources, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/array_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/array_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("array_constants_values", VALUES).unwrap();
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
fn array_constants_copy_move_and_clean_partial_allocations() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
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
fn array_constants_pending_values_survive_reload_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = configured(mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            arena::LIVE.with(|live| assert!(live.get() > 0));
            let error = vm.load_source("import joky/array.{type Array} const BASE: Array<Int32> = [1] const BAD: Int32 = BASE[1]").unwrap_err();
            assert_eq!(error.code(), ErrorCode::InvalidState);
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            vm.load_source("fn replacement() -> Int32 { 0 }").unwrap();
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
fn array_constants_cancel_during_materialization() {
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
fn array_constants_charge_storage_to_the_reading_task() {
    extern "C" fn exhaust() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let source = r#"
        import joky/array.{type Array}
        const VALUES: Array<Int32> = [20, 22]
        struct Config {}
        impl Config { const VALUES: Array<String> = ["first", "second"] }
        extern fn exhaust()
        fn module_read() -> Int32 { exhaust(); let values = VALUES; values[0] }
        fn associated_read() -> Int32 { exhaust(); let values = Config.VALUES; int32(values.len()) }
    "#;
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: The callback has the declared synchronous () -> Unit ABI.
        unsafe {
            vm.register_fn(
                "exhaust",
                FunctionSignature::new(vec![], Type::Unit),
                exhaust as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source)
            .unwrap_or_else(|error| panic!("{error}"));
        for entry in ["module_read", "associated_read"] {
            let result = vm.call_int32_0(entry);
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            let error = result.unwrap_err();
            assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
            clean();
        }
    }
}

#[test]
fn array_constants_reject_invalid_elements_calls_moves_and_cycles() {
    for (body, code, message) in [
        ("const BAD: Array<String> = [42]", ErrorCode::TypeMismatch, "expects String"),
        ("const BAD: Array<&Int32> = []", ErrorCode::TypeMismatch, "constants require"),
        ("const BAD: Array<Ptr> = []", ErrorCode::TypeMismatch, "constants require"),
        ("import joky/set.{type Set} const BAD: Array<Set<Int32>> = []", ErrorCode::TypeMismatch, "constants require"),
        ("struct Holder<T> {} impl<T> Holder<T> { const BAD: Array<T> = [] }", ErrorCode::TypeMismatch, "constants require"),
        ("const BAD: Array<Int32> = Array.new::<Int32>()", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: Array<Int32> = array.with_capacity::<Int32>(0)", ErrorCode::InvalidState, "Constants allow"),
        ("fn value() -> Int32 { 42 } const BAD: Array<Int32> = [value()]", ErrorCode::InvalidState, "Constants allow"),
        ("const VALUES: Array<String> = [\"x\"] const BAD: String = VALUES[0]", ErrorCode::InvalidAssignment, "non-Copy"),
        ("const VALUES: Array<String> = [\"x\"] const BAD: Array<String> = [..&VALUES[0..1]]", ErrorCode::InvalidState, "Constants allow"),
        ("const VALUES: Array<Int32> = [] fn bad() { let first = VALUES; let moved = first; let again = first; }", ErrorCode::InvalidAssignment, "moved"),
        ("const VALUES: Array<Int32> = [1] fn bad() { VALUES[0] = 2 }", ErrorCode::InvalidAssignment, "stable array"),
        ("const VALUES: Array<Int32> = [1] fn bad() -> &Int32 { &VALUES[0] }", ErrorCode::InvalidAssignment, "outlive"),
        ("const A: Array<Int32> = [..Holder.B] struct Holder {} impl Holder { const B: Array<Int32> = [..A] }", ErrorCode::InvalidState, "Cyclic"),
        ("const INDEX: UInt64 = VALUES[0] const VALUES: Array<UInt64> = [INDEX]", ErrorCode::InvalidState, "Cyclic"),
    ] {
        let source = format!("import joky/array.{{type Array}} {body}");
        let error = super::support::frontend_error(&source, &ModuleSources::new());
        assert_eq!(error.code(), code, "{body}: {error}");
        assert!(error.message().contains(message), "{body}: {error}");
    }
    for expression in ["cfg.DEFAULT.secret", "[cfg.DEFAULT][0].secret"] {
        let source =
            format!("import array_constants_values as cfg const BAD: String = {expression}");
        let error = super::support::frontend_error(&source, &modules());
        assert_eq!(error.code(), ErrorCode::UnknownName);
        assert!(error.message().contains("private"), "{error}");
    }
    for (ty, literal, index, associated) in [
        ("Int32", "[]", "0", false),
        ("Unit", "[{}]", "1", true),
        ("Int32", "[42]", "18446744073709551615", false),
    ] {
        let expression = format!("VALUES[{index}]");
        let declaration = format!("const BAD: {ty} = {expression}");
        let declaration = if associated {
            format!("struct Holder {{}} impl Holder {{ {declaration} }}")
        } else {
            declaration
        };
        let source = format!(
            "import joky/array.{{type Array}} const VALUES: Array<{ty}> = {literal} {declaration}"
        );
        let mut modules = ModuleSources::new();
        modules.insert("bad", source.as_str()).unwrap();
        let error = super::support::frontend_error("import bad", &modules);
        assert_eq!(error.code(), ErrorCode::InvalidState, "{error}");
        assert_eq!(error.phase(), crate::ErrorPhase::TypeCheck);
        let span = error.span().unwrap();
        assert_eq!(
            span.slice(error.sources().get(span.source_id()).unwrap().text()),
            Some(expression.as_str())
        );
    }
}

#[test]
fn array_constants_bound_actual_elements_and_expansion() {
    let mut source = "import joky/array.{type Array} const V0: Array<Unit> = [{}]\n".to_owned();
    for level in 1..=11 {
        source += &format!(
            "const V{level}: Array<Unit> = [..V{}, ..V{}]\n",
            level - 1,
            level - 1
        );
    }
    let spread = (0..=11)
        .map(|n| format!("..V{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    source += &format!("const BIG: Array<Unit> = [{spread}]\n");
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&format!("{source} fn check() -> UInt64 {{ BIG.len() }}"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(vm.call_uint64_0("check").unwrap(), 4095);
    clean();
    for suffix in [
        "const BAD: Array<Unit> = [..BIG, {}]",
        "const BAD: Array<Array<Unit>> = [BIG]",
    ] {
        let error =
            super::support::frontend_error(&format!("{source} {suffix}"), &ModuleSources::new());
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains("4096 components"), "{error}");
    }
    let repeated = std::iter::repeat_n("BIG[0]", 17)
        .collect::<Vec<_>>()
        .join(", ");
    let error = super::support::frontend_error(
        &format!("{source} const BAD: Unit = ({repeated}).0"),
        &ModuleSources::new(),
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(
        error.message().contains("65536 expanded Array components"),
        "{error}"
    );
    for n in 0..13 {
        source += &format!("const C{n}: Array<Unit> = BIG\n");
    }
    vm.load_source(&source).unwrap_or_else(|e| panic!("{e}"));
    let error = super::support::frontend_error(
        &format!("{source} const EXTRA: Array<Unit> = BIG"),
        &ModuleSources::new(),
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(
        error.message().contains("65536 components in total"),
        "{error}"
    );
    let mut ty = "Int32".to_owned();
    for _ in 0..33 {
        ty = format!("Array<{ty}>");
    }
    let error = super::support::frontend_error(
        &format!("import joky/array.{{type Array}} const BAD: {ty} = []"),
        &ModuleSources::new(),
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
}

#[test]
fn array_constants_count_nested_string_bytes() {
    let text = "é".repeat(256 * 1024);
    let mut modules = ModuleSources::new();
    modules.insert("large", format!("import joky/array.{{type Array}} pub const TEXT: Array<String> = [\"{text}\", \"{text}\"]")).unwrap();
    let prefix = "import joky/array.{type Array} import large ";
    let mut source = prefix.to_owned();
    for n in 0..14 {
        source += &format!("const C{n}: Array<String> = large.TEXT\n");
    }
    source += "struct Holder {} impl Holder { const TEXT: Array<String> = C0 } fn check() -> UInt64 { Holder.TEXT.len() }";
    let mut vm = VmEngine::new_interpreter();
    vm.load_source_with_modules(&source, &modules)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(vm.call_uint64_0("check").unwrap(), 2);
    clean();
    let error = super::support::frontend_error(
        &format!("{source} const EXTRA: Array<String> = Holder.TEXT"),
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("16 MiB in total"), "{error}");
    let error = super::support::frontend_error(
        &format!("{prefix} const BAD: Array<Array<String>> = [large.TEXT, [\"x\"]]"),
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("1 MiB"), "{error}");
}
