//! StringBuilder source algorithms, ownership and resource failure tests.
use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::{self, LIVE};

const SOURCE: &str = include_str!("../../tests/fixtures/string_builder.jk");

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

fn grown_text() -> String {
    let mut text = String::new();
    for index in 0..100 {
        text.push(char::from(b'A' + index % 26));
        if index % 10 == 0 {
            text.push('中');
        }
    }
    text
}

#[test]
fn string_builder_s3_constructs_grows_clears_and_copies_inputs() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        for (name, expected) in [
            ("build", "aé中🙂\0tail".to_owned()),
            ("from_text", "中🙂\0tail!".to_owned()),
            ("empty", String::new()),
            ("clear_reuse", "🙂\0".to_owned()),
            ("growth", grown_text()),
        ] {
            assert_eq!(
                vm.call_string_0(name).unwrap(),
                expected,
                "{mode:?}, {name}"
            );
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        assert_eq!(vm.call_int32_0("operations").unwrap(), 42);
        for name in ["overflow", "oversized_capacity"] {
            assert_eq!(
                vm.call_int32_0(name).unwrap_err().code(),
                crate::ErrorCode::ResourceLimit
            );
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
    }
}

#[test]
fn string_builder_s3_encodes_scalar_width_boundaries() {
    let mut scalars = vec![
        0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0x10000, 0x10ffff,
    ];
    let mut seed = 0x51a3_73b5u32;
    for _ in 0..64 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let scalar = seed % 0x110000;
        if char::from_u32(scalar).is_some() {
            scalars.push(scalar);
        }
    }
    let mut source = String::from("import joky/string.{type StringBuilder} fn text() -> String { var builder = StringBuilder.new() ");
    let mut expected = String::new();
    for scalar in scalars {
        source.push_str(&format!("builder.push('\\u{{{scalar:x}}}') "));
        expected.push(char::from_u32(scalar).unwrap());
    }
    source.push_str("builder.finish() }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert_eq!(
            parity_vm(&source, mode).call_string_0("text").unwrap(),
            expected
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn string_builder_s3_rejects_private_storage_moves_and_conflicting_borrows() {
    let prefix = "import joky/string.{type StringBuilder} ";
    for body in [
        "fn f() { let b = StringBuilder.new() b.push('a') }",
        "fn f() { var b = StringBuilder.new() b.finish().release() b.push('a') }",
        "fn f() { var b = StringBuilder.new() b.release() b.clear() }",
        "fn f() { var b = StringBuilder.new() let other = b b.clear() other.release() }",
        "fn f() { var b = StringBuilder.new() let shared = &b b.push('a') shared.byte_length(); }",
        "fn f(b: &StringBuilder) { b.push('a') }",
        "fn f() { var b = StringBuilder.new() b.data.push(255) }",
        "import joky/array.{type Array} fn f() -> StringBuilder { StringBuilder { data: Array.new::<UInt8>() } }",
        "fn tick() @suspends {} fn f(b: StringBuilder) @suspends -> &StringBuilder { let r = &b tick() r }",
        "fn f(b: &mut StringBuilder) @suspends -> &mut StringBuilder { b }",
    ] {
        let source = format!("{prefix}{body}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            assert!(VmEngine::with_mode(mode).load_source(&source).is_err(), "{mode:?}: {body}");
        }
        assert!(crate::aot::AotCompiler::new("builder-negative").unwrap().compile_module(&crate::parser::parse(&source).unwrap()).is_err(), "AOT: {body}");
    }
}

#[test]
fn string_builder_s3_growth_charges_old_and_new_payloads_at_the_peak() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    let source = r#"
        import joky/string.{type StringBuilder}
        fn grow() -> String {
            var builder = StringBuilder.with_capacity(4)
            builder.push('🙂')
            builder.push('a')
            builder.finish()
        }
    "#;
    let record = std::mem::size_of::<(std::ptr::NonNull<u8>, std::alloc::Layout)>();
    let control = std::mem::size_of::<crate::runtime::buffer::Buffer>() + record;
    let peak = 2 * control + (4 + record) + (8 + record);
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        arena::LIMIT.with(|limit| limit.set(peak - 1));
        assert_eq!(
            vm.call_string_0("grow").unwrap_err().code(),
            crate::ErrorCode::ResourceLimit
        );
        LIVE.with(|live| assert_eq!(live.get(), 0));
        arena::LIMIT.with(|limit| limit.set(peak));
        assert_eq!(vm.call_string_0("grow").unwrap(), "🙂a");
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn string_builder_s3_module_aliases_and_function_values_preserve_owners() {
    let source = r#"
        import parts/text as parts
        fn text() -> String { let build = parts.build build() }
    "#;
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "parts/text",
            r#"
        import joky/string as text.{type StringBuilder as Builder}
        fn append(builder: &mut Builder, rune: Rune) { builder.push(rune) }
        fn finish(builder: Builder) -> String { builder.finish() }
        pub fn build() -> String {
            var builder: Builder = text.StringBuilder.with_capacity(32)
            let append = append
            append(&mut builder, '中')
            let finish = finish
            finish(builder)
        }
    "#,
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_string_0("text").unwrap(), "中");
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn string_builder_s3_budget_failures_reclaim_growth_and_partial_construction() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(SOURCE, mode);
        let mut successes = 0;
        let mut failures = 0;
        for budget in (0..2048).step_by(8) {
            arena::LIMIT.with(|limit| limit.set(budget));
            for name in ["build", "from_text", "clear_reuse", "growth"] {
                match vm.call_string_0(name) {
                    Ok(_) => successes += 1,
                    Err(error) => {
                        assert_eq!(
                            error.code(),
                            crate::ErrorCode::ResourceLimit,
                            "{mode:?}, {name}, {budget}"
                        );
                        failures += 1;
                    }
                }
                LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}, {name}, {budget}"));
            }
        }
        assert!(successes > 0 && failures > 0);
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
}

#[test]
fn string_builder_s3_finish_transfers_with_no_payload_allocation() {
    extern "C" fn exhaust_budget() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    let source = r#"
        import joky/string.{type StringBuilder}
        extern fn exhaust_budget()
        fn text() -> String {
            var builder = StringBuilder.with_capacity(128)
            builder.push('中')
            builder.push('🙂')
            exhaust_budget()
            builder.finish()
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: Static, non-unwinding Unit callback with no parameters.
        unsafe {
            vm.register_fn(
                "exhaust_budget",
                crate::FunctionSignature::new(vec![], crate::Type::Unit),
                exhaust_budget as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_string_0("text").unwrap(), "中🙂");
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn string_builder_s3_interpreter_cancellation_cleans_sync_helpers() {
    let source = r#"
        import joky/string.{type StringBuilder}
        fn build() -> String {
            var builder = StringBuilder.with_capacity(4)
            var index = 0
            while index < 12 {
                builder.push('🙂')
                builder.push('中')
                index = index + 1
            }
            builder.finish()
        }
        fn run() @suspends -> Int32 { let text = build() text.release() 42 }
    "#;
    let mut vm = parity_vm(source, VmMode::Interpreter);
    assert_cancellation(
        &mut vm,
        "run",
        20_000,
        CancellationSweep::ResourceBoundaries,
        || {},
        || LIVE.with(|live| live.get()),
        || LIVE.with(|live| assert_eq!(live.get(), 0)),
    );
}
