//! Owning constant templates: independent fields, partial construction and cancellation.
use super::{support::CancellationSweep, VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, ModuleSources, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/owning_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/owning_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("owning_constants_values", VALUES).unwrap();
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
fn owning_constants_materialize_independent_fields_and_clean_partial_allocations() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut failed = false;
        let mut completed = false;
        for budget in (0..32768).step_by(32) {
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
fn owning_constants_pending_values_survive_reload_and_cancel_cleanly() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = configured(mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            arena::LIVE.with(|live| assert!(live.get() > 0));
            let error = vm
                .load_source("const BAD: (String, String) = (\"ok\", 42)")
                .unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
            vm.load_source(
                "const NEW: (String, Int32) = (\"new\", 1) fn replacement() -> Int32 { NEW.1 }",
            )
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
fn owning_constants_cancel_during_partial_materialization() {
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
fn owning_constants_report_budget_errors_at_module_and_associated_reads() {
    extern "C" fn exhaust() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let source = r#"
        import owning_constants_values as cfg
        extern fn exhaust()
        fn module_read() -> Int32 { exhaust(); let value = cfg.DEFAULT; value.validate() }
        fn associated_read() -> Int32 { exhaust(); let value = cfg.Settings.DEFAULT; value.validate() }
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
        vm.load_source_with_modules(source, &modules())
            .unwrap_or_else(|error| panic!("{error}"));
        for (entry, read) in [
            ("module_read", "cfg.DEFAULT"),
            ("associated_read", "cfg.Settings.DEFAULT"),
        ] {
            let error = vm.call_int32_0(entry).unwrap_err();
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            assert_eq!(error.code(), ErrorCode::ResourceLimit, "{error}");
            assert_eq!(
                error.span().and_then(|span| span.slice(source)),
                Some(read),
                "{mode:?}: {error}"
            );
            clean();
        }
    }
}

#[test]
fn owning_constants_keep_move_borrow_privacy_and_initializer_rules() {
    for (source, code, message) in [
        ("const BAD: (String, String) = (\"ok\", 42)", ErrorCode::TypeMismatch, "mismatch"),
        ("struct P { text: String } const BAD: P = P { text: 42 }", ErrorCode::TypeMismatch, "expects String"),
        ("const BAD: (String, Int32) = (\"{42}\", 1)", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: (String, Int32) = (String.new(), 1)", ErrorCode::InvalidState, "Constants allow"),
        ("const BAD: (String, Int32) = (\"a\" + \"b\", 1)", ErrorCode::TypeMismatch, "does not implement"),
        ("const A: (String, Int32) = (P.TEXT, 1) struct P {} impl P { const TEXT: String = A.0 }", ErrorCode::InvalidState, "Cyclic"),
        ("const VALUE: (String, Int32) = (\"a\", 1) fn bad() { let value = VALUE; let moved = value; let again = value; }", ErrorCode::InvalidAssignment, "moved"),
        ("const VALUE: (String, Int32) = (\"a\", 1) fn bad() -> &String { let (text, _) = VALUE; &text }", ErrorCode::InvalidAssignment, "borrowed input"),
        ("import owning_constants_values as cfg const BAD: String = cfg.DEFAULT.secret", ErrorCode::UnknownName, "private"),
        ("import owning_constants_values as cfg const BAD: cfg.Settings = cfg.Settings { title: \"changed\", ..cfg.DEFAULT }", ErrorCode::UnknownName, "private"),
    ] {
        let error = super::support::frontend_error(source, &modules());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

#[test]
fn owning_constants_bound_string_bytes_across_fields_aliases_and_modules() {
    // Each leaf is half the per-value limit in UTF-8 bytes. Repeated fields
    // count independently, as do aliases in either kind of declaration.
    let half = "é".repeat(256 * 1024);
    let source = format!(
        "const HALF: String = \"{half}\"\n\
         const PAIR: (String, String) = (HALF, HALF)\n\
         fn check() -> Int32 {{ 42 }}"
    );
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&source)
        .unwrap_or_else(|error| panic!("1 MiB aggregate must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    let error = super::support::frontend_error(
        &format!("{source}\nstruct P {{}} impl P {{ const EXTRA: ((String, String), String) = (PAIR, \"x\") }}"),
        &ModuleSources::new(),
    );
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("1 MiB"), "{error}");

    let mut modules = ModuleSources::new();
    modules
        .insert(
            "large",
            format!("pub const PAIR: (String, String) = (\"{half}\", \"{half}\")"),
        )
        .unwrap();
    let mut source = "import large\n".to_owned();
    for index in 0..14 {
        source += &format!("const C{index}: (String, String) = large.PAIR\n");
    }
    source += "struct P {} impl P { const PAIR: (String, String) = C0 } fn check() -> Int32 { 42 }";
    vm.load_source_with_modules(&source, &modules)
        .unwrap_or_else(|error| panic!("16 MiB must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    source += "\nconst EXTRA: (String, String) = P.PAIR";
    let error = super::support::frontend_error(&source, &modules);
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("16 MiB in total"), "{error}");
}

#[test]
fn owning_constants_bound_expansion_before_discarding_string_fields() {
    // The result is scalar, but each repeated reference expands a whole
    // template during planning. Limit that work before constructing the IR.
    let text = "x".repeat(1024 * 1024);
    let base = format!(
        "struct P {{ text: String, code: Int32 }}\n\
         const LARGE: P = P {{ text: \"{text}\", code: 1 }}\n"
    );
    let expression = vec!["LARGE.code"; 16].join(" + ");
    let source =
        format!("{base} const COUNT: Int32 = {expression}\nfn check() -> Int32 {{ COUNT }}");
    let mut vm = VmEngine::new_interpreter();
    vm.load_source(&source)
        .unwrap_or_else(|error| panic!("16 MiB input expansion must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 16);
    let source = format!("{base} const COUNT: Int32 = {expression} + LARGE.code");
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("expanded string data"), "{error}");
}
