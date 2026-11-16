//! Fixed-array constant snapshots, owning elements and partial storage construction.
use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, ModuleSources, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/fixed_array_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/fixed_array_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules
        .insert("fixed_array_constants_values", VALUES)
        .unwrap();
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
fn fixed_array_constants_copy_move_and_clean_partial_allocations() {
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
fn fixed_array_constants_pending_values_survive_reload_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = configured(mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            arena::LIVE.with(|live| assert!(live.get() > 0));
            let error = vm.load_source("import joky/fixed_array.{type FixedArray} const BAD: FixedArray<Int32, 2> = [1]").unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
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
fn fixed_array_constants_cancel_during_materialization() {
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
fn fixed_array_constants_charge_storage_to_the_reading_task() {
    extern "C" fn exhaust() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let source = r#"
        import joky/fixed_array.{type FixedArray}
        const VALUES: FixedArray<Int32, 2> = [20, 22]
        struct Config {}
        impl Config { const VALUES: FixedArray<String, 2> = ["first", "second"] }
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
fn fixed_array_constants_reject_invalid_elements_lengths_and_initializers() {
    for (body, code, message) in [
        ("const BAD: FixedArray<Int32, 2> = [1]", ErrorCode::TypeMismatch, "expected 2"),
        ("const BAD: FixedArray<Int32, 1> = [1, 2]", ErrorCode::TypeMismatch, "expected 1"),
        ("const BAD: FixedArray<String, 2> = [\"ok\", 42]", ErrorCode::TypeMismatch, "expects String"),
        ("const BAD: FixedArray<UInt8, 1> = [256]", ErrorCode::TypeMismatch, "range"),
        ("struct Holder<const N: UInt64> {} impl<const N: UInt64> Holder<N> { const BAD: FixedArray<Int32, N> = [] }", ErrorCode::TypeMismatch, "constants require"),
        ("struct Holder<T> {} impl<T> Holder<T> { const BAD: FixedArray<T, 0> = [] }", ErrorCode::TypeMismatch, "constants require"),
        ("const BAD: FixedArray<Int32, 2> = fixed_array.repeat::<Int32, 2>(1)", ErrorCode::InvalidState, "Constants allow"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: FixedArray<Int32, 2> = [..BASE, 2]", ErrorCode::InvalidState, "Constants allow"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[0]", ErrorCode::InvalidState, "Constants allow"),
        ("fn value() -> String { \"x\" } const BAD: FixedArray<String, 1> = [value()]", ErrorCode::InvalidState, "Constants allow"),
        ("import joky/array.{type Array} enum E { Empty, Other(Array<Int32>) } struct Holder { unused: FixedArray<Unit, 0>, inner: E } const BAD: Int32 = (Holder { unused: [], inner: E.Empty }.inner, 42).1", ErrorCode::TypeMismatch, "Constant storage"),
        ("const BAD: FixedArray<String, 1> = [\"{42}\"]", ErrorCode::InvalidState, "Constants allow"),
        ("const A: FixedArray<Int32, 1> = B struct Holder {} impl Holder { const VALUE: FixedArray<Int32, 1> = A } const B: FixedArray<Int32, 1> = Holder.VALUE", ErrorCode::InvalidState, "Cyclic"),
        ("const VALUE: FixedArray<String, 0> = [] fn bad() { let value = VALUE; let moved = value; let again = value; }", ErrorCode::InvalidAssignment, "moved"),
        ("const VALUE: FixedArray<Int32, 1> = [1] fn bad() { VALUE[0] = 2 }", ErrorCode::InvalidAssignment, "stable array"),
        ("const VALUE: FixedArray<Int32, 1> = [1] fn bad() -> &Int32 { &VALUE[0] }", ErrorCode::InvalidAssignment, "cannot outlive"),
    ] {
        let source = format!("import joky/fixed_array.{{type FixedArray}}\n{body}");
        let error = super::support::frontend_error(&source, &ModuleSources::new());
        assert_eq!(error.code(), code, "{body}: {error}");
        assert!(error.message().contains(message), "{body}: {error}");
    }
    for body in [
        "const BAD: String = cfg.DEFAULT.secret",
        "const BAD: cfg.Settings = cfg.Settings { names: [\"a\", \"b\"], ..cfg.DEFAULT }",
    ] {
        let error = super::support::frontend_error(
            &format!("import fixed_array_constants_values as cfg {body}"),
            &modules(),
        );
        assert_eq!(error.code(), ErrorCode::UnknownName);
        assert!(error.message().contains("private"), "{error}");
    }
}

#[test]
fn fixed_array_constants_bound_zero_sized_expansion_and_string_storage() {
    let prefix = "import joky/fixed_array.{type FixedArray}\n";
    let mut source = format!("{prefix} type A0 = FixedArray<Unit, 0> const V0: A0 = []\n");
    for level in 1..=11 {
        source += &format!(
            "type A{level} = FixedArray<A{}, 2> const V{level}: A{level} = [V{}, V{}]\n",
            level - 1,
            level - 1,
            level - 1
        );
    }
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&format!("{source} fn check() -> UInt64 {{ V11.len() }}"))
        .unwrap_or_else(|error| panic!("4095 components must be accepted: {error}"));
    assert_eq!(vm.call_uint64_0("check").unwrap(), 2);
    clean();
    source += "type A12 = FixedArray<A11, 2> const BAD: A12 = [V11, V11]";
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("4096 components"), "{error}");

    let text = "é".repeat(256 * 1024);
    let mut modules = ModuleSources::new();
    modules
        .insert(
            "large",
            format!("{prefix} pub const PAIR: FixedArray<String, 2> = [\"{text}\", \"{text}\"]"),
        )
        .unwrap();
    let mut source = format!("{prefix} import large\n");
    for index in 0..14 {
        source += &format!("const C{index}: FixedArray<String, 2> = large.PAIR\n");
    }
    source += "struct Holder {} impl Holder { const PAIR: FixedArray<String, 2> = C0 } fn check() -> Int32 { 42 }";
    vm.load_source_with_modules(&source, &modules)
        .unwrap_or_else(|error| panic!("16 MiB must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    let error = super::support::frontend_error(
        &format!("{source} const EXTRA: FixedArray<String, 2> = Holder.PAIR"),
        &modules,
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("16 MiB in total"), "{error}");
    let source = format!(
        "{prefix} import large const EXTRA: (FixedArray<String, 2>, String) = (large.PAIR, \"x\")"
    );
    let error = super::support::frontend_error(&source, &modules);
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("1 MiB"), "{error}");
}
