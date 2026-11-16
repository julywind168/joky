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
            let error = vm.load_source("import joky/fixed_array.{type FixedArray} const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[1]").unwrap_err();
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
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: FixedArray<Int32, 3> = [..BASE, 2]", ErrorCode::TypeMismatch, "expected 3"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[1]", ErrorCode::InvalidState, "out of bounds"),
        ("const BASE: FixedArray<String, 1> = [\"x\"] const BAD: String = BASE[0]", ErrorCode::InvalidAssignment, "non-Copy"),
        ("const BASE: FixedArray<String, 0> = [] const BAD: FixedArray<String, 0> = [..BASE]", ErrorCode::TypeMismatch, "Copy elements"),
        ("const BASE: FixedArray<String, 1> = [\"x\"] const BAD: FixedArray<String, 1> = [..BASE]", ErrorCode::TypeMismatch, "Copy elements"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: FixedArray<Int32, 0> = [..BASE]", ErrorCode::TypeMismatch, "expected 0"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: FixedArray<UInt32, 1> = [..BASE]", ErrorCode::TypeMismatch, "Cannot spread"),
        ("const BAD: FixedArray<Int32, 2> = [..[1, 2]]", ErrorCode::TypeMismatch, "statically known spread lengths"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[-1]", ErrorCode::TypeMismatch, "UInt64"),
        ("const I: Int32 = 0 const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[I]", ErrorCode::TypeMismatch, "UInt64"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[true]", ErrorCode::TypeMismatch, "UInt64"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[0 / 0]", ErrorCode::InvalidState, "division"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: FixedArray<Int32, 1> = [..&BASE[0..1]]", ErrorCode::InvalidState, "Constants allow"),
        ("const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[if true { 0 } else { 0 }]", ErrorCode::InvalidState, "Constants allow"),
        ("fn index() -> UInt64 { 0 } const BASE: FixedArray<Int32, 1> = [1] const BAD: Int32 = BASE[index()]", ErrorCode::InvalidState, "Constants allow"),
        ("const VALUES: FixedArray<UInt64, 1> = [INDEX] struct Holder {} impl Holder { const INDEX: UInt64 = VALUES[0] } const INDEX: UInt64 = Holder.INDEX", ErrorCode::InvalidState, "Cyclic"),
        ("const VALUES: FixedArray<Int32, 1> = [..Holder.VALUES] struct Holder {} impl Holder { const VALUES: FixedArray<Int32, 1> = [..VALUES] }", ErrorCode::InvalidState, "Cyclic"),
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
        "import joky/fixed_array.{type FixedArray} const VALUES: FixedArray<cfg.Settings, 1> = [cfg.DEFAULT] const BAD: String = VALUES[0].secret",
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
fn fixed_array_constants_index_errors_preserve_source_locations() {
    for (ty, values, index, length) in [
        ("Int32", "[1, 2]", "2", 2),
        ("Unit", "[{}, {}]", "2", 2),
        ("Unit", "[]", "0", 0),
        ("Int32", "[1, 2]", "18446744073709551615", 2),
    ] {
        for associated in [false, true] {
            let expression = format!("VALUES[{index}]");
            let declaration = format!("const BAD: {ty} = {expression}");
            let declaration = if associated {
                format!("struct Holder {{}} impl Holder {{ {declaration} }}")
            } else {
                declaration
            };
            let source = format!("import joky/fixed_array.{{type FixedArray}} const VALUES: FixedArray<{ty}, {length}> = {values} {declaration}");
            let mut modules = ModuleSources::new();
            modules.insert("invalid", source.as_str()).unwrap();
            let error = super::support::frontend_error("import invalid", &modules);
            assert_eq!(error.code(), ErrorCode::InvalidState, "{error}");
            assert_eq!(error.phase(), crate::ErrorPhase::TypeCheck);
            assert!(error.message().contains("out of bounds"), "{error}");
            let span = error.span().unwrap();
            let file = error.sources().get(span.source_id()).unwrap();
            assert_eq!(span.slice(file.text()), Some(expression.as_str()));
        }
    }
}

#[test]
fn fixed_array_constants_bound_spread_work_and_discarded_storage() {
    let mut source =
        "import joky/fixed_array.{type FixedArray} const V0: FixedArray<Unit, 1> = [{}]\n"
            .to_owned();
    for level in 1..=11 {
        source += &format!(
            "const V{level}: FixedArray<Unit, {}> = [..V{}, ..V{}]\n",
            1 << level,
            level - 1,
            level - 1,
        );
    }
    let spreads = (0..=11)
        .map(|level| format!("..V{level}"))
        .collect::<Vec<_>>()
        .join(", ");
    source += &format!("const BIG: FixedArray<Unit, 4095> = [{spreads}]\n");
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&format!("{source} fn check() -> UInt64 {{ BIG.len() }}"))
        .unwrap_or_else(|error| panic!("4096 components must be accepted: {error}"));
    assert_eq!(vm.call_uint64_0("check").unwrap(), 4095);
    clean();

    let wide = format!("{source} struct Wide {{ values: FixedArray<Unit, 4096> }} const BAD: Unit = Wide {{ values: [..BIG, {{}}] }}.values[0]");
    let error = super::support::frontend_error(&wide, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("4096 components"), "{error}");

    // The scalar result is tiny; evaluation must still bound repeated spread
    // loops, including their logically present but physically empty elements.
    let near = (1..=11)
        .map(|level| format!("..V{level}"))
        .collect::<Vec<_>>()
        .join(", ");
    source += &format!("const NEAR: FixedArray<Unit, 4094> = [{near}]\n");
    let repeated = std::iter::repeat_n("Pack { values: [..NEAR] }.values[0]", 30)
        .collect::<Vec<_>>()
        .join(", ");
    let work = format!("{source} struct Pack {{ values: FixedArray<Unit, 4094> }} const BAD: Unit = ({repeated}).0");
    let error = super::support::frontend_error(&work, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("1048576 instructions"), "{error}");
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
