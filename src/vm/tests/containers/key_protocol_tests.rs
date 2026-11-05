use super::{VmEngine, VmMode};
use crate::{aot::AotCompiler, ModuleSources, TypeRegistry};

const SOURCE: &str = include_str!("../../../../tests/fixtures/key_protocols.jk");

#[test]
fn standard_key_protocols_execute_vectors_and_fixed_conditional_dispatch() {
    let source = SOURCE
        .split("fn main()")
        .next()
        .unwrap()
        .replace("extern fn record_int(value: Int32)", "");
    let mut modules = ModuleSources::new();
    modules.insert("keys", r#"
        import joky/eq.{type Eq as Equality}
        import joky/hash.{type Hash as Hashing}
        pub struct Key { pub value: UInt32 }
        impl Equality for Key { fn eq(&self, other: &Self) -> Bool { self.value == other.value } }
        impl Hashing for Key { fn hash(&self) -> UInt64 { Hashing.hash(self.value) } }
        pub type Alias = Key
        pub fn compare<K: Equality + Hashing>(a: &K, b: &K) -> Bool { a.eq(b) && a.hash() == b.hash() }
    "#).unwrap();
    let source = format!(
        r#"import keys
        {source}
        trait Eq {{ fn eq(&self, other: &Self) -> Bool }}
        fn test() -> Int32 {{
            let a: keys.Alias = keys.Key {{ value: 42 }}
            if keys.compare(a, a) && (a == keys.Key {{ value: 42 }}) && (a != keys.Key {{ value: 43 }}) {{ protocols() }} else {{ 0 }}
        }}
    "#
    );
    let ast = TypeRegistry::default()
        .with_prelude(false)
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap_or_else(|error| {
            let source = error
                .span()
                .and_then(|span| span.slice(error.sources().get(span.source_id())?.text()));
            panic!("{error}: {source:?}")
        });
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
        let mut task = vm.call_async("equality_pending", &[]).unwrap();
        assert!(!task.is_finished());
        while !task.is_finished() {
            task.resume().unwrap();
        }
        assert_eq!(task.cps_result(), Some(42));
        drop(task);
        drop(vm.call_async("equality_pending", &[]).unwrap());
        for (entry, message) in [
            ("equality_failed", "Eq comparison failed"),
            ("equality_failed_rhs", "Eq argument failed"),
        ] {
            assert!(vm
                .call_unit_0(entry)
                .unwrap_err()
                .message()
                .contains(message));
        }
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
}

#[test]
fn standard_key_protocols_reject_missing_requirements_and_wrong_signatures() {
    for (body, expected) in [
        ("struct Key { n: Int32 } fn f<K: Copy + Eq + Hash>(k: K) -> Bool { k.eq(k) } fn test() { f(Key { n: 1 }) }", "does not satisfy trait bound"),
        ("fn f<K>(k: K) -> UInt64 { Hash.hash(k) }", "does not satisfy trait bound"),
        ("struct Key {} impl Hash for Key { fn hash(&self) -> Int32 { 0 } }", "Signature does not match trait method"),
        ("struct Key {} impl Eq for Key { fn eq(self, other: &Self) -> Bool { true } }", "Signature does not match trait method"),
        ("struct Key {} impl Key { fn eq(&self, other: &Self) -> Bool { true } } fn f(a: Key, b: Key) -> Bool { a == b }", "does not implement joky/eq.Eq"),
        ("fn f<T>(a: &T, b: &T) -> Bool { a != b }", "does not implement joky/eq.Eq"),
        ("struct Key {} impl Eq for Key { fn eq(&self, other: &Self) -> Bool { true } } fn f(a: Key) -> Bool { a == 1 }", "type mismatch"),
        ("struct Key { text: String } impl Eq for Key { fn eq(&self, other: &Self) -> Bool { true } } fn f(a: Key) -> Bool { a == { let moved = a; moved } }", "borrow"),
    ] {
        let source = format!("import joky/eq.{{type Eq}} import joky/hash.{{type Hash}} {body}");
        let ast = TypeRegistry::default().resolve_with_modules(&crate::parser::parse(&source).unwrap(), &ModuleSources::new()).unwrap();
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let e = VmEngine::with_mode(mode).load_module(&ast).unwrap_err();
            assert!(e.message().contains(expected), "{source}: {e}");
        }
        let e = AotCompiler::new("bad-key-protocol").unwrap().compile_module(&ast).unwrap_err();
        assert!(e.message().contains(expected), "{source}: {e}");
    }
}
