//! Public API boundaries apply equally to source libraries and compiler adapters.
use super::*;

#[test]
fn standard_library_helpers_and_internal_modules_are_not_exported() {
    let library = StandardLibrary::new();
    for (path, names) in [
        ("joky/assertions", &["quoted", "hex_digit"][..]),
        ("joky/log", &["rank", "add"]),
        (
            "joky/fs",
            &["error", "error_kind", "read_remaining", "decode_text"],
        ),
        (
            "joky/http",
            &["token", "header", "read_request", "exchange"],
        ),
        ("joky/tls", &["failure", "handshake", "client_config"]),
        ("joky/shutdown", &["reason"]),
        (
            "joky/postgres",
            &[
                "__sql_text",
                "__sql_identifier",
                "__sql_parameter",
                "rows",
                "encode",
            ],
        ),
        ("joky/task", &["__isolate_success", "__isolate_failure"]),
        (
            "joky/float",
            &["float64_sqrt", "float32_sqrt", "i8_from_valid_f64"],
        ),
        ("joky/arena", &["new", "allocate"]),
    ] {
        for name in names {
            assert!(
                !library.modules[path].exports.functions.contains_key(*name),
                "{path}.{name}"
            );
        }
    }
    for (path, name) in [
        ("joky/postgres", "SqlBuilder"),
        ("joky/http", "Wire"),
        ("joky/http", "ReadFailure"),
        ("joky/http", "ReplyHead"),
    ] {
        assert!(
            !library.modules[path].exports.types.contains_key(name),
            "{path}.{name}"
        );
    }
    for (path, name) in [
        ("joky/fs", "IO_CHUNK_SIZE"),
        ("joky/tls", "RECORD"),
        ("joky/tls", "MAX_IO"),
    ] {
        assert!(
            !library.modules[path].exports.constants.contains_key(name),
            "{path}.{name}"
        );
    }
    for (path, module) in &library.modules {
        if path.contains("/internal/") {
            assert!(module.private, "{path}");
        }
        // Even when a private implementation changes, it must not silently
        // become an export just because it belongs to the standard library.
        for item in &module.module.items {
            if let Item::FnDef(function) = item {
                if !function.public {
                    assert!(
                        !module
                            .exports
                            .functions
                            .values()
                            .any(|name| name == &function.name),
                        "{}",
                        function.name
                    );
                }
            }
        }
    }
    let postgres = &library.modules["joky/postgres"];
    assert!(postgres.compiler_exports.types.contains_key("SqlBuilder"));
    assert_eq!(postgres.compiler_exports.functions.len(), 3);
    assert_eq!(postgres.compiler_exports.members.len(), 2);
}

#[test]
fn standard_library_private_entries_reject_user_access() {
    let checker = crate::Checker::new();
    for source in [
        "import joky/assertions fn f() { assertions.hex_digit(0); }",
        "import joky/assertions.{quoted} fn f() {}",
        "import joky/log fn f() { log.rank(log.Level.Info); }",
        "import joky/fs fn f() -> UInt64 { fs.IO_CHUNK_SIZE }",
        "import joky/http.{type Wire} fn f() {}",
        "import joky/http.{type ReadFailure} fn f() {}",
        "import joky/http fn f(c: &http.Client) @suspends { c.open(); }",
        "import joky/tls fn f() -> UInt64 { tls.MAX_IO }",
        "import joky/tls fn f(s: &mut tls.TlsStream) @suspends { s.flush(); }",
        "import joky/shutdown fn f() { shutdown.reason(0); }",
        "import joky/postgres.{type SqlBuilder} fn f() {}",
        "import joky/postgres.{__sql_text} fn f() {}",
        "import joky/task fn f() { task.__isolate_success(1); }",
        "import joky/float fn f() { float.float64_sqrt(4.0); }",
        "import joky/arena fn f() { arena.new(); }",
        "import joky/internal/text fn f() {}",
        "import joky/internal/resolve fn f() {}",
    ] {
        let error = checker
            .check_module(&crate::parser::parse(source).unwrap())
            .unwrap_err();
        assert!(
            matches!(
                error.code(),
                ErrorCode::UnknownName | ErrorCode::UnknownType | ErrorCode::InvalidImport
            ),
            "{source}: {error}"
        );
    }
}

#[test]
fn explicit_standard_library_exports_execute_without_prelude() {
    let source = r#"
        import joky/option.{type Option, Some, None}
        import joky/result.{type Result, Ok, Err}
        import joky/float
        import joky/arena.{type Arena}
        import joky/assertions.{assert_eq, assert_ne}
        import joky/check.{assert}
        fn test() -> Int32 {
            let option = Some(20).map(|n| { n + 1 })
            let result: Result<Int32, Int32> = option.ok_or(0)
            let value = result.map(|n| { n * 2 }).unwrap()
            let arena = Arena.new()
            let stored = arena.alloc(value)
            assert_eq(*stored, 42)
            assert_ne("left", "right")
            assert_eq((4.0).sqrt(), 2.0)
            assert(Float64.INFINITY.is_finite() == false, "float constants")
            *stored
        }
    "#;
    for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
        let mut vm = crate::vm::VmEngine::with_mode(mode);
        vm.set_prelude_enabled(false);
        vm.load_source(source)
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert_eq!(vm.call_int32_0("test").unwrap(), 42);
    }
    #[cfg(unix)]
    {
        let types = crate::stdlib::module_registry()
            .types()
            .clone()
            .with_prelude(false);
        let ast = types
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        crate::backends::aot::tests::assert_sync_int32(&ast, "test", 42);
    }
}
