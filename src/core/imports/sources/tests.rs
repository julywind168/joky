use super::*;
use crate::{
    backends::{interp, jit},
    core::{ir::ModulePlan, resolver::BindingRegistry},
    runtime::suspension::ModuleId,
};

fn definition(path: &'static str, source: &'static str) -> SourceDefinition {
    SourceDefinition {
        path,
        source,
        reexport_imports: true,
        implicit: ImplicitImports::None,
    }
}

fn registry(sources: &[SourceDefinition]) -> TypeRegistry {
    let registry = TypeRegistry::new().with_prelude(false);
    registry
        .standard
        .set(StandardLibrary::from_sources(sources).unwrap())
        .unwrap();
    registry
}

#[test]
fn cached_modules_keep_identity_scopes_and_per_import_spans() {
    let types = TypeRegistry::new().with_prelude(false);
    let source = "// first\nimport joky/integer fn check() -> Int32 { 42 }";
    let first = types
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let other_source =
        "import joky/integer import joky/option.{type Option as Maybe} fn check() -> Int32 { 42 }";
    let second = types
        .clone()
        .resolve_aliases(&crate::parser::parse(other_source).unwrap())
        .unwrap();
    let again = types.resolve_aliases(&second).unwrap();
    assert_eq!(first.enums[0].id, second.enums[0].id);
    assert_eq!(second.enums.len(), 1);
    assert_eq!(again.implementations.len(), 4);
    let first_method = &first.implementations[0].methods[0];
    let second_method = &second.implementations[0].methods[0];
    assert!(Arc::ptr_eq(
        first_method.scope.as_ref().unwrap(),
        second_method.scope.as_ref().unwrap()
    ));
    assert_eq!(
        first_method.body.span().slice(source),
        Some("import joky/integer")
    );
    assert_eq!(
        second_method.body.span().slice(other_source),
        Some("import joky/integer")
    );
    assert_eq!(
        second.enums[0].span.slice(other_source),
        Some("import joky/option.{type Option as Maybe}")
    );
    let cache = types.standard.get().unwrap();
    assert_eq!(cache.modules.len(), SOURCES.len());
    let integer = &cache.modules["joky/integer"];
    assert_eq!(integer.dependencies, ["joky/option"]);
    assert_eq!(
        integer.module.implementations[0].methods[0].body.span(),
        Span::empty(0)
    );
    assert!(!integer.module.implementations[0].methods[0]
        .scope
        .as_ref()
        .unwrap()
        .imports
        .types
        .contains_key("Result"));
}

#[test]
fn prelude_exports_selected_names_without_leaking_dependency_aliases() {
    let library = StandardLibrary::new();
    let prelude = &library.modules[PRELUDE];
    assert_eq!(
        prelude
            .exports
            .types
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Option", "Result"]
    );
    assert_eq!(
        prelude
            .exports
            .constructors
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Err", "None", "Ok", "Some"]
    );
    assert!(prelude.exports.modules.is_empty());
    assert!(prelude.module.implementations.is_empty());
    let integer = &library.modules["joky/integer"];
    let option = &library.modules["joky/option"];
    assert_eq!(
        integer.exports.types["Option"].declaration,
        option.exports.types["Option"].declaration
    );
    assert_eq!(
        prelude.exports.types["Option"].declaration,
        option.exports.types["Option"].declaration
    );
}

#[test]
fn dependency_graph_rejects_cycles_missing_modules_and_implicit_prelude() {
    for (sources, message) in [
        (
            vec![
                definition("joky/option", "import joky/integer"),
                definition("joky/integer", "import joky/option"),
            ],
            "Cyclic source module dependency",
        ),
        (
            vec![definition("joky/option", "import joky/missing")],
            "Unknown embedded source module",
        ),
    ] {
        let error = StandardLibrary::from_sources(&sources).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidImport);
        assert!(error.message().contains(message), "{error}");
    }
    let error = StandardLibrary::from_sources(&[
        definition("joky/option", include_str!("../../../stdlib/option.jk")),
        definition(
            "joky/integer",
            "impl Int32 { fn checked(self) -> Option<Int32> { Some(self) } }",
        ),
    ])
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::UnknownType);
}

#[test]
fn source_functions_aliases_and_methods_keep_their_definition_namespace() {
    let types = registry(&[
        // Deliberately list the consumer first to exercise dependency ordering.
        definition(
            "joky/integer",
            r#"
            import joky/option as opt.{type Choice as Answer, Value as Present, identity as copy}
            fn helper(n: Int32) -> Answer { Present(copy(n)) }
            fn shadow(Present: Int32) -> Int32 { Present }
            impl Int32 {
                const BASE: Int32 = 42
                fn checked(self) -> Answer { helper(self) }
            }
        "#,
        ),
        definition(
            "joky/option",
            r#"
            enum Choice { Empty Value(Int32) }
            fn identity<T>(value: T) -> T { value }
        "#,
        ),
    ]);
    let source = r#"
        import joky/integer as ints.{helper as make}
        enum Answer { Wrong }
        fn helper(n: Int32) -> Int32 { 0 }
        fn copy(n: Int32) -> Int32 { 0 }
        fn Present(n: Int32) -> Int32 { 0 }
        fn check() -> Int32 {
            let a: ints.Answer = make(21)
            let b = (21).checked()
            let x = match a { | ints.Present(n) => n | ints.Answer.Empty => 0 }
            let y = match b { | ints.Present(n) => n | ints.Answer.Empty => 0 }
            if ints.copy(true) && ints.shadow(42) == Int32.BASE { x + y } else { 0 }
        }
    "#;
    let module = types
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let interpreted = interp::compile_with_types(
        &module,
        &HashMap::new(),
        HashMap::new(),
        ModuleId(0),
        &types,
    )
    .unwrap();
    assert!(matches!(
        interp::call_sync(&interpreted, "check", &[], Default::default()).unwrap(),
        interp::Value::Int32(42)
    ));
    let native = jit::compile_with_registry(
        &module,
        &[],
        &HashMap::new(),
        vec![],
        ModuleId(0),
        &BindingRegistry::default(),
        &types,
    )
    .unwrap();
    assert_eq!(native.exports.len(), 4);
    let entry: unsafe extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(native.exports["check"].entry) };
    assert_eq!(unsafe { entry() }, 42);
}

#[test]
fn importing_modules_cannot_supply_missing_library_functions_or_members() {
    for library in [
        "fn value() -> Int32 { caller_only() }",
        "fn value() -> Int32 { (1).caller_only() }",
        "fn value() -> Int32 { Int32.CALLER }",
        "impl Int32 { const LIBRARY: Int32 = Int32.CALLER } fn value() -> Int32 { Int32.LIBRARY }",
    ] {
        let types = registry(&[definition("joky/integer", library)]);
        let source = r#"
            import joky/integer as ints
            fn caller_only() -> Int32 { 42 }
            impl Int32 {
                const CALLER: Int32 = 42
                fn caller_only(self) -> Int32 { 42 }
            }
            fn check() -> Int32 { ints.value() }
        "#;
        let module = types
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        let error = ModulePlan::with_import_layouts_and_types(&module, &HashMap::new(), &types)
            .err()
            .unwrap();
        assert_eq!(error.code(), ErrorCode::UnknownName, "{library}: {error}");
    }
}
