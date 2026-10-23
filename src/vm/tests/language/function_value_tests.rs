//! Function values retain their module and preserve ordinary call semantics.
use super::{VmEngine, VmMode};
use crate::{FunctionSignature, Type};
const FIXTURE: &str = include_str!("../../../../tests/fixtures/function_values.jk");
const ENTRIES: &[&str] = &[
    "function_basic",
    "function_order",
    "function_storage",
    "function_layouts",
    "function_combinators",
    "function_arena",
    "function_mixed_dispatch",
];
thread_local! { static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
unsafe extern "C" fn pause(frame: *mut crate::runtime::cps::CpsFrame) -> i32 {
    if !READY.with(std::cell::Cell::get) {
        return 1;
    }
    let frame = &mut *frame;
    frame.result = frame.slots[0];
    0
}
fn engine(mode: VmMode) -> VmEngine {
    let mut vm = VmEngine::with_mode(mode);
    unsafe {
        vm.register_suspending_fn(
            "pause",
            FunctionSignature::new(vec![Type::Int32], Type::Int32),
            pause,
        )
        .unwrap();
    }
    vm
}
#[test]
fn function_values_execute_generics_storage_combinators_and_effects() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for prelude in [true, false] {
            let mut vm = engine(mode);
            vm.set_prelude_enabled(prelude);
            let exports = vm.load_source(FIXTURE).unwrap();
            assert!(!exports
                .iter()
                .any(|n| n.starts_with("__joky_") || *n == "choose"));
            for entry in ENTRIES {
                assert_eq!(vm.call_int32_0(entry).unwrap(), 42, "{mode:?}: {entry}");
            }
            // Taking an address does not remove the original scalar host entry.
            assert_eq!(vm.call_int32_1("increment", 41).unwrap(), 42);
            let error = vm.call_int32_0("function_failure").unwrap_err();
            assert!(error.message().contains("zero"), "{error}");
            assert_eq!(vm.call_int32_0("function_basic").unwrap(), 42);
            crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
        }
    }
}
#[test]
fn function_values_survive_pending_reload_vm_drop_and_cancellation() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = engine(mode);
        vm.load_source(FIXTURE).unwrap();
        READY.with(|v| v.set(false));
        let mut old = vm.call_async("function_task", &[]).unwrap();
        let mut ordered = vm.call_async("function_task_order", &[]).unwrap();
        let mut aggregate = vm.call_async("function_task_aggregate", &[]).unwrap();
        for task in [&mut old, &mut ordered, &mut aggregate] {
            task.resume().unwrap();
            assert!(task.cps_result().is_none());
        }
        let cancelled = vm.call_async("function_task", &[]).unwrap();
        drop(cancelled);
        vm.load_source(&FIXTURE.replace("{ x + 1 }", "{ x + 10 }"))
            .unwrap();
        let mut new = vm.call_async("function_task", &[]).unwrap();
        assert_ne!(old.module_version(), new.module_version());
        drop(vm);
        READY.with(|v| v.set(true));
        for task in [&mut old, &mut ordered, &mut aggregate] {
            task.resume().unwrap();
            assert_eq!(task.cps_result(), Some(42));
        }
        new.resume().unwrap();
        assert_eq!(new.cps_result(), Some(51));
        drop((old, ordered, aggregate, new));
        crate::runtime::arena::LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
#[test]
fn function_values_reject_invalid_signatures_bounds_effects_and_host_escapes() {
    for (source, message) in [
        (
            "fn id(x: Int32) -> Int32 { x } fn bad() { let f: fn(Bool) -> Int32 = id }",
            "mismatch",
        ),
        (
            "fn id(x: Int32) -> Int32 { x } fn bad() { let f = id; f(true); }",
            "mismatch",
        ),
        (
            "fn id(x: Int32) -> Int32 { x } fn bad() { let f = id; f(); }",
            "count",
        ),
        (
            "fn id<T>(x: T) -> T { x } fn bad() { let f = id }",
            "explicit type arguments",
        ),
        (
            "fn id(x: Int32) -> Int32 { x } fn bad() { let f = id::<Int32> }",
            "Non-generic",
        ),
        (
            "fn id(x: Int32) -> Int32 { x } fn bad() { let f = id; f::<Int32>(1); }",
            "do not accept",
        ),
        (
            "fn async_fn() @suspends {} fn bad() { let f = async_fn; f() }",
            "@suspends",
        ),
        (
            "extern fn pause(x: Int32) @suspends -> Int32 fn bad() { let f = pause }",
            "language function",
        ),
        ("extern fn host(f: fn(Int32) -> Int32)", "host ABI"),
        (
            "struct Holder { f: fn(Int32) -> Int32 } unsafe extern \"C\" fn host(x: &Holder)",
            "host ABI",
        ),
        (
            "trait Mark {} fn need<T: Mark>(x: T) -> T { x } fn bad() { let f = need::<Int32> }",
            "does not satisfy",
        ),
        ("fn bad() { let id = 42; id(1); }", "not a function"),
        (
            "fn bad(f: fn(&Int32) -> Int32) -> Int32 { let x = 42; f(&x) }",
            "retained",
        ),
        (
            "fn escape(x: &Int32) -> &Int32 { x } fn bad() { let f: fn(&Int32) @noescape -> &Int32 = escape }",
            "does not satisfy @noescape",
        ),
        (
            "fn escape(x: &Int32) -> &Int32 { x } fn relay(x: &Int32) -> &Int32 { escape(x) } fn bad() { let f: fn(&Int32) @noescape -> &Int32 = relay }",
            "does not satisfy @noescape",
        ),
        (
            "fn bad(f: fn(&Int32) -> Int32) { let promised: fn(&Int32) @noescape -> Int32 = f }",
            "mismatch",
        ),
        (
            "fn escape(x: &Int32) -> &Int32 { x } fn bad() { let f: fn(&Int32) @noescape -> &Int32 = |value: &Int32| { escape(value) } }",
            "does not satisfy @noescape",
        ),
        ("fn bad<T>(x: T) { x(); }", "not a function"),
        ("fn bad(f: fn() -> Int32) -> Bool { f == f }", "operands"),
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = engine(mode);
            vm.load_source("fn previous() -> Int32 { 42 }").unwrap();
            let error = vm.load_source(source).expect_err(source);
            assert!(
                error.message().contains(message),
                "{mode:?}: {error}\n{source}"
            );
            assert_eq!(vm.call_int32_0("previous").unwrap(), 42);
        }
        let error = crate::aot::AotCompiler::new("bad-function-value")
            .unwrap()
            .compile_module(&crate::parser::parse(source).unwrap())
            .expect_err(source);
        assert!(error.message().contains(message), "{error}\n{source}");
    }
}
#[test]
fn function_values_keep_private_definition_scopes_and_imported_identities() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("helpers", "pub struct Number { pub n: Int32 } fn hidden(x: Number) -> Int32 { x.n } pub fn get() -> fn(Number) -> Int32 { hidden } pub fn identity<T>(x: T) -> T { x } pub fn inc(x: Int32) -> Int32 { x + 1 }").unwrap();
    let source = "import helpers as h.{inc as plus} fn main() -> Int32 { let f = h.get(); let same = h.identity::<h.Number>; if plus(41) != 42 { return 0 }; let g = plus; if g(41) != 42 { return 0 }; f(same(h.Number { n: 42 })) }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("main").unwrap(), 42);
        assert!(vm
            .load_source_with_modules(
                "import helpers fn bad() { let f = helpers.hidden }",
                &modules
            )
            .is_err());
    }
}

#[test]
fn language_function_values_keep_legacy_native_registration_abi() {
    let mut vm = VmEngine::with_mode(VmMode::Jit);
    vm.load_source(r#"
        import joky/vm
        fn increment(x: Int32) -> Int32 { x + 1 }
        fn main() -> Int32 {
            let f = increment
            let handle = vm.Vm.new()
            { let vm_text = "increment"; vm.register_int32_1(handle, &vm_text, increment) }
            { let vm_text = "extern fn increment(x: Int32) -> Int32 fn go() -> Int32 {{ increment(41) }}"; vm.load_source(handle, &vm_text) }
            let result = { let vm_text = "go"; vm.call_int32_0(handle, &vm_text) }
            vm.close(handle)
            if result == f(41) { result } else { 0 }
        }
    "#).unwrap();
    assert_eq!(vm.call_int32_0("main").unwrap(), 42);
}

#[test]
fn function_type_graphs_have_bounded_comparison_normalization_and_substitution() {
    for source in [
        "type F = fn(&F) -> Int32 fn bad(f: F) {}",
        "type F<T> = fn(&F<T>) -> T fn bad(f: F<Int32>) {}",
        "type A = fn(&B) -> Int32 type B = fn(&A) -> Int32 fn bad(f: A) {}",
    ] {
        let registry = crate::TypeRegistry::new();
        assert!(
            registry
                .resolve_aliases(&crate::parser::parse(source).unwrap())
                .is_err(),
            "{source}"
        );
    }
    let mut reference_chain = String::from("type F0 = Int32\n");
    for i in 1..40 {
        reference_chain.push_str(&format!("type F{i} = fn(&F{}) -> Int32\n", i - 1));
    }
    let registry = crate::TypeRegistry::new();
    assert!(registry
        .resolve_aliases(&crate::parser::parse(&reference_chain).unwrap())
        .is_err());
    let mut source = String::from("type F0 = fn(Int32) -> Int32\n");
    for i in 1..31 {
        source.push_str(&format!(
            "type F{i} = fn(F{}, F{}) -> F{}\n",
            i - 1,
            i - 1,
            i - 1
        ));
    }
    source.push_str("fn id<T>(x: T) -> T { x } fn hold(x: F30) -> F30 { id(x) } fn other(x: F30) -> F30 { x } fn main() -> Int32 { 42 }");
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(&source).unwrap();
        assert_eq!(vm.call_int32_0("main").unwrap(), 42);
    }
    let registry = crate::TypeRegistry::new();
    let module = registry
        .resolve_aliases(&crate::parser::parse(&source).unwrap())
        .unwrap();
    let again = registry.resolve_aliases(&module).unwrap();
    let find = |m: &crate::parser::ast::Module, name: &str| -> Type {
        m.items
            .iter()
            .find_map(|i| match i {
                crate::parser::ast::Item::FnDef(f) if f.name == name => Some(f.ret_ty.clone()),
                _ => None,
            })
            .unwrap()
    };
    let ty = find(&module, "hold");
    assert_eq!(ty, find(&again, "other"));
    assert!(ty.to_string().len() < 5000);
    let mut oversized = Type::Int32;
    for _ in 0..258 {
        oversized = Type::function(vec![oversized], Type::Unit);
    }
    assert!(registry
        .validate(&oversized)
        .unwrap_err()
        .message()
        .contains("256"));
    assert!(crate::parser::parse("fn valid(x: fn() @suspends -> Int32) {}").is_ok());
}
