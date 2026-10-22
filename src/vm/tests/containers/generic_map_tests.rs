use super::{support::generic_fixture_entries, VmEngine, VmMode};
use crate::{
    runtime::arena::LIVE, ErrorCode, FunctionSignature, ModuleSources, Type, TypeRegistry,
};
const SOURCE: &str = include_str!("../../../../tests/fixtures/generic_map.jk");
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
fn configured(mode: VmMode, source: &str, modules: &ModuleSources) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_fn(
            "record_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, modules)
        .unwrap_or_else(|e| {
            let location = e.span().and_then(|s| {
                e.sources()
                    .get(s.source_id())
                    .map(|f| (f.name(), s.slice(f.text())))
            });
            panic!("{mode:?}: {e} {location:?}")
        });
    vm
}
#[test]
fn generic_map_executes_basic_custom_colliding_and_nested_types() {
    // Share common specializations without exceeding the generic-instance limit.
    for names in [
        &["integer32", "array_of_maps", "map_of_maps"][..],
        &["integer64", "unsigned64"],
        &["unsigned32", "booleans", "units"],
        &["collisions", "enums"],
        &["retained_key", "cached_rehash"],
        &["lifecycle", "map_of_arrays"],
    ] {
        let entries: Vec<_> = names
            .iter()
            .map(|&name| (format!("probe_{name}"), name))
            .collect();
        let source = generic_fixture_entries(SOURCE, &entries);
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let vm = configured(mode, &source, &ModuleSources::new());
            for (entry, name) in &entries {
                EVENTS.with(|e| e.borrow_mut().clear());
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?} {name}");
                EVENTS.with(|e| {
                    let events = e.borrow();
                    if *name == "retained_key" {
                        assert_eq!(*events, [11, 11, 11]);
                    } else if *name == "cached_rehash" {
                        let start = events.iter().position(|&n| n == 900).unwrap();
                        assert_eq!(
                            events[start + 1],
                            901,
                            "rehash called a protocol: {events:?}"
                        );
                    } else {
                        assert!(events.is_empty(), "{name}: {events:?}");
                    }
                });
                LIVE.with(|n| assert_eq!(n.get(), 0));
            }
        }
    }
}
#[test]
fn generic_map_propagates_protocol_failures_and_cleans_incoming_and_stored_values() {
    let source = generic_fixture_entries(
        SOURCE,
        &[("probe_hash", "hash_failure"), ("probe_eq", "eq_failure")],
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source, &ModuleSources::new());
        for (entry, message) in [
            ("probe_hash", "Map Hash failure"),
            ("probe_eq", "Map Eq failure"),
        ] {
            EVENTS.with(|e| e.borrow_mut().clear());
            let e = vm.call_int32_0(entry).unwrap_err();
            assert_eq!(e.code(), ErrorCode::RuntimeError);
            assert_eq!(e.message(), message);
            EVENTS.with(|e| {
                let mut events = e.borrow().clone();
                events.sort_unstable();
                assert_eq!(events, [1, 2]);
            });
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
fn rejected(source: &str, modules: &ModuleSources, expected: &str) {
    let error = super::support::frontend_error(source, modules);
    assert!(error.message().contains(expected), "{source}: {error}");
    assert!(error.span().is_some());
}
#[test]
fn generic_map_rejects_reference_bearing_keys() {
    for (decl, ty, expected) in [
        ("", "&Int32", "reference-free storage"),
        (
            "struct Key { target: &Int32 }",
            "Key",
            "reference-free storage",
        ),
        (
            "struct Inner { target: &Int32 } struct Key { inner: Inner }",
            "Key",
            "reference-free storage",
        ),
        (
            "enum Key { Empty, Full(&Int32) }",
            "Key",
            "reference-free storage",
        ),
        ("type Key = &Int32", "Key", "reference-free storage"),
    ] {
        rejected(&format!("import joky/map.{{type Map}} import joky/array.{{type Array}} {decl} fn bad() {{ Map.new::<{ty},Int32>(); }}"),&ModuleSources::new(),expected);
    }
    rejected("import joky/map.{type Map} import joky/eq.{type Eq} import joky/hash.{type Hash} struct Key { target: &Int32 } impl Eq for Key { fn eq(self,other:Self)->Bool { true } } impl Hash for Key { fn hash(self)->UInt64 { 0 } } fn bad(m:&mut Map<Key,Int32>,key:Key) { m.insert(key,1); }",&ModuleSources::new(),"reference-free storage");
    let mut modules = ModuleSources::new();
    modules
        .insert(
            "keys",
            "pub struct Inner { target: &Int32 } pub type Alias = Inner",
        )
        .unwrap();
    rejected(
        "import joky/map.{type Map} import keys fn bad() { Map.new::<keys.Alias,Int32>(); }",
        &modules,
        "reference-free storage",
    );
}
#[test]
fn generic_map_rejects_missing_bounds_private_storage_and_conflicting_loans() {
    for (body,expected) in [
        ("struct Key { n: Int32 } impl Eq for Key { fn eq(self, other: Self) -> Bool { true } } fn bad() { var m = Map.new::<Key,Int32>(); m.insert(Key { n: 1 },1) }","does not satisfy trait bound"),
        ("struct Key { n: Int32 } impl Hash for Key { fn hash(self) -> UInt64 { 0 } } fn bad() { var m = Map.new::<Key,Int32>(); m.insert(Key { n: 1 },1) }","does not satisfy trait bound"),
        ("fn bad<K: Copy,V>(m: &Map<K,V>,k: K) -> Bool { m.contains_key(k) }","does not satisfy trait bound"),
        ("fn bad(m: &Map<Int32,Int32>) { m.keys; }","private"),
        ("fn bad(m: &Map<Int32,Int32>) { maps.invariant(m); }","invariant"),
        ("fn bad(m: &maps.Bucket) {}","Bucket"),
        ("fn bad() { var m = Map.new::<Int32,Int32>(); let loan = &m; m.insert(1,2); loan.len(); }","borrow"),
        ("fn bad() { let m = Map.new::<Int32,Int32>(); m.release(); m.len(); }","moved"),
        ("fn bad(m: &Map<Int32,Array<Int32>>) { m.get(1); }","Copy"),
    ] {
        rejected(&format!("import joky/map as maps.{{type Map}} import joky/array.{{type Array}} import joky/eq.{{type Eq}} import joky/hash.{{type Hash}} {body}"),&ModuleSources::new(),expected);
    }
}
#[test]
fn generic_map_cross_module_aliases_and_conditional_impls_preserve_identity() {
    let mut modules = ModuleSources::new();
    modules.insert("keys",r#"
        import joky/map.{type Map as Table}
        import joky/eq.{type Eq as Equality}
        import joky/hash.{type Hash as Hashing}
        pub struct Key<T> { pub value: T }
        impl<T: Equality> Equality for Key<T> { fn eq(self,other: Self)->Bool { Equality.eq(self.value,other.value) } }
        impl<T: Hashing> Hashing for Key<T> { fn hash(self)->UInt64 { Hashing.hash(self.value) } }
        impl<T> Key<T> { fn eq(self,other:Self)->Bool { false } fn hash(self)->UInt64 { 0 } }
        pub type Dictionary<V> = Table<Key<Int32>, V>
        pub fn make<V>() -> Dictionary<V> { Table.new::<Key<Int32>,V>() }
        pub fn put<V>(m: &mut Dictionary<V>,key: Int32,value: V) { m.insert(Key { value: key },value); }
    "#).unwrap();
    let source = r#"import keys fn test()->Int32 {
        var m: keys.Dictionary<Int32> = keys.make::<Int32>()
        keys.put(&mut m,1,20); keys.put(&mut m,1,42)
        let result=m.get(keys.Key { value: 1 }).unwrap_or(0)
        let loan=&m; let n=loan.len()
        m.clear(); m.release()
        if n==1 { result } else { 0 }
    }"#;
    let ast = TypeRegistry::default()
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
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
fn generic_map_uses_the_target_tagged_bucket_layout() {
    use crate::memory_layout::{MemoryLayoutShape, MemoryLayoutTarget};
    let registry = TypeRegistry::default();
    let ast = registry
        .resolve_with_modules(
            &crate::parser::parse(
                "import joky/map.{type Map} fn test() { Map.new::<Int32,Int32>(); }",
            )
            .unwrap(),
            &ModuleSources::new(),
        )
        .unwrap();
    let types = crate::core::types::nominal_types(&registry, &ast).unwrap();
    let bucket = &types["joky/map.Bucket"];
    for align in [4, 8] {
        let layout = bucket
            .memory_layout_for(MemoryLayoutTarget {
                int64_align: align,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (layout.size, layout.align),
            (if align == 8 { 16 } else { 12 }, align)
        );
        let MemoryLayoutShape::Enum { tag, variants } = layout.shape else {
            panic!("bucket must use the ordinary enum ABI")
        };
        assert_eq!(tag.size, 4);
        assert_eq!(variants[2].payload.as_ref().unwrap().offset, align);
    }
}
