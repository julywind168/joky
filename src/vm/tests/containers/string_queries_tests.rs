//! UTF-8 validation and text queries against independent host models.
use super::support::*;
use super::{VmEngine, VmMode};
use crate::runtime::arena::{self, LIVE};

use crate::string_query_model as model;

#[test]
fn string_queries_matches_utf8_substring_rune_and_decimal_models() {
    let cases = model::cases();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(&cases.source).unwrap();
        for (name, expected) in &cases.integers {
            assert_eq!(
                vm.call_int32_0(name).unwrap(),
                *expected,
                "{mode:?}, {name}"
            );
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        for (name, expected) in &cases.strings {
            assert_eq!(
                vm.call_string_0(name).unwrap(),
                *expected,
                "{mode:?}, {name}"
            );
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        for (name, message) in [
            ("bad_offset", "offset"),
            ("huge_offset", "offset"),
            ("reversed_byte_range", "range"),
            ("oversized_byte_range", "range"),
        ] {
            let error = vm.call_int32_0(name).unwrap_err();
            assert_eq!(error.code(), crate::ErrorCode::RuntimeError);
            assert!(error.span().is_some(), "{mode:?}, {name}: {error}");
            assert!(error.message().contains(message));
            LIVE.with(|live| assert_eq!(live.get(), 0));
        }
        assert_eq!(
            vm.call_int32_0("overflow").unwrap_err().code(),
            crate::ErrorCode::ResourceLimit
        );
    }
}

#[test]
fn string_queries_rejects_byte_view_escape_writes_moves_and_private_freezing() {
    for source in [
        "import joky/slice.{type Slice} fn f() -> &Slice<UInt8> { let text = \"中\"; text.bytes() }",
        "import joky/slice.{type Slice} fn view(text: &String) -> &Slice<UInt8> { text.bytes() } fn f() -> &Slice<UInt8> { let text = \"中\"; view(&text) }",
        "import joky/slice fn f() { let text = \"中\"; slice.set(text.bytes(), 0, 255); }",
        "import joky/slice fn f() { let text = \"中\"; let view = text.bytes(); text.release(); slice.len(view); }",
        "import joky/slice fn f() { let text = \"中\"; let view = text.bytes(); text.into_bytes().release(); slice.len(view); }",
        "import joky/slice fn f() { var text = \"中\"; let view = text.bytes(); text = \"next\"; slice.len(view); }",
        "import joky/mem\nimport joky/slice fn f() { var text = \"中\"; let view = text.bytes(); mem.replace(&mut text, \"next\").release(); slice.len(view); }",
        "import joky/slice fn f() @suspends { let text = \"中\"; let view = text.bytes(); join {}; slice.len(view); }",
        "import joky/array.{type Array} fn f() { let data = Array.new::<UInt8>(); String.from_utf8(data); data.release() }",
        "import joky/array.{type Array} fn f() { var data: Array<UInt8> = [42]; let view = data.slice(0, 1); array.extend_bytes(&mut data, view, 0, 1) }",
        "import joky/array.{type Array} fn f() { var data: Array<UInt8> = [42]; let view = data.slice(0, 1); data.release(); var out: Array<UInt8> = []; array.extend_bytes(&mut out, view, 0, 1) }",
        "import joky/string.{freeze_utf8} fn f() {}",
    ] {
        let error = super::support::frontend_error(source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
}

struct RestoreLimit(usize);
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        arena::LIMIT.with(|limit| limit.set(self.0));
    }
}

#[test]
fn string_queries_budget_failures_and_cancellation_reclaim_partial_text() {
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    let source = include_str!("../../../../tests/fixtures/string_queries.jk");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm(source, mode);
        let mut successes = 0;
        let mut failures = 0;
        for budget in (0..2048).step_by(8) {
            arena::LIMIT.with(|limit| limit.set(budget));
            match vm.call_int32_0("work") {
                Ok(value) => {
                    assert_eq!(value, 42);
                    successes += 1;
                }
                Err(error) => {
                    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
                    failures += 1;
                }
            }
            LIVE.with(|live| assert_eq!(live.get(), 0, "{mode:?}, {budget}"));
        }
        assert!(successes > 0 && failures > 0);
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
    }
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

#[test]
fn string_queries_byte_borrow_and_consuming_conversion_need_no_allocation() {
    extern "C" fn exhaust_budget() {
        arena::LIMIT.with(|limit| limit.set(0));
    }
    let _restore = RestoreLimit(arena::LIMIT.with(|limit| limit.get()));
    let source = r#"
        import joky/slice
        extern fn exhaust_budget()
        fn transfer() -> Int32 {
            let text = "é中🙂"
            let header = "Content-TYPE"
            let lower = "content-type"
            exhaust_budget()
            if !header.eq_ignore_ascii_case(&lower) || header.eq_ignore_ascii_case(&text) { return 3 }
            let view = text.bytes()
            if slice.len(view) != 9 { return 1 }
            let data = text.into_bytes()
            if data.len() != 9 || data.get(0).unwrap_or(0) != 0xc3 { return 2 }
            data.release()
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        arena::LIMIT.with(|limit| limit.set(arena::MAX_BYTES));
        let mut vm = VmEngine::with_mode(mode);
        // SAFETY: Static non-unwinding callback with the exact Unit signature.
        unsafe {
            vm.register_fn(
                "exhaust_budget",
                crate::FunctionSignature::new(vec![], crate::Type::Unit),
                exhaust_budget as *const u8,
            )
            .unwrap();
        }
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("transfer").unwrap(), 42);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn string_equality_borrows_places_and_temporaries_without_prelude() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(include_str!(
            "../../../../tests/fixtures/string_equality.jk"
        ))
        .unwrap_or_else(|error| panic!("{error}"));
        for entry in ["compute", "early"] {
            assert_eq!(vm.call_int32_0(entry).unwrap(), 42);
        }
        LIVE.with(|n| assert_eq!(n.get(), 0));
        assert!(vm
            .call_unit_0("failed")
            .unwrap_err()
            .message()
            .contains("comparison argument failed"));
        LIVE.with(|n| assert_eq!(n.get(), 0));
        vm.load_source("fn equals(a: &String, b: &String) -> Bool { false } fn compute() -> Int32 { if \"a\" == \"a\" { 42 } else { 0 } }").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
