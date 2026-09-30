//! Transparent alias templates, scoping, and bounded expansion.
use super::*;
use std::sync::Arc;

fn function(module: &Module, index: usize) -> &crate::parser::ast::FnDef {
    let Item::FnDef(function) = &module.items[index] else {
        panic!()
    };
    function
}

#[test]
fn generic_aliases_erase_to_canonical_types_and_cache_expansions() {
    let registry = TypeRegistry::new().with_prelude(false);
    let source = r#"
        type PairOf<T> = (T, T)
        type Same<T> = PairOf<T>
        type Ignored<T> = Int32
        type Wrapped<T> = Box<PairOf<T>>
        struct Box<T> { value: T }
        fn a(x: PairOf<Int64>) -> (Int64, Int64) { x }
        fn b(x: Same<Int64>) -> PairOf<Int64> { x }
        fn c(x: Ignored<Bool>) -> Ignored<Unit> { x }
        fn d(x: Wrapped<Int32>) -> Box<(Int32, Int32)> { x }
        fn generic<T>(x: PairOf<T>) -> Same<T> { x }
    "#;
    let parsed = crate::parser::parse(source).unwrap();
    let normalized = registry.resolve_aliases(&parsed).unwrap();
    let again = registry.resolve_aliases(&normalized).unwrap();
    assert_eq!(parsed.type_aliases.len(), 4);
    assert!(normalized.type_aliases.is_empty());
    for index in 0..5 {
        let f = function(&normalized, index);
        assert_eq!(f.params[0].ty, f.ret_ty);
        assert_eq!(f.ret_ty, function(&again, index).ret_ty);
    }
    let (Type::Tuple(a), Type::Tuple(b)) = (
        &function(&normalized, 0).params[0].ty,
        &function(&normalized, 1).ret_ty,
    ) else {
        panic!()
    };
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(function(&normalized, 2).ret_ty, Type::Int32);
    assert_eq!(
        function(&normalized, 3).ret_ty.to_string(),
        "Box<(Int32, Int32)>"
    );
    let f = function(&normalized, 4);
    registry
        .with_parameters(&f.type_params)
        .unwrap()
        .validate(&f.ret_ty)
        .unwrap();
    assert_eq!(
        registry.validate(&f.ret_ty).unwrap_err().code(),
        ErrorCode::UnknownType
    );
    assert!(registry.alias_instances.borrow().is_empty());

    // A parameter from another declaration cannot escape through a hand-built AST.
    let mut escaped = parsed.clone();
    escaped.type_aliases[0].target = Type::Parameter(function(&parsed, 4).type_params[0].clone());
    assert_eq!(
        registry.resolve_aliases(&escaped).unwrap_err().code(),
        ErrorCode::UnknownType
    );
}

#[test]
fn generic_alias_parameters_shadow_types_without_leaking_or_capturing_names() {
    let source = "type T = Int64 type Identity<T> = T type Concrete<U> = T fn a(x: Identity<Bool>) -> Bool { x } fn b<T>(x: Concrete<T>) -> Int64 { x }";
    let resolved = TypeRegistry::new()
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    assert_eq!(function(&resolved, 0).params[0].ty, Type::Bool);
    assert_eq!(function(&resolved, 1).params[0].ty, Type::Int64);
    for source in [
        "type Identity<T> = T type Bad = T",
        "type Identity<T> = T fn f(x: T) {}",
        "type Identity<T> = T struct R { value: T }",
        "type A<T> = T type B<U> = T",
        "type A<T> = T<Int32>",
        "type T<T> = T<Int32>",
    ] {
        assert!(
            crate::vm::VmEngine::new_interpreter()
                .load_source(source)
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn generic_alias_caches_refresh_forward_owner_views_in_signatures() {
    let source = r#"
        import joky/array.{type Array}
        type View<T> = &Owner<T>
        struct First { callback: fn(View<Int32>) -> Int32 }
        struct Owner<T> { values: Array<T> }
        type Mixed<T> = fn(View<T>, &Owner<T>) -> Int32
        struct Second { callback: Mixed<Int32> }
        fn accept(callback: Mixed<Int32>) {}
    "#;
    let registry = TypeRegistry::default();
    let normalized = registry
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let again = registry.resolve_aliases(&normalized).unwrap();
    for module in [&normalized, &again] {
        let accept = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(f) if f.name == "accept" => Some(f),
                _ => None,
            })
            .unwrap();
        let Type::Function(callback) = &accept.params[0].ty else {
            panic!("expected callback")
        };
        assert_eq!(callback.signature.params.len(), 2);
        assert!(callback.signature.params.iter().all(Type::is_buffer_view));
    }
}

#[test]
fn generic_alias_cache_distinguishes_targets_and_allows_fixed_noncopy_targets() {
    let mut parsed = crate::parser::parse("type First<T> = T type Second<U> = (U,) fn a(x: First<Int32>) -> Int32 { x } fn b(x: Second<Int32>) -> (Int32,) { x }").unwrap();
    // Reusing rigid parameters in public AST declarations cannot conflate targets.
    parsed.type_aliases[1].type_params = parsed.type_aliases[0].type_params.clone();
    parsed.type_aliases[1].target = Type::tuple(vec![Type::Parameter(
        parsed.type_aliases[0].type_params[0].clone(),
    )]);
    let registry = TypeRegistry::new();
    let normalized = registry.resolve_aliases(&parsed).unwrap();
    for n in 0..2 {
        let f = function(&normalized, n);
        assert_eq!(f.params[0].ty, f.ret_ty);
    }
    assert_ne!(
        function(&normalized, 0).ret_ty,
        function(&normalized, 1).ret_ty
    );
    let module = crate::parser::parse("type Text<T> = String type Handle<T> = vm.Handle fn text() -> Text<Bool> { \"text\" } extern fn handle() -> Handle<Int32>").unwrap();
    let resolved = registry.resolve_aliases(&module).unwrap();
    assert_eq!(function(&resolved, 0).ret_ty, Type::String);
    let Item::ExternFn(f) = &resolved.items[1] else {
        panic!()
    };
    assert_eq!(f.ret_ty, Type::Named("vm.Handle".into()));
}

#[test]
fn generic_alias_import_caches_do_not_accumulate_across_resolutions() {
    let registry = TypeRegistry::new().with_prelude(false);
    let mut modules = crate::ModuleSources::new();
    modules.insert("model", "struct Box<T> { value: T } type PairOf<T> = (T, T) pub type Nested<T> = Box<PairOf<T>>").unwrap();
    let parsed = crate::parser::parse(
        "import model fn f(x: model.Nested<Int32>) -> model.Nested<Int32> { x }",
    )
    .unwrap();
    for _ in 0..3 {
        let resolved = registry.resolve_with_modules(&parsed, &modules).unwrap();
        let f = function(&resolved, 0);
        assert_eq!(f.params[0].ty, f.ret_ty);
        assert!(registry.alias_instances.borrow().is_empty());
        assert!(registry.record_instances.borrow().is_empty());
    }
}

#[test]
fn generic_alias_expansion_bounds_instances_layouts_and_shared_zero_sized_graphs() {
    let registry = TypeRegistry::new().with_prelude(false);
    let mut instances = "type Drop<T> = Unit".to_owned();
    for n in 0..257 {
        instances.push_str(&format!(" struct R{n} {{}} type A{n} = Drop<R{n}>"));
    }
    let mut wide = "type Double<T> = (T, T) type T0 = Int32".to_owned();
    for n in 1..=17 {
        wide.push_str(&format!(" type T{n} = Double<T{}>", n - 1));
    }
    let mut deep = "type Wrap<T> = ((T,),) type T0 = Unit".to_owned();
    for n in 1..=129 {
        deep.push_str(&format!(" type T{n} = Wrap<T{}>", n - 1));
    }
    for (source, message) in [
        (instances, "256 instances"),
        (wide, "65536"),
        (deep, "256 levels"),
    ] {
        let error = registry
            .resolve_aliases(&crate::parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ResourceLimit);
        assert!(error.message().contains(message), "{error}");
    }
    let mut dag = "type Double<T> = (T, T) type T0 = Unit".to_owned();
    for n in 1..=35 {
        dag.push_str(&format!(" type T{n} = Double<T{}>", n - 1));
    }
    dag.push_str(" fn id<T>(x: T) -> T { x } fn hold(x: T35) -> T35 { id(x) }");
    let parsed = crate::parser::parse(&dag).unwrap();
    let first = registry.resolve_aliases(&parsed).unwrap();
    let second = registry.resolve_aliases(&parsed).unwrap();
    let ty = &function(&first, 1).ret_ty;
    assert_eq!(*ty, function(&second, 1).ret_ty);
    assert!(ty.scalar_types().is_empty());
    assert!(ty.to_string().len() < 4096);
    crate::core::ir::ModulePlan::with_import_layouts_and_types(&first, &HashMap::new(), &registry)
        .unwrap();

    // Repeated identical applications share the budget instead of consuming it.
    let repeated = format!(
        "type PairOf<T> = (T, T) {}",
        (0..300)
            .map(|n| format!("type A{n} = PairOf<Int32> "))
            .collect::<String>()
    );
    registry
        .resolve_aliases(&crate::parser::parse(&repeated).unwrap())
        .unwrap();
}
