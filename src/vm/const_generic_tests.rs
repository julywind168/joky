use super::{VmEngine, VmMode};

#[test]
fn fixed_array_f1_const_parameters_execute_in_both_vms() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let mut modules = crate::ModuleSources::new();
        modules
            .insert(
                "const_model",
                include_str!("../../tests/fixtures/const_generics_model.jk"),
            )
            .unwrap();
        vm.load_source_with_modules(
            include_str!("../../tests/fixtures/const_generics.jk"),
            &modules,
        )
        .unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42, "{mode:?}");
    }
}

#[test]
fn fixed_array_f1_invalid_constants_and_value_operations_are_rejected() {
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};
    let types = crate::TypeRegistry::new();
    for source in [
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { f::<-1>(); }",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { f::<18446744073709551616>(); }",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { let n: UInt64 = 3; f::<n>(); }",
        "const N: UInt64 = 3 fn f<const M: UInt64>() -> UInt64 { M } fn check() { f::<N>(); }",
        "struct S {} impl S { const N: UInt64 = 3 } fn f<const M: UInt64>() -> UInt64 { M } fn check() { f::<S.N>(); }",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { f::<1 + 2>(); }",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { f::<_>(); }",
        "fn f<const N: Int32>() {}",
        "fn f<const N: UInt64 = 3>() {}",
        "enum E<const N: UInt64> { A }",
        "struct Tag<const N: UInt64> {} fn check() { let tag: Tag<4> = Tag {} }",
        "struct Tag<const N: UInt64> {} impl<const N: UInt64> Tag<N> { const VALUE: UInt64 = N }",
        "trait T<const N: UInt64> {}",
        "struct S<const N: UInt64> {} trait T {} impl<const N: UInt64> T for S<N> {}",
        "fn f<const N: UInt64>(N: UInt64) {}",
        "fn f<const N: UInt64>() { let N = 3 }",
        "fn f<const N: UInt64>() { var N = 3 }",
        "fn f<const N: UInt64>() { N = 3 }",
        "fn f<const N: UInt64>() { let (N,x) = (3,4) }",
        "fn f<const N: UInt64>() -> N { N }",
        "fn f<const N: UInt64>() { let g = |N: UInt64| N }",
        "import joky/internal/fixed_array fn f() {}",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { f(); }",
        "fn f<const N: UInt64,T>(x:T) -> UInt64 { N } fn check() { f::<3>(4); }",
        "fn f<const N: UInt64>() -> UInt64 { N } fn check() { let f = f; f(); }",
        "fn f<T>() {} fn check() { f::<3>(); }",
        "type Alias<const N: UInt64> = Int32 fn f(x: &Alias<Int32>) {}",
        "type Alias<T> = Int32 fn f(x: &Alias<4>) {}",
        "fn f<T>(x:T) -> T { x } fn check() { f::<3>(4); }",
        "struct Tag<const N: UInt64> {} fn f<const N: UInt64>(x:Tag<N>) {} fn check() { f::<Int32>(Tag::<4>{}); }",
        "struct Box<T> { value:T } fn check() { Box::<4> { value:3 } }",
        "fn check() { Option.Some::<3>(4); }",
        "fn f<const N: UInt64>() {} fn check() { f::<Int32>(); }",
        "struct Tag<const N: UInt64> {} fn same<const N: UInt64>(a:Tag<N>,b:Tag<N>) {} fn check() { same(Tag::<1>{},Tag::<2>{}); }",
        "struct Tag<const N: UInt64> {} fn f<const N: UInt64,const M: UInt64>(a:Tag<N>) -> Tag<M> { a }",
        "fn f(x:FixedArray<FixedArray<Int32,2>,2>) @suspends -> FixedArray<FixedArray<Int32,2>,2> { x }",
        "fn f(x:&FixedArray<FixedArray<Int32,2>,2>) @suspends {}",
        "fn f() { let x = FixedArray::<Int32,4>{} }",
        "fn f() { FixedArray.new::<Int32,4>(); }",
    ] {
        let source=format!("import joky/fixed_array.{{type FixedArray}}\n{source}");
        let result=crate::parser::parse(&source).map_err(crate::Error::from).and_then(|m|PreparedModule::new(&m,&types,&BindingRegistry::default())).and_then(|p|p.plan(&Default::default()));
        assert!(result.is_err(),"accepted: {source}");
    }
    // Each public compilation entry rejects the same unsupported value boundary.
    let source="import joky/fixed_array.{type FixedArray}\nfn f(x:FixedArray<FixedArray<Int32,2>,2>) @suspends -> FixedArray<FixedArray<Int32,2>,2> { x }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let error = VmEngine::with_mode(mode).load_source(source).unwrap_err();
        assert!(
            error.to_string().contains("not supported in this context"),
            "{error}"
        );
    }
    let mut aot = crate::aot::AotCompiler::new("fixed_array_rejected").unwrap();
    assert!(aot
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err()
        .to_string()
        .contains("not supported in this context"));
}

#[test]
fn fixed_array_f2a_values_execute() {
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(include_str!("../../tests/fixtures/fixed_array_f2a.jk"))
            .unwrap_or_else(|e| {
                panic!(
                    "{mode:?}: {e} {:?}",
                    e.span().and_then(|s| e
                        .sources()
                        .get(s.source_id())
                        .map(|f| (f.name(), s.slice(f.text()))))
                )
            });
        for name in [
            "copies",
            "callbacks",
            "zero",
            "boundary",
            "aggregates",
            "reuse",
            "single",
            "whole_reference",
        ] {
            assert_eq!(
                vm.call_int32_0(name)
                    .unwrap_or_else(|e| panic!("{mode:?} {name}: {e}")),
                42,
                "{mode:?} {name}"
            );
        }
    }
}
