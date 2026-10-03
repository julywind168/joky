//! Borrowed protocols must be checked independently of the Map implementation.
use super::{VmEngine, VmMode};
use crate::{aot::AotCompiler, ErrorCode};

fn rejects(source: &str, message: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = VmEngine::with_mode(mode).load_source(source).unwrap_err();
        assert!(error.message().contains(message), "{source}: {error}");
    }
    let ast = crate::parser::parse(source).unwrap();
    let error = AotCompiler::new("borrowed-keys")
        .unwrap()
        .compile_module(&ast)
        .unwrap_err();
    assert!(error.message().contains(message), "{source}: {error}");
}

#[test]
fn string_s6_noescape_rejects_retention_return_forwarding_and_trait_violations() {
    for source in [
        "fn bad(value: &Int32) @noescape -> &Int32 { value }",
        "struct Saved { value: &Int32 } fn bad(value: &Int32) @noescape { let stored = alloc(Saved { value: value }) }",
        "struct Saved { value: &Int32 } fn retain(value: &Int32) { let stored = alloc(Saved { value: value }) } fn bad(value: &Int32) @noescape { retain(value) }",
        "import joky/array.{type Array} fn bad(out: &mut Array<&Int32>, value: &Int32) @noescape(value) { out.push(value) }",
        "trait Query { fn check(&self) @noescape -> Bool } struct Key { n: Int32 } struct Saved { key: &Key } impl Query for Key { fn check(&self) -> Bool { let stored = alloc(Saved { key: self }) true } } fn f() {}",
        "trait Query { fn check(&self, other: &Int32) @noescape(other) -> &Int32 } struct Key { n: Int32 } impl Query for Key { fn check(&self, renamed: &Int32) -> &Int32 { renamed } } fn f() {}",
        "fn escaped(a: &Int32, b: &Int32) @noescape(a) -> &Int32 { b } fn bad(value: &Int32) @noescape -> &Int32 { escaped(value, value) }",
        "fn forward<T>(value: &T) @noescape -> &T { value } fn f() {}",
    ] {
        rejects(source, "@noescape");
    }
}

#[test]
fn string_s6_key_loans_block_structural_changes_and_mutation() {
    for body in [
        "let keys = m.keys() m.clear() slice.len(keys);",
        "let keys = m.keys() let old = memory.replace(&mut m, Map.new::<String, Int32>()) slice.len(keys);",
        "let keys = m.keys() let key = slice.borrow_ref(keys, 0) m.insert(\"b\", 1) key.byte_length();",
        "let keys = m.keys() slice.borrow_mut_ref(keys, 0);",
        "let query = \"a\" let value = m.value_ref(&query) m.remove_by_ref(&query) memory.read(value);",
    ] {
        let source = format!("import joky/map.{{type Map}} import joky/slice fn f() {{ var m = Map.new::<String, Int32>() m.insert(\"a\", 1) {body} }}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let error = VmEngine::with_mode(mode).load_source(&source).unwrap_err();
            assert!(matches!(error.code(), ErrorCode::InvalidAssignment | ErrorCode::TypeMismatch), "{source}: {error}");
        }
        assert!(AotCompiler::new("key-loans").unwrap().compile_module(&crate::parser::parse(&source).unwrap()).is_err());
    }
}

#[test]
fn string_s6_noescape_recursion_and_blanket_protocols_are_general() {
    let source = r#"
        trait Value { fn value(self) -> Int32 }
        trait Query { fn query(&self) @noescape -> Int32 }
        impl<T: Value> Query for T { fn query(&self) -> Int32 { Value.value(memory.read(self)) } }
        impl Value for Int32 { fn value(self) -> Int32 { self } }
        fn recurse(value: &Int32, n: Int32) @noescape -> Int32 {
            if n == 0 { Query.query(value) } else { recurse(value, n - 1) }
        }
        fn choose(left: &Int32, ignored: &Int32) @noescape(ignored) -> &Int32 { left }
        fn probe() -> Int32 {
            let value = 42
            let result = recurse(&value, 8)
            let loan = { let ignored = 9 choose(&value, &ignored) }
            if memory.read(loan) == result { result } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
    }
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "probe",
        42,
    );
}

#[test]
fn string_s6_blanket_overlap_has_no_specialization_priority() {
    rejects(
        r#"
        trait A { fn a(self) -> Int32 }
        trait B { fn b(self) -> Int32 }
        impl<T: A> B for T { fn b(self) -> Int32 { A.a(self) } }
        impl A for Int32 { fn a(self) -> Int32 { self } }
        impl B for Int32 { fn b(self) -> Int32 { self + 1 } }
        fn f() -> Int32 { B.b(1) }
    "#,
        "trait",
    );
}
