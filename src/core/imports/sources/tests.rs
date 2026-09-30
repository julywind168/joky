use super::*;
use crate::{
    backends::{interp, jit},
    core::{ir::ModulePlan, resolver::BindingRegistry},
    runtime::suspension::ModuleId,
};

#[test]
fn array_exports_and_storage_privileges_follow_compiler_definitions() {
    let library = StandardLibrary::new();
    let array = &library.modules["joky/array"];
    assert_eq!(
        array
            .exports
            .types
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Array"]
    );
    assert_eq!(
        array
            .exports
            .functions
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "borrow",
            "borrow_mut",
            "capacity",
            "get",
            "is_empty",
            "len",
            "new",
            "pop",
            "push",
            "reserve",
            "set",
            "slice",
            "slice_mut",
            "with_capacity"
        ]
    );
    assert_eq!(array.implicit, ImplicitImports::None);
    assert!(array.exports.constructors.is_empty());
    assert!(library.modules["joky/internal/buffer"].private);
    for (path, name, functions) in [
        ("joky/element", "Element", vec!["read", "write"]),
        (
            "joky/slice",
            "Slice",
            vec![
                "borrow",
                "borrow_mut",
                "get",
                "is_empty",
                "len",
                "set",
                "slice",
                "slice_mut",
            ],
        ),
    ] {
        let module = &library.modules[path];
        assert_eq!(
            module
                .exports
                .types
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [name]
        );
        assert_eq!(
            module
                .exports
                .functions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            functions
        );
        assert!(module.exports.constructors.is_empty());
        assert_eq!(module.implicit, ImplicitImports::None);
    }

    // Reusing a trusted module path does not give an ordinary source definition
    // the private import permission or the compiler Buffer/intrinsic identities.
    let storage = || SourceDefinition {
        kind: SourceKind::BufferStorage,
        path: "joky/internal/buffer",
        source: include_str!("../../../stdlib/internal/buffer.jk"),
        reexport_imports: false,
        implicit: ImplicitImports::None,
    };
    for path in ["joky/array", "joky/element", "joky/slice"] {
        let error = StandardLibrary::from_sources(&[
            storage(),
            definition(
                path,
                "import joky/internal/buffer as b fn value() -> UInt64 { b.capacity_limit() }",
            ),
        ])
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidImport);
    }
    let fake = StandardLibrary::from_sources(&[definition(
        "joky/internal/buffer",
        "type Buffer<T> = T fn allocate() -> Int32 { 42 }",
    )])
    .unwrap();
    let Item::FnDef(f) = &fake.modules["joky/internal/buffer"].module.items[0] else {
        unreachable!()
    };
    assert!(f.buffer_intrinsic.is_none());
    assert!(!fake.modules["joky/internal/buffer"].private);
}

#[test]
fn embedded_runtime_diagnostics_survive_reload_without_production_traps() {
    use crate::{
        runtime::{cps::CpsFrame, suspension::ResumeError},
        vm::{VmEngine, VmMode},
        FunctionSignature,
    };

    unsafe extern "C" fn pause(frame: *mut CpsFrame) -> i32 {
        let frame = &mut *frame;
        if frame.state < 2 {
            frame.state += 1;
            1
        } else {
            0
        }
    }

    const LIBRARY: &str = "// Test-only embedded source.\nfn boom<T>(value: T) -> Int32 { 1 / 0 }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        for suspends in [false, true] {
            let types = registry(&[definition("joky/integer", LIBRARY)]);
            let source = if suspends {
                "import joky/integer as ints extern fn pause() @suspends fn check() @suspends -> Int32 { pause(); ints.boom(42) }"
            } else {
                "import joky/integer as ints fn check() -> Int32 { ints.boom(42) }"
            };
            let module = types
                .resolve_aliases(&crate::parser::parse(source).unwrap())
                .unwrap();
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            unsafe {
                vm.register_suspending_fn(
                    "pause",
                    FunctionSignature::new(vec![], Type::Unit),
                    pause,
                )
                .unwrap();
            }
            vm.load_module(&module).unwrap();
            drop(module);
            drop(types);
            let error = if suspends {
                let mut task = vm.call_async("check", &[]).unwrap();
                assert!(!task.is_finished());
                vm.load_source("fn replacement() -> Int32 { 42 }").unwrap();
                drop(vm);
                task.resume().unwrap();
                assert!(!task.is_finished());
                let ResumeError::Failed(error) = task.resume().unwrap_err() else {
                    panic!("expected the embedded division failure");
                };
                assert_eq!(task.cps_result(), None);
                error
            } else {
                let error = vm.call_int32_0("check").unwrap_err();
                vm.load_source("fn replacement() -> Int32 { 42 }").unwrap();
                drop(vm);
                error
            };
            assert_eq!(error.code(), ErrorCode::RuntimeError);
            assert_eq!(error.message(), "Division by zero");
            let span = error.span().unwrap();
            let file = error.sources().get(span.source_id()).unwrap();
            assert_eq!(file.name(), "joky/integer.jk");
            assert_eq!(file.text(), LIBRARY);
            assert_eq!(span.slice(file.text()), Some("1 / 0"));
            if mode == VmMode::Interpreter {
                assert!(error
                    .runtime_trace()
                    .iter()
                    .any(|frame| { frame.span.source_id() == span.source_id() }));
            }
        }
    }
}

#[test]
fn integer_library_does_not_export_diagnostic_probes() {
    let library = StandardLibrary::new();
    assert!(!library.modules["joky/integer"]
        .exports
        .functions
        .contains_key("boom"));
    let types = TypeRegistry::new().with_prelude(false);
    let error = types
        .resolve_aliases(
            &crate::parser::parse("import joky/integer.{boom} fn check() -> Int32 { boom(42) }")
                .unwrap(),
        )
        .unwrap_err();
    assert!(error.message().contains("boom"));
}

fn definition(path: &'static str, source: &'static str) -> SourceDefinition {
    SourceDefinition {
        kind: SourceKind::Embedded,
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
fn cached_modules_keep_identity_scopes_and_source_ids() {
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
    assert_eq!(again.implementations.len(), 5);
    let first_method = &first.implementations[0].methods[0];
    let second_method = &second.implementations[0].methods[0];
    assert!(Arc::ptr_eq(
        first_method.scope.as_ref().unwrap(),
        second_method.scope.as_ref().unwrap()
    ));
    // Embedded declarations keep their stdlib source identity, identical
    // across consumer compilations and never attributed to the import site.
    assert_eq!(first_method.body.span(), second_method.body.span());
    assert_ne!(first_method.body.span(), Span::empty(0));
    let file = first
        .sources
        .get(first_method.body.span().source_id())
        .unwrap();
    assert_eq!(file.name(), "joky/integer.jk");
    assert_eq!(file.text(), include_str!("../../../stdlib/integer.jk"));
    let option = second
        .sources
        .get(second.enums[0].span.source_id())
        .unwrap();
    assert_eq!(option.name(), "joky/option.jk");
    let cache = types.standard.get().unwrap();
    assert_eq!(cache.modules.len(), SOURCES.len());
    let integer = &cache.modules["joky/integer"];
    assert_eq!(integer.dependencies, ["joky/option"]);
    let result = &cache.modules["joky/result"];
    assert_eq!(result.dependencies, ["joky/option"]);
    assert!(!result.exports.types.contains_key("Option"));
    assert!(!result.exports.constructors.contains_key("Some"));
    assert!(!result.exports.constructors.contains_key("None"));
    assert!(cache.modules["joky/option"].dependencies.is_empty());
    assert_ne!(
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
