//! String constant templates use ordinary owning literal storage at every read.
use super::{VmEngine, VmMode};
use crate::{runtime::arena, ErrorCode, FunctionSignature, ModuleSources, Type};

const SOURCE: &str = include_str!("../../../../tests/fixtures/string_constants.jk");
const VALUES: &str = include_str!("../../../../tests/fixtures/string_constants_values.jk");

fn modules() -> ModuleSources {
    let mut modules = ModuleSources::new();
    modules.insert("string_constants_values", VALUES).unwrap();
    modules
}

fn configured(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    vm.load_source_with_modules(SOURCE, &modules())
        .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
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
fn string_constants_report_allocation_failure_at_the_read_site() {
    extern "C" fn exhaust() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let source = r#"
        import string_constants_values as cfg
        extern fn exhaust()
        fn module_read() -> String { exhaust(); cfg.NAME }
        fn associated_read() -> String { exhaust(); cfg.Label.VALUE }
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
        for (entry, expression) in [
            ("module_read", "cfg.NAME"),
            ("associated_read", "cfg.Label.VALUE"),
        ] {
            let error = vm.call_string_0(entry).unwrap_err();
            arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
            assert_eq!(error.code(), ErrorCode::ResourceLimit, "{mode:?}: {error}");
            assert_eq!(
                error.span().and_then(|span| span.slice(source)),
                Some(expression),
                "{mode:?}: {error}"
            );
            clean();
        }
    }
}

#[test]
fn string_constants_materialize_independent_owners_with_budgeted_cleanup() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode);
        let mut failed = false;
        let mut completed = false;
        for budget in (0..16384).step_by(32) {
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
fn string_constants_pending_owners_survive_reload_and_cancel_cleanly() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for finish in [false, true] {
            let mut vm = configured(mode);
            let mut task = vm.call_async("pending", &[]).unwrap();
            assert!(!task.is_finished());
            arena::LIVE.with(|live| assert!(live.get() > 0));
            let error = vm.load_source("const BAD: String = 42").unwrap_err();
            assert_eq!(error.code(), ErrorCode::TypeMismatch);
            assert_eq!(vm.call_string_0("owned").unwrap(), "é中🙂\0");
            vm.load_source("const TEXT: String = \"new\" fn owned() -> String { TEXT }")
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(vm.call_string_0("owned").unwrap(), "new");
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
fn string_constants_reject_runtime_initializers_cycles_and_escaping_owners() {
    let cases = [
        ("const BAD: String = 42", ErrorCode::TypeMismatch, "mismatch"),
        ("const N: Int32 = 42 const BAD: String = N", ErrorCode::TypeMismatch, "mismatch"),
        ("const BAD: String = \"a\" + \"b\"", ErrorCode::InvalidState, "String constants require"),
        ("const BAD: String = \"{42}\"", ErrorCode::InvalidState, "String constants require"),
        ("const BAD: String = String.new()", ErrorCode::InvalidState, "String constants require"),
        ("fn make() -> String { \"a\" } const BAD: String = make()", ErrorCode::InvalidState, "String constants require"),
        ("const BAD: String = if true { \"a\" } else { \"b\" }", ErrorCode::InvalidState, "String constants require"),
        ("const BAD: String = { let x = \"a\"; x }", ErrorCode::InvalidState, "String constants require"),
        ("const A: String = P.B struct P {} impl P { const B: String = A }", ErrorCode::InvalidState, "Cyclic"),
        ("struct P {} impl P { const BAD: String = false }", ErrorCode::TypeMismatch, "mismatch"),
        ("struct P<T> { value: T } impl<T> P<T> { const BAD: T = \"text\" }", ErrorCode::TypeMismatch, "constants require"),
        ("const TEXT: String = \"a\" fn bad() { let value = TEXT; value.release(); value.release() }", ErrorCode::InvalidAssignment, "moved"),
        ("const TEXT: String = \"a\" fn bad() -> &String { let value = TEXT; &value }", ErrorCode::InvalidAssignment, "borrowed input"),
        ("import string_constants_values as cfg const BAD: String = cfg.SECRET", ErrorCode::UnknownName, "constant"),
        ("import string_constants_values as cfg const BAD: String = cfg.Label.PRIVATE", ErrorCode::UnknownName, "constant"),
        ("trait Mark {} struct NoMark {} struct Box<T> { value: T } impl<T: Mark> Box<T> { const NAME: String = \"box\" } fn bad() -> String { Box::<NoMark>.NAME }", ErrorCode::TypeMismatch, "Mark"),
    ];
    for (source, code, message) in cases {
        let error = super::support::frontend_error(source, &modules());
        assert_eq!(error.code(), code, "{source}: {error}");
        assert!(error.message().contains(message), "{source}: {error}");
    }
}

#[test]
fn string_constants_bound_decoded_bytes_per_value_and_across_modules() {
    // UTF-8 byte accounting, including aliases, is independent of source escapes
    // and includes unused constants in the entry and its imported modules.
    let text = "é".repeat(512 * 1024);
    let mut values = ModuleSources::new();
    values
        .insert("large", format!("pub const TEXT: String = \"{text}\""))
        .unwrap();
    let mut source = "import large const A: String = large.TEXT\n".to_owned();
    for index in 0..14 {
        source += &format!("const C{index}: String = A\n");
    }
    source += "fn check() -> Int32 { 42 }";
    let mut vm = VmEngine::new_interpreter();
    vm.load_source_with_modules(&source, &values)
        .unwrap_or_else(|error| panic!("16 MiB must be accepted: {error}"));
    assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    source += "\nstruct P {} impl P { const EXTRA: String = A }";
    let error = super::support::frontend_error(&source, &values);
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("16 MiB"), "{error}");
    let source = format!("struct P {{}} impl P {{ const TEXT: String = \"{text}x\" }}");
    let error = super::support::frontend_error(&source, &ModuleSources::new());
    assert_eq!(error.code(), ErrorCode::ResourceLimit);
    assert!(error.message().contains("1 MiB"), "{error}");
}
