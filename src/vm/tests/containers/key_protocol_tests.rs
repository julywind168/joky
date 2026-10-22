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
        impl Equality for Key { fn eq(self, other: Self) -> Bool { self.value == other.value } }
        impl Hashing for Key { fn hash(self) -> UInt64 { Hashing.hash(self.value) } }
        pub type Alias = Key
        pub fn compare<K: Copy + Equality + Hashing>(a: K, b: K) -> Bool { a.eq(b) && a.hash() == b.hash() }
    "#).unwrap();
    let source = format!(
        r#"import keys
        {source}
        fn test() -> Int32 {{
            let a: keys.Alias = keys.Key {{ value: 42 }}
            if keys.compare(a, a) {{ protocols() }} else {{ 0 }}
        }}
    "#
    );
    let ast = TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(&source).unwrap(), &modules)
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
}

#[test]
fn standard_key_protocols_reject_missing_requirements_and_wrong_signatures() {
    for (body, expected) in [
        ("struct Key { n: Int32 } fn f<K: Copy + Eq + Hash>(k: K) -> Bool { k.eq(k) } fn test() { f(Key { n: 1 }) }", "does not satisfy trait bound"),
        ("fn f<K>(k: K) -> UInt64 { Hash.hash(k) }", "does not satisfy trait bound"),
        ("struct Key {} impl Hash for Key { fn hash(self) -> Int32 { 0 } }", "Signature does not match trait method"),
        ("struct Key {} impl Eq for Key { fn eq(self, other: &Self) -> Bool { true } }", "Signature does not match trait method"),
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
