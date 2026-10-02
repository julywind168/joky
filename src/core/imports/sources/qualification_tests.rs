use super::*;
use crate::vm::{VmEngine, VmMode};
const GUARD: &str =
    "import joky/internal/types as types pub fn guard<T>() { types.require_plain_copy::<T>() }";
fn qualified_registry() -> TypeRegistry {
    qualified_registry_with(GUARD)
}
fn qualified_registry_with(guard: &'static str) -> TypeRegistry {
    let mut definitions: Vec<_> = SOURCES
        .iter()
        .map(|s| SourceDefinition {
            kind: s.kind,
            path: s.path,
            source: s.source,
            reexport_imports: s.reexport_imports,
            implicit: s.implicit,
        })
        .collect();
    definitions.push(SourceDefinition {
        kind: SourceKind::BufferLibrary,
        path: "joky/test/qualification",
        source: guard,
        reexport_imports: false,
        implicit: ImplicitImports::None,
    });
    let registry = TypeRegistry::new();
    registry
        .standard
        .set(StandardLibrary::from_sources(&definitions).unwrap())
        .unwrap();
    registry
}
#[test]
fn plain_copy_qualification_executes_legal_layouts_on_both_vms() {
    let source = r#"
        import joky/test/qualification as q
        struct Key { number: Int64 flag: Bool }
        enum Choice { Empty Full(UInt64) }
        type Alias = Key
        fn test() -> Int32 {
            q.guard::<Int32>() q.guard::<Int64>() q.guard::<UInt32>() q.guard::<UInt64>()
            q.guard::<Bool>() q.guard::<Unit>() q.guard::<Key>() q.guard::<Alias>()
            q.guard::<Choice>() q.guard::<(Int32, Bool)>()
            42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let registry = qualified_registry();
        let ast = registry
            .resolve_with_modules(
                &crate::parser::parse(source).unwrap(),
                &crate::ModuleSources::new(),
            )
            .unwrap();
        #[cfg(unix)]
        if mode == VmMode::Jit {
            crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
        }
        let mut vm = VmEngine::with_mode(mode);
        vm.load_module(&ast).unwrap();
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
}
#[test]
fn plain_copy_qualification_rejects_actual_owners_references_strings_and_aliases() {
    let prefix = "import joky/test/qualification as q import joky/array.{type Array}";
    for (declaration, ty) in [
        ("", "&Int32"),
        ("struct Key { value: &Int32 }", "Key"),
        (
            "struct Inner { value: &Int32 } struct Key { nested: Inner }",
            "Key",
        ),
        ("enum Key { Empty Full(&Int32) }", "Key"),
        ("", "(&Int32, Int32)"),
        ("struct Key { values: Array<Int32> }", "Key"),
        ("", "Array<Int32>"),
        ("", "String"),
        ("type Key = &Int32", "Key"),
        ("struct Inner { value: &Int32 } type Key = Inner", "Key"),
    ] {
        let source = format!("{prefix} {declaration} fn bad() {{ q.guard::<{ty}>() }}");
        let registry = qualified_registry();
        let ast = registry
            .resolve_with_modules(
                &crate::parser::parse(&source).unwrap(),
                &crate::ModuleSources::new(),
            )
            .unwrap();
        let message = if ty == "String" {
            "Copy type or owning record"
        } else {
            "reference-free Copy storage value"
        };
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let e = VmEngine::with_mode(mode).load_module(&ast).unwrap_err();
            assert_eq!(e.code(), ErrorCode::TypeMismatch);
            assert!(e.message().contains(message), "{source}: {e}");
            assert!(e.span().is_some());
        }
        let e = crate::aot::AotCompiler::new("invalid-key-qualification")
            .unwrap()
            .compile_module(&ast)
            .unwrap_err();
        assert!(e.message().contains(message), "{source}: {e}");
    }
}
#[test]
fn plain_copy_qualification_follows_cross_module_concrete_arguments() {
    let mut modules = crate::ModuleSources::new();
    modules.insert("keys", "import joky/test/qualification as q pub struct Key { value: &Int32 } pub fn check<T>() { q.guard::<T>() }").unwrap();
    let registry = qualified_registry();
    let source = "import keys fn bad() { keys.check::<keys.Key>() }";
    let ast = registry
        .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
        .unwrap();
    assert!(VmEngine::new()
        .load_module(&ast)
        .unwrap_err()
        .message()
        .contains("reference-free Copy storage value"));
    let fake =
        "fn require_plain_copy<T>() {} fn test() -> Int32 { require_plain_copy::<&Int32>() 42 }";
    let mut vm = VmEngine::new();
    vm.load_source(fake).unwrap();
    assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    assert_eq!(
        vm.load_source("import joky/internal/types fn bad() {}")
            .unwrap_err()
            .code(),
        ErrorCode::InvalidImport
    );
}

#[test]
fn plain_copy_qualification_checks_fixed_types_in_unused_generic_bodies() {
    let registry = qualified_registry_with("import joky/internal/types as types pub fn bad<T>() { types.require_plain_copy::<&Int32>() } pub fn good() -> Int32 { 42 }");
    let source = "import joky/test/qualification fn test() -> Int32 { qualification.good() }";
    let ast = registry
        .resolve_with_modules(
            &crate::parser::parse(source).unwrap(),
            &crate::ModuleSources::new(),
        )
        .unwrap();
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        assert!(VmEngine::with_mode(mode)
            .load_module(&ast)
            .unwrap_err()
            .message()
            .contains("reference-free Copy storage value"));
    }
    assert!(crate::aot::AotCompiler::new("unused-invalid-qualification")
        .unwrap()
        .compile_module(&ast)
        .unwrap_err()
        .message()
        .contains("reference-free Copy storage value"));
}
