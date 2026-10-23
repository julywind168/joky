use super::support::*;
use super::VmMode;
const SOURCE: &str = include_str!("../../../../tests/fixtures/for_indexed.jk");

#[test]
fn for_indexed_infers_integer_types_and_destructures_map_entries() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = parity_vm_with_prelude(SOURCE, mode, false);
        assert_eq!(vm.call_int32_0("compute").unwrap(), 42);
        assert_eq!(vm.call_int32_1("boundary", 128).unwrap(), 127);
        let error = vm.call_int32_1("boundary", 129).unwrap_err();
        assert!(error.message().contains("For index overflow"), "{error}");
        assert_eq!(vm.call_int32_1("unsigned_boundary", 256).unwrap(), 255);
        assert!(vm
            .call_int32_1("unsigned_boundary", 257)
            .unwrap_err()
            .message()
            .contains("For index overflow"));
        assert!(vm
            .call_unit_0("overflow_owned")
            .unwrap_err()
            .message()
            .contains("For index overflow"));
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}

#[test]
fn for_indexed_rejects_conflicting_types_and_invalid_patterns() {
    for body in [
        "let a: Int32 = i; let b: UInt64 = i;",
        "let a: UInt64 = i; let b: Int32 = i;",
        "let a: Float64 = i;",
        "let a = i; let b: UInt64 = a; let c: Int16 = i;",
        "if true { signed8(i); } else { unsigned64(i); }",
        "expect(i < uint64(2)); signed8(i);",
        "let n = if true { i } else { uint64(2) }; signed8(i);",
        "let f = || -> UInt64 { i }; signed8(i);",
        "i += 1",
    ] {
        let source = format!("{SOURCE}\nfn bad() {{ for i, _ in range(0, 2) {{ {body} }} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{body}: {error}");
        if body != "i += 1" {
            assert!(
                error.message().contains("Cannot infer one integer type"),
                "{body}: {error}"
            );
        }
    }
    for source in [
        "for i, i in [1] {}",
        "for i, (i, v) in [(1, 2)] {}",
        "for (i, j), v in [1] {}",
        "for i, j, v in [1] {}",
        "for i, v in [1] {}; let x = i;",
        "for i, (key, value, extra) in &scores {}",
        "for i, ((key, nested), value) in &scores {}",
        "for (key, value) in &mut scores { *key = 3 }",
        "for (key, value) in &scores { *value = 3 }",
        "for (key, value) in &mut scores { scores.insert(3, 4); *value += *key }",
        "let cursor = scores.iter_mut(); map.advance(cursor); let entry = map.current_mut(cursor); let (_, a) = &mut *entry; let (_, b) = &mut *entry; *a += *b;",
        "let cursor = scores.iter_mut(); map.advance(cursor); let entry = map.current_mut(cursor); let (_, value) = entry; map.advance(cursor); *value += 1;",
    ] {
        let source = format!("import joky/map.{{type Map}}\nfn bad() {{ var scores = Map.new::<Int32, Int32>(); {source} }}");
        let error = frontend_error(&source, &crate::ModuleSources::new());
        assert!(error.span().is_some(), "{source}: {error}");
    }
    for source in [
        "import joky/iter.{enumerate} fn bad() {}",
        "import joky/iter.{type Enumerate} fn bad() {}",
        "fn bad() { let a = [1]; for _ in a.into_iter().enumerate() {} }",
        "fn bad() { let a = [1]; for _ in a.iter().enumerate() {} }",
    ] {
        frontend_error(source, &crate::ModuleSources::new());
    }
}

#[test]
fn for_indexed_u64_maximum_is_valid_until_another_item_arrives() {
    let ast = crate::parser::test_support::indexed_for_u64_boundary();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = super::VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_module(&ast).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vm.call_int32_1("boundary", 0).unwrap(), 0);
        assert_eq!(vm.call_int32_1("boundary", 2).unwrap(), 2);
        let error = vm.call_int32_1("boundary", 3).unwrap_err();
        assert!(error.message().contains("For index overflow"), "{error}");
        assert!(error.span().is_some());
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
}
