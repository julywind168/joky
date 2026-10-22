//! Inferred borrowing guarantees must agree across every generated-code backend.
use super::{InstanceValue as V, VmEngine, VmMode};
use crate::aot::AotCompiler;

fn executes(source: &str, expected: i32) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(vm.call_int32_0("probe").unwrap(), expected);
    }
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "probe",
        expected,
    );
}

fn rejects(source: &str) {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert!(
            VmEngine::with_mode(mode).load_source(source).is_err(),
            "{source}"
        );
    }
    assert!(
        AotCompiler::new("inferred-borrows")
            .unwrap()
            .compile_module(&crate::parser::parse(source).unwrap())
            .is_err(),
        "{source}"
    );
}

#[test]
fn noescape_inference_handles_allocating_mutual_recursion_and_implicit_arguments() {
    executes(
        r#"
        fn second(value: &Int32, n: Int32) -> Int32 {
            let unused = alloc(0)
            if n == 0 { (*value) } else { first(value, n - 1) }
        }
        fn first(value: &Int32, n: Int32) -> Int32 { second(value, n) }
        fn probe() -> Int32 { let value = 42; first(value, 5) }
    "#,
        42,
    );
}

#[test]
fn noescape_inference_preserves_return_sources_and_excludes_only_proven_parameters() {
    executes(
        r#"
        fn choose(value: &Int32, ignored: &Int32) -> &Int32 {
            let unused = alloc(0)
            value
        }
        fn identity(value: &Int32) -> &Int32 { value }
        fn probe() -> Int32 {
            let value = (&*(alloc(42)))
            let result = { let ignored = 9; choose(value, &ignored) }
            let local = 2;
            (*result) + (*(identity(&local)))
        }
    "#,
        44,
    );
}

#[test]
fn noescape_inference_checks_generic_bodies_and_references_inside_aggregates() {
    executes(
        r#"
        import joky/string
        fn unused<T>(value: T) -> UInt64 { string.byte_length("abc") }
        struct Input { value: &Int32 }
        fn read<T: Copy>(value: &T) -> T { let unused = alloc(0) ; (*value) }
        fn wrapper(input: Input) -> Int32 { read(input.value) }
        fn probe() -> Int32 {
            let value = 21
            wrapper(Input { value: &value }) + read(value)
        }
    "#,
        42,
    );
}

#[test]
fn noescape_inference_rejects_retention_forwarding_and_implicit_escaping_borrows() {
    for source in [
        "struct Saved { value: &Int32 } fn retain(value: &Int32) { let saved = alloc(Saved { value: value }) } fn first(value: &Int32, n: Int32) { if n == 0 { retain(value) } else { second(value, n - 1) } } fn second(value: &Int32, n: Int32) { first(value, n) } fn probe() { let local = 42; first(&local, 2) }",
        "fn store<T: Copy>(x: T) { let saved = alloc((x,)) } fn forward(value: &Int32) { store(Some(value)) } fn probe() { let local = 42; forward(&local) }",
        "fn store<T: Copy>(x: T) { let saved = alloc((x,)) } fn unused<U>(value: &Int32) @noescape { store(Some(value)) } fn probe() {}",
        "fn identity(value: &Int32) -> &Int32 { value } fn probe() { identity(42); }",
        "fn identity(value: &Int32) -> &Int32 { value } fn probe() -> &Int32 { let local = 42; identity(&local) }",
        "fn choose(a: &Int32, b: &Int32) -> &Int32 { b } fn bad(value: &Int32) @noescape -> &Int32 { choose(value, value) }",
        "struct Saved { value: &Int32 } fn bad<T>(value: &T) @noescape -> &T { value } fn probe() {}",
        "import joky/array.{type Array} fn save(out: &mut Array<&Int32>, value: &Int32) { out.push(value) } fn probe() { var out = Array.new::<&Int32>(); let local = 42; save(&mut out, &local) }",
        "trait Query { fn get(&self) -> Int32 } fn read<T: Copy + Query>(value: &T) { let local = 1; value.get(); } fn implicit<T: Copy + Query>(value: T) -> Int32 { value.get() }",
    ] {
        rejects(source);
    }
}

#[test]
fn noescape_inference_keeps_indirect_calls_conservative() {
    rejects(
        r#"
        fn read(value: &Int32) -> Int32 { (*value) }
        fn invoke(f: fn(&Int32) -> Int32, value: &Int32) -> Int32 { f(value) }
        fn probe() -> Int32 { let local = 42; invoke(read, &local) }
    "#,
    );
}

#[test]
fn noescape_inference_admits_instance_methods_but_not_retaining_or_unknown_host_wrappers() {
    let source = r#"
        #repr(C) struct State { count: Int32 }
        struct Saved { state: &State }
        fn make() -> State { State { count: 0 } }
        fn update(state: &mut State) { let unused = alloc(0); state.count = state.count + 1 }
        fn tick(state: &mut State) -> Int32 { update(state); state.count }
        fn retain(state: &State) { let saved = alloc(Saved { state: state }) }
        fn forward(state: &State) { retain(state) }
        unsafe extern "C" fn unknown(state: &State)
        fn host(state: &State) { unsafe { unknown(state) } }
        fn host_wrapper(state: &State) { host(state) }
    "#;
    // Metadata checking needs no linked host function and exercises AOT as well.
    let mut aot = AotCompiler::new("inferred-instance").unwrap();
    aot.compile_module(&crate::parser::parse(source).unwrap())
        .unwrap();
    let types = crate::TypeRegistry::default();
    let ast = types
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &Default::default())
        .unwrap();
    let plan = crate::core::ir::ModulePlan::with_import_layouts_and_types(
        &ast,
        &Default::default(),
        &types,
    )
    .unwrap();
    assert!(!crate::core::instance::eligible(&plan.functions["host"]));
    assert!(!crate::core::instance::eligible(
        &plan.functions["host_wrapper"]
    ));
    // The executable VM source excludes the unbound external symbol.
    let executable = source.split("unsafe extern").next().unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(executable).unwrap();
        assert!(vm.instance_function_signature("tick").is_some());
        assert!(vm.instance_function_signature("retain").is_none());
        assert!(vm.instance_function_signature("forward").is_none());
        let id = vm.create_instance("make", &[]).unwrap();
        assert_eq!(vm.call_instance(id, "tick", &[]).unwrap(), V::Int32(1));
        assert_eq!(vm.call_instance(id, "tick", &[]).unwrap(), V::Int32(2));
        vm.close_instance(id).unwrap();
    }
}

#[test]
fn noescape_inference_consumes_returned_views_without_retaining_the_input() {
    executes(
        r#"
        fn identity(value: &Int32) -> &Int32 { let unused = alloc(0); value }
        fn read(value: &Int32) -> Int32 { (*(identity(value))) }
        fn text_size(value: &String) -> Int32 { int32(value.byte_length()) }
        struct Point { value: Int32 }
        impl Point { fn read(&self) -> Int32 { self.value } }
        fn first(a: &Point, b: &Point) -> &Point { a }
        fn probe() -> Int32 {
            let a = alloc(Point { value: 20 })
            let b = alloc(Point { value: 0 })
            let view = first(a, b)
            b.value = 2
            let local = 18
            read(local) + text_size("four") + Point.read(Point { value: 0 }) + view.value
        }
    "#,
        42,
    );
}

#[test]
fn noescape_inference_does_not_reborrow_reference_expressions() {
    executes(
        r#"
        fn read(value: &Int32) -> Int32 { (*value) }
        fn probe() -> Int32 { read((&*(alloc(42)))) }
    "#,
        42,
    );
}

#[test]
fn noescape_inference_maps_aggregate_lanes_to_source_parameters() {
    executes(
        r#"
        struct Empty {}
        fn choose(prefix: (Int32, Int32), empty: Empty, unit: Unit,
                  value: &Int32, ignored: &Int32) @noescape(ignored) -> &Int32 {
            value
        }
        fn probe() -> Int32 {
            let value = 42
            let result = { let ignored = 1; choose((1, 2), Empty {}, {}, &value, ignored) };
            (*result)
        }
    "#,
        42,
    );
}

#[test]
fn noescape_inference_follows_imported_private_generic_helpers() {
    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "helpers",
            r#"
        fn read<T: Copy>(value: &T) -> T { let unused = alloc(0) ; (*value) }
        pub fn inspect<T: Copy>(value: &T) -> T { read(value) }
    "#,
        )
        .unwrap();
    let ast = crate::TypeRegistry::default()
        .resolve_with_modules(
            &crate::parser::parse("import helpers.{inspect} fn probe() -> Int32 { inspect(42) }")
                .unwrap(),
            &modules,
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("probe").unwrap(), 42);
    }
    crate::backends::aot::tests::assert_sync_int32(&ast, "probe", 42);
}
