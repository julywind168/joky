use super::{VmEngine, VmMode};

#[test]
fn removed_drop_module_reports_migration_and_preserves_loaded_code() {
    let source = "import joky/drop.{type Drop} struct R { text: String } impl Drop for R { fn drop(&mut self) {} }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source("fn check() -> Int32 { 42 }").unwrap();
        let error = vm.load_source(source).unwrap_err();
        assert_eq!(error.code(), crate::ErrorCode::InvalidImport);
        assert!(error.message().contains("joky/drop has been removed"));
        assert!(error.span().is_some());
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    let mut compiler = crate::aot::AotCompiler::new("removed-drop").unwrap();
    let error = compiler
        .compile_module(&crate::parser::parse(source).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::InvalidImport);
    assert!(error.message().contains("joky/drop has been removed"));
}

#[test]
fn user_trait_named_drop_remains_ordinary() {
    let source = "trait Drop { fn drop(&self) -> Int32 } struct R { id: Int32 } impl Drop for R { fn drop(&self) -> Int32 { self.id } } fn check() -> Int32 { let r = R { id: 42 }; r.drop() }";
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn ordinary_trait_methods_do_not_change_layout_or_run_during_cleanup() {
    let source = r#"
        trait Drop { fn drop(&mut self) }
        struct Wrapper<T> { value: T }
        impl<T> Drop for Wrapper<T> { fn drop(&mut self) { 1 / 0; } }
        struct Pair { first: Wrapper<String>, second: String }
        fn copied<T: Copy>(value: T) -> T { value }
        fn check() -> Int32 {
            let value = Wrapper { value: 21 }
            let duplicate = copied(value)
            let owner = Pair { first: Wrapper { value: "owned" }, second: "other" }
            let Pair { first: Wrapper { value: text }, .. } = owner
            if text == "owned" { duplicate.value + value.value } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source(source)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    #[cfg(unix)]
    crate::backends::aot::tests::assert_sync_int32(
        &crate::parser::parse(source).unwrap(),
        "check",
        42,
    );
}
