use super::{VmEngine, VmMode};
use crate::{runtime::arena::LIVE, ErrorCode, FunctionSignature, Type};
const SOURCE: &str = include_str!("../../tests/fixtures/int_map.jk");
#[path = "../../tests/fixtures/int_map_oracle.rs"]
mod oracle;
thread_local! { static EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) }; }
extern "C" fn record(n: i32) {
    EVENTS.with(|e| e.borrow_mut().push(n));
}
extern "C" fn deny_allocations() {
    crate::runtime::arena::LIMIT.with(|l| l.set(0));
}
struct RestoreLimit;
impl Drop for RestoreLimit {
    fn drop(&mut self) {
        crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
    }
}
fn configured(mode: VmMode, source: &str) -> VmEngine {
    let registry = std::rc::Rc::new(crate::modules::ModuleRegistry::default());
    assert!(registry
        .types()
        .standard
        .set(crate::core::imports::map_test_library())
        .is_ok());
    let mut vm = VmEngine::with_mode_and_registry(mode, registry);
    unsafe {
        vm.register_fn(
            "print_int",
            FunctionSignature::new(vec![Type::Int32], Type::Unit),
            record as *const u8,
        )
        .unwrap();
        vm.register_fn(
            "deny_allocations",
            FunctionSignature::new(vec![], Type::Unit),
            deny_allocations as *const u8,
        )
        .unwrap();
    }
    vm.load_source_with_modules(source, &crate::ModuleSources::new())
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
fn int_map_checks_indices_and_matches_fixed_seed_oracle() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, SOURCE);
        for name in ["basic", "collisions", "capacities", "oracle_sequence"] {
            EVENTS.with(|e| e.borrow_mut().clear());
            assert_eq!(vm.call_int32_0(name).unwrap(), 42, "{mode:?} {name}");
            let expected = if name == "oracle_sequence" {
                oracle::events()
            } else {
                vec![]
            };
            EVENTS.with(|e| assert_eq!(*e.borrow(), expected, "{mode:?} {name}"));
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
        for name in ["capacity_failure", "bucket_limit_failure"] {
            let e = vm.call_int32_0(name).unwrap_err();
            assert_eq!(e.code(), ErrorCode::RuntimeError);
            assert_eq!(e.message(), "Map capacity exceeds the supported limit");
            assert!(e.span().is_some());
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
#[test]
fn int_map_empty_queries_replacement_and_reserved_inserts_reuse_buffers() {
    let _restore = RestoreLimit;
    let source = format!(
        r#"{SOURCE}
        extern fn deny_allocations()
        fn no_alloc_empty() -> Int32 {{
            var m = Map.new::<Int32, Int32>()
            deny_allocations()
            if m.contains_key(1) || !m.get(1).is_none() || !m.remove(1).is_none() {{ return 0 }}
            42
        }}
        fn no_alloc_replace() -> Int32 {{
            var m = Map.with_capacity::<Int32, Int32>(6) m.insert(1, 20)
            let cap = m.capacity()
            deny_allocations()
            let old = m.insert(1, 22)
            if m.len() != 1 || m.capacity() != cap || m.get(1).unwrap_or(0) != 22 {{ return 0 }}
            old.unwrap_or(0) + 22
        }}
        fn no_alloc_reserve() -> Int32 {{
            var m = Map.new::<Int32, Int32>() m.reserve(20)
            let cap = m.capacity()
            deny_allocations()
            var i = 0
            while i < 20 {{ m.insert(i, i) i = i + 1 }}
            if m.capacity() == cap && maps.test_invariant(&m) {{ 42 }} else {{ 0 }}
        }}
    "#
    );
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let vm = configured(mode, &source);
        for name in ["no_alloc_empty", "no_alloc_replace", "no_alloc_reserve"] {
            let result = vm.call_int32_0(name);
            crate::runtime::arena::LIMIT.with(|l| l.set(crate::runtime::arena::MAX_BYTES));
            assert_eq!(result.unwrap(), 42, "{mode:?} {name}");
            LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}

#[test]
#[cfg(unix)]
fn int_map_checks_bidirectional_indices_in_standalone_native_code() {
    let registry = crate::TypeRegistry::default();
    registry
        .standard
        .set(crate::core::imports::map_test_library())
        .unwrap();
    let ast = registry
        .resolve_with_modules(
            &crate::parser::parse(SOURCE.split("fn main()").next().unwrap()).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap();
    for name in ["basic", "collisions", "capacities", "oracle_sequence"] {
        crate::backends::aot::tests::assert_sync_int32(&ast, name, 42);
    }
    for name in ["capacity_failure", "bucket_limit_failure"] {
        crate::backends::aot::tests::assert_sync_error(&ast, name);
    }
}
