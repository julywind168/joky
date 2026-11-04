//! Show and interpolation execute generated code and preserve owning inputs.
use super::{VmEngine, VmMode};
use crate::runtime::arena::{LIMIT, LIVE, MAX_BYTES};

const SOURCE: &str = include_str!("../../../../tests/fixtures/show.jk");

#[test]
fn io_prelude_requires_stdout_only_when_printing_is_used() {
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode_and_registry(mode, registry.clone());
        vm.load_source("fn answer() -> Int32 { 42 }").unwrap();
        assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        for source in [
            "extern fn __joky_write_stdout(text: &String) -> Int32 fn answer() -> Int32 { 42 }",
            "fn answer() { println(42) }",
            "fn answer() { print(42) }",
            "fn answer() @suspends { scope { branch { println(\"hello\") } } }",
        ] {
            let error = vm.load_source(source).unwrap_err();
            assert_eq!(
                error.code(),
                crate::ErrorCode::HostError,
                "{mode:?}: {error}"
            );
            assert!(error.message().contains("__joky_write_stdout"), "{error}");
            assert_eq!(vm.call_int32_0("answer").unwrap(), 42);
        }
    }
}

#[test]
fn automatic_shared_arguments_reclaim_temporaries_after_allocation_failure() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|n| n.set(MAX_BYTES));
        }
    }
    let _reset = Reset;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"
            fn render(value: &String) @noescape -> String { value.clone() }
            fn text() -> String { render("owned={42}") }
        "#,
        )
        .unwrap();
        let mut complete = false;
        for limit in (1..8192).step_by(31) {
            LIMIT.with(|n| n.set(limit));
            let result = vm.call_string_0("text");
            LIMIT.with(|n| n.set(MAX_BYTES));
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if let Ok(text) = result {
                assert_eq!(text, "owned=42");
                complete = true;
                break;
            }
        }
        assert!(complete, "{mode:?}");
    }
}
fn shared_argument_cancellation(sweep: super::support::CancellationSweep) {
    use super::support::{assert_cancellation, parity_vm, EVENTS};
    let mut vm = parity_vm(
        r#"
        import joky/drop.{type Drop}
        extern fn record(value: Int32) -> Int32
        struct Item { value: Int32, text: String }
        impl Drop for Item { fn drop(&mut self) { record(2); } }
        fn make() -> Item { record(1); Item { value: 42, text: "owned" } }
        fn inspect(value: &Item) @noescape -> Int32 { value.value }
        fn helper() -> Int32 { inspect(make()) }
        fn main() @suspends -> Int32 { helper() }
    "#,
        VmMode::Interpreter,
    );
    assert_cancellation(
        &mut vm,
        "main",
        1000,
        sweep,
        || EVENTS.with(|events| events.borrow_mut().clear()),
        || EVENTS.with(|events| events.borrow().len()),
        || {
            LIVE.with(|live| assert_eq!(live.get(), 0));
            EVENTS.with(|events| {
                assert!(events.borrow().iter().filter(|&&event| event == 2).count() <= 1)
            });
        },
    );
}

#[test]
fn automatic_shared_arguments_cancel_at_resource_boundaries() {
    shared_argument_cancellation(super::support::CancellationSweep::ResourceBoundaries);
}

#[test]
#[ignore]
fn exhaustive_automatic_shared_arguments_cancel_at_every_instruction() {
    shared_argument_cancellation(super::support::CancellationSweep::EveryInstruction);
}

#[test]
fn show_and_interpolation_preserve_scalars_custom_owners_and_evaluation_order() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            let source = format!("import joky/option.{{type Option, Some, None}} import joky/result.{{type Result, Ok, Err}}\n{SOURCE}");
            vm.load_source(&source)
                .unwrap_or_else(|e| panic!("{mode:?} prelude={prelude}: {e}"));
            for (name, expected) in [
                ("scalar_text", "-128|-32768|-2147483648|-9223372036854775808|255|65535|4294967295|18446744073709551615|1.5|-0|true|界|{}|NaN|inf"),
                ("custom_text", "<Item(42:hé\0🙂)>|Item(42:hé\0🙂)|hé\0🙂"),
                ("array_text", "[Item(1:one), Item(2:two)]|[Item(1:one), Item(2:two)]"),
                ("option_text", "Some(Item(42:yes))|None|Some(Item(42:yes))"),
                ("result_text", "Ok(Item(42:yes))|Err(no)|Ok(Item(42:yes))"),
                ("evaluation_text", "{x}=1:2:[2]:kept:block:2:kept"),
            ] {
                assert_eq!(vm.call_string_0(name).unwrap(), expected, "{mode:?} {name}");
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}

#[test]
fn interpolation_allocation_failures_reclaim_every_temporary() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            LIMIT.with(|limit| limit.set(MAX_BYTES));
        }
    }
    let _reset = Reset;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(
            r#"fn text() -> String { let value = "owned"; "prefix={value}:{42}:{1.5}" }"#,
        )
        .unwrap();
        let mut complete = false;
        for limit in (1..8192).step_by(31) {
            LIMIT.with(|n| n.set(limit));
            let result = vm.call_string_0("text");
            LIMIT.with(|n| n.set(MAX_BYTES));
            LIVE.with(|n| assert_eq!(n.get(), 0));
            if let Ok(text) = result {
                assert_eq!(text, "prefix=owned:42:1.5");
                complete = true;
                break;
            }
        }
        assert!(complete, "{mode:?}");
    }
}

#[test]
fn interpolation_reports_original_expression_spans_and_rejects_missing_show() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        for source in [
            "struct Missing {} fn text() -> String { let value = Missing {}; \"{value}\" }",
            "fn text() -> String { \"{missing}\" }",
        ] {
            let error = vm.load_source(source).unwrap_err();
            let span = error.span().unwrap();
            assert!(
                source[span.start()..span.end()].contains(if source.contains("Missing") {
                    "value"
                } else {
                    "missing"
                }),
                "{error}"
            );
        }
    }
}

#[test]
fn interpolation_failure_drops_temporary_once_and_preserves_primary_error() {
    thread_local! { static DROPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    extern "C" fn record_drop() {
        DROPS.with(|n| n.set(n.get() + 1));
    }
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        unsafe {
            vm.register_fn(
                "record_drop",
                crate::FunctionSignature::new(vec![], crate::Type::Unit),
                record_drop as *const u8,
            )
            .unwrap();
        }
        vm.load_source(r#"
            import joky/show.{type Show}
            import joky/string.{type StringBuilder}
            import joky/drop.{type Drop}
            extern fn record_drop()
            struct Resource { text: String }
            impl Drop for Resource { fn drop(&mut self) { record_drop() } }
            impl Show for Resource { fn show(&self, output: &mut StringBuilder) @noescape { output.push_str(&self.text); 1 / 0; } }
            fn make() -> Resource { Resource { text: "owned" } }
            fn text() -> String { "prefix {make()} unreachable" }
        "#).unwrap();
        assert!(vm.call_string_0("text").is_err());
        DROPS.with(|n| {
            assert_eq!(n.get(), 1);
            n.set(0);
        });
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
