use super::{VmEngine, VmMode};
use crate::{aot::AotCompiler, parser, ErrorCode};

#[test]
fn default_prelude_provides_constructors_constants_and_checked_conversions() {
    let source = r#"
        fn unwrap<T>(value: Option<T>, fallback: T) -> T {
            match value { Some(n) => n, None => fallback }
        }
        fn check() -> Int32 {
            let a = Some(42)
            let b = None::<Int32>
            let c: Result<Int32, Bool> = Ok(42)
            let d: Result<Int32, Bool> = Err(false)
            let r = match c { Ok(n) => n, Err(_) => 0 }
            let negative = match (-1).to_u64() { Some(_) => 0, None => 42 }
            let failed = match d { Ok(_) => 0, Err(_) => 42 }
            if unwrap(a, 0) == 42 && unwrap(b, 42) == 42 && r == 42 &&
                failed == 42 && negative == 42 && UInt64.MAX == uint64(-1) &&
                Int32.MIN == -2147483648 { 42 } else { 0 }
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        assert!(vm.prelude_enabled());
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        vm.set_prelude_enabled(false);
        assert!(!vm.prelude_enabled());
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        assert_eq!(
            vm.load_source(source).unwrap_err().code(),
            ErrorCode::UnknownType
        );
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        vm.load_source(&format!("import joky/prelude\n{source}"))
            .unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    let mut compiler = AotCompiler::new("prelude_configuration").unwrap();
    assert!(compiler.prelude_enabled());
    compiler.set_prelude_enabled(false).unwrap();
    assert!(!compiler.prelude_enabled());
    compiler
        .compile_module(&parser::parse("enum Option { Empty } fn f() -> Int32 { 42 }").unwrap())
        .unwrap();
    assert_eq!(
        compiler.set_prelude_enabled(true).unwrap_err().code(),
        ErrorCode::InvalidState
    );
}

#[test]
fn host_module_aliases_and_selected_call_forms_keep_canonical_abi_bindings() {
    let source = r#"
        import joky/vm as engine.{type Vm as Engine, call_int32_0 as invoke, register_int32_1 as register, close}
        fn next(n: Int32) -> Int32 { n + 1 }
        fn check() -> Int32 {
            let vm = 99
            let h: Engine = Engine.new()
            let same: &engine.Vm = &h
            let name = "next"; register(same, &name, next)
            let code = "extern fn next(n: Int32) -> Int32 fn value() -> Int32 {{ next(41) }}"; engine.load_source(h, &code)
            let entry = "value"; let a = invoke(h, &entry)
            let b = engine.call_int32_0(h, &entry)
            close(h)
            a + b - 42
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        let source = if mode == VmMode::Interpreter {
            // Interpreter functions have no native entry to register in another VM.
            source.replace("register(same, &name, next)", "").replace(
                "extern fn next(n: Int32) -> Int32 fn value() -> Int32 {{ next(41) }}",
                "fn value() -> Int32 {{ 42 }}",
            )
        } else {
            source.into()
        };
        vm.load_source(&source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
}

#[test]
fn import_errors_are_located_and_do_not_publish_partial_modules() {
    for (prelude, source, code) in [
        (
            false,
            "import joky/option.{Some} fn f(x: Option<Int32>) {}",
            ErrorCode::UnknownType,
        ),
        (
            false,
            "import joky/option.{type Option} fn f() -> Option<Int32> { Some(1) }",
            ErrorCode::UnknownName,
        ),
        (
            false,
            "import joky/option as opt fn f(x: Option<Int32>) {}",
            ErrorCode::UnknownType,
        ),
        (
            false,
            "import joky/option as opt fn f() { option.Some(1) }",
            ErrorCode::UnknownName,
        ),
        (
            false,
            "import joky/option.{type Option as Maybe} fn f(x: Option<Int32>) {}",
            ErrorCode::UnknownType,
        ),
        (
            false,
            "import joky/option.{type Missing} fn f() {}",
            ErrorCode::InvalidImport,
        ),
        (
            false,
            "import joky/option.{Missing} fn f() {}",
            ErrorCode::InvalidImport,
        ),
        (
            false,
            "import joky/option.{type Option, type Option} fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option.{Some, Some} fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option.{Some as X} import joky/result.{Ok as X} fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option as m import joky/result as m fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option as Option fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "enum Option<T> { None, Some(T) } fn f() {}",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option.{Some} fn Some(n: Int32) -> Int32 { n }",
            ErrorCode::DuplicateDefinition,
        ),
        (
            true,
            "import joky/option.{Some as int32} fn f() {}",
            ErrorCode::ReservedName,
        ),
        (
            true,
            "fn f() { let Some = 1; Some(42) }",
            ErrorCode::InvalidCall,
        ),
        (
            true,
            "import joky/vm as v fn f() { let v = 1; v.create() }",
            ErrorCode::UnknownName,
        ),
        (
            true,
            "import joky/vm as v fn f() { vm.Vm.new() }",
            ErrorCode::UnknownName,
        ),
        (
            true,
            "import joky/vm.{missing} fn f() {}",
            ErrorCode::InvalidImport,
        ),
        (
            true,
            "import joky/vm.{type Missing} fn f() {}",
            ErrorCode::InvalidImport,
        ),
        (
            true,
            "import joky/vm.{close as new} fn f() { let new = 1; new() }",
            ErrorCode::InvalidCall,
        ),
        (
            true,
            "fn f(x: Option<Int32>) { match x { Ok(n) => {}, None => {} } }",
            ErrorCode::InvalidPattern,
        ),
        (
            true,
            "fn id<T>(n: T) -> T { n } fn f() { id::<Int32> }",
            ErrorCode::TypeMismatch,
        ),
        (true, "fn f() { vm.Vm.new() }", ErrorCode::UnknownName),
    ] {
        let source = format!("// 🙂\n{source}");
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(prelude);
            vm.load_source("fn check() -> Int32 { 42 }").unwrap();
            let version = vm.module_version();
            let error = vm.load_source(&source).unwrap_err();
            assert_eq!(error.code(), code, "{source}: {error}");
            assert!(
                error
                    .span()
                    .is_some_and(|span| span.slice(&source).is_some()),
                "{source}: {error}"
            );
            assert_eq!(vm.module_version(), version);
            assert_eq!(vm.call_int32_0("check").unwrap(), 42);
        }
        let mut compiler = AotCompiler::new("invalid_import").unwrap();
        compiler.set_prelude_enabled(prelude).unwrap();
        let error = compiler
            .compile_module(&parser::parse(&source).unwrap())
            .unwrap_err();
        assert_eq!(error.code(), code, "{source}: {error}");
    }
}

#[test]
fn private_standard_dependencies_cannot_capture_local_types_or_functions() {
    let source = r#"
        import joky/integer as ints.{type Option as Maybe}
        import joky/option.{Some as Present}
        enum Option { Value(Int32) }
        fn local(n: Int32) -> Int32 { n + 1 }
        fn check() -> Int32 {
            let local = 41
            let own = Option.Value(local(local))
            let checked: Maybe<UInt64> = (42).to_u64()
            let present = Present(42)
            let a = match own { Value(n) => n }
            let b = match checked { ints.Some(n) => int32(n), ints.None => 0 }
            let c = match present { Present(n) => n, option.Option.None => 0 }
            a + b + c - 84
        }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(source).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42);
    }
    for source in [
        "import joky/integer as ints fn f(x: Option<Int32>) {}",
        "import joky/prelude as p fn f(x: Option<Int32>) {}",
        "import joky/option as opt fn f() -> opt.Option<Bool> { opt.Option::<Int32>.None }",
        "import joky/option as opt.{type Option as Maybe} fn f<opt>() -> Maybe<Int32> { opt.Option.None }",
        "import joky/option as _ fn f() {}",
        "import joky/option.{Some as _} fn f() {}",
    ] {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let mut vm = VmEngine::with_mode(mode);
            vm.set_prelude_enabled(false);
            let error = vm.load_source(source).unwrap_err();
            assert!(error.span().is_some(), "{source}: {error}");
        }
    }
}
