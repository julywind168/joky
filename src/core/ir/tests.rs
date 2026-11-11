use super::ModulePlan;
use crate::{ErrorCode, TypeRegistry};
use std::collections::HashMap;

#[test]
fn code_dependencies_execute_indirect_methods_and_cleanup_in_all_backends() {
    use crate::vm::{VmEngine, VmMode};

    let mut modules = crate::ModuleSources::new();
    modules
        .insert(
            "dependencies",
            r#"
        fn increment(value: Int32) -> Int32 { value + 1 }
        pub fn callback() -> fn(Int32) -> Int32 { increment }
        pub trait Read { fn read(&self) @noescape -> Int32 }
        pub struct Value { number: Int32 }
        impl Read for Value { fn read(&self) -> Int32 { self.number } }
        pub fn value() -> Value { Value { number: 41 } }
        pub struct Owner { text: String, divisor: Int32 }
        pub fn cleanup(divisor: Int32) { let checked = 42 / divisor }

        pub fn owner(divisor: Int32) -> Owner { Owner { text: "cleanup", divisor: divisor } }
    "#,
        )
        .unwrap();
    let source = r#"
        import dependencies as d.{type Read}
        fn check() -> Int32 {
            let owner = d.owner(1)
            let value = d.value()
            let view: &dyn Read = &value
            let callback = d.callback()
            callback(view.read())
        }
        fn failure() -> Int32 { let owner = d.owner(0); d.cleanup(0); 42 }
    "#;
    for mode in [VmMode::Jit, VmMode::Interpreter] {
        let mut vm = VmEngine::with_mode(mode);
        vm.load_source_with_modules(source, &modules).unwrap();
        assert_eq!(vm.call_int32_0("check").unwrap(), 42, "{mode:?}");
        let error = vm.call_int32_0("failure").unwrap_err();
        assert_eq!(error.code(), ErrorCode::RuntimeError, "{mode:?}: {error}");
        assert!(error.message().contains("zero"), "{mode:?}: {error}");
        assert_eq!(vm.call_int32_0("check").unwrap(), 42, "{mode:?}");
        crate::runtime::arena::LIVE.with(|live| assert_eq!(live.get(), 0));
    }
    #[cfg(unix)]
    {
        let ast = TypeRegistry::default()
            .resolve_with_modules(&crate::parser::parse(source).unwrap(), &modules)
            .unwrap();
        crate::aot::tests::assert_sync_results(
            &ast,
            &[("check", Some(42)), ("failure", None), ("check", Some(42))],
        );
    }
}

#[test]
fn failure_effects_propagate_through_recursion_without_marking_pure_loops() {
    let ast = crate::parser::parse(
        r#"
        extern fn host() -> Int32
        fn first(x: Int32) -> Int32 { second(x) }
        fn second(x: Int32) -> Int32 { if x == 0 { 7 / x } else { first(x - 1) } }
        fn pure(x: Int32) -> Int32 { var i = 0; while i < x { i = i + 1 }; i }
        fn pure_call(x: Int32) -> Int32 { pure(x) }
        fn bits(x: Int32) -> Int32 { ~(x & 3) | x ^ 1 }
        fn shift(x: Int32) -> Int32 { 1 << x }
        fn shift_call(x: Int32) -> Int32 { shift(x) }
        fn convert(x: UInt32) -> Int64 { int64(x) }
        fn convert_call(x: UInt32) -> Int64 { convert(x) }
        fn convert_failure(x: Int32) -> UInt64 { uint64(first(x)) }
        fn convert_pending(x: UInt32) @suspends -> Int64 { int64(x) }
        fn external() -> Int32 { host() }
        fn branch_task() @suspends { external(); }
        fn parent() @suspends { join { branch_task() } }
    "#,
    )
    .unwrap();
    let plan =
        ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &TypeRegistry::default())
            .unwrap();
    for name in [
        "host",
        "first",
        "second",
        "external",
        "branch_task",
        "parent",
        "shift",
        "shift_call",
        "convert_failure",
    ] {
        assert!(plan.functions[name].may_fail, "{name}");
    }
    for name in [
        "pure",
        "pure_call",
        "bits",
        "convert",
        "convert_call",
        "convert_pending",
    ] {
        assert!(!plan.functions[name].may_fail, "{name}");
    }
    assert_eq!(plan.bodies["convert_pending"].state_count, 1);
    assert!(plan.bodies["convert"].ops.iter().any(|op| matches!(op, super::Op::Convert(dst, src) if dst.ty == crate::Type::Int64 && src.ty == crate::Type::UInt32)));
    for name in [
        "int32",
        "int64",
        "uint32",
        "uint64",
        "joky_runtime_error_code",
        "__joky_runtime_division_by_zero",
        "__joky_runtime_invalid_shift",
        "__joky_native_stack_room_v1",
        "__joky_float_env_enter",
    ] {
        let ast = crate::parser::parse(&format!("fn {name}() {{}}")).unwrap();
        let error = ModulePlan::with_import_layouts_and_types(
            &ast,
            &HashMap::new(),
            &TypeRegistry::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code(), ErrorCode::ReservedName);
    }
}

#[test]
fn reject_invalid_assignment_loop_and_comparison_semantics() {
    use crate::ErrorPhase;
    for attribute in ["", "@suspends"] {
        for (body, code, marked) in [
            ("let x = 1; x = 2", ErrorCode::InvalidAssignment, "x = 2"),
            ("p = 2", ErrorCode::InvalidAssignment, "p = 2"),
            ("x = 2", ErrorCode::UnknownName, "x = 2"),
            ("var x = 1; x = true", ErrorCode::TypeMismatch, "x = true"),
            (
                "var x: Bool = 1",
                ErrorCode::TypeMismatch,
                "var x: Bool = 1",
            ),
            ("var x = 1; x = {}", ErrorCode::TypeMismatch, "x = {}"),
            ("var x = {}; x = 1", ErrorCode::TypeMismatch, "x = 1"),
            (
                "var x = 1; { let x = 2; x = 3 }",
                ErrorCode::InvalidAssignment,
                "x = 3",
            ),
            ("while 1 {}", ErrorCode::TypeMismatch, "1"),
            ("while {} {}", ErrorCode::TypeMismatch, "{}"),
            ("while (true;) {}", ErrorCode::TypeMismatch, "(true;)"),
            ("while (1; 2) {}", ErrorCode::TypeMismatch, "(1; 2)"),
            (
                "let x: Int32 = (1;);",
                ErrorCode::TypeMismatch,
                "let x: Int32 = (1;)",
            ),
            ("let x = 1; x = 2;", ErrorCode::InvalidAssignment, "x = 2"),
            ("var x = 1; x = (2;);", ErrorCode::TypeMismatch, "x = (2;)"),
            ("{ var x = 1; }; x", ErrorCode::UnknownName, "x"),
            (
                "while (var x = 1; false) { x; }",
                ErrorCode::UnknownName,
                "x",
            ),
            ("while false { var x = 1 }; x", ErrorCode::UnknownName, "x"),
            (
                "while { var x = 1; false } { x }",
                ErrorCode::UnknownName,
                "x",
            ),
            (
                "while false { 1 + true }",
                ErrorCode::TypeMismatch,
                "1 + true",
            ),
            ("true < 1", ErrorCode::TypeMismatch, "true < 1"),
            ("true == 1", ErrorCode::TypeMismatch, "true == 1"),
            ("1 != false", ErrorCode::TypeMismatch, "1 != false"),
            ("true < false", ErrorCode::TypeMismatch, "true < false"),
            ("true + false", ErrorCode::TypeMismatch, "true + false"),
            ("-true", ErrorCode::TypeMismatch, "-true"),
            ("-{}", ErrorCode::TypeMismatch, "-{}"),
            ("-\"text\"", ErrorCode::TypeMismatch, "-\"text\""),
            ("!1", ErrorCode::TypeMismatch, "!1"),
            ("!{}", ErrorCode::TypeMismatch, "!{}"),
            ("!\"text\"", ErrorCode::TypeMismatch, "!\"text\""),
            ("if 1 {}", ErrorCode::TypeMismatch, "1"),
            ("if (true;) {}", ErrorCode::TypeMismatch, "(true;)"),
            ("if true { 1 }", ErrorCode::TypeMismatch, "{ 1 }"),
            (
                "if true { 1 } else { false }",
                ErrorCode::TypeMismatch,
                "if true { 1 } else { false }",
            ),
            (
                "if false {} else { 1 }",
                ErrorCode::TypeMismatch,
                "if false {} else { 1 }",
            ),
            (
                "if true {} else { missing }",
                ErrorCode::UnknownName,
                "missing",
            ),
            (
                "if true {} else if false { 1 }",
                ErrorCode::TypeMismatch,
                "{ 1 }",
            ),
            (
                "if (let local = true; local) { local }",
                ErrorCode::UnknownName,
                "local",
            ),
            (
                "if (let local = true; local) {} else { local }",
                ErrorCode::UnknownName,
                "local",
            ),
            (
                "if true { let local = 1 }; local",
                ErrorCode::UnknownName,
                "local",
            ),
            (
                "if true { let local = 1 } else { local }",
                ErrorCode::UnknownName,
                "local",
            ),
            ("false && 1", ErrorCode::TypeMismatch, "1"),
            ("true || {}", ErrorCode::TypeMismatch, "{}"),
            ("1 && true", ErrorCode::TypeMismatch, "1"),
            ("{} || false", ErrorCode::TypeMismatch, "{}"),
            ("false && missing", ErrorCode::UnknownName, "missing"),
            ("true || missing", ErrorCode::UnknownName, "missing"),
            (
                "false && (let local = true; local); local",
                ErrorCode::UnknownName,
                "local",
            ),
            (
                "true && (let local = true; local); local",
                ErrorCode::UnknownName,
                "local",
            ),
            (
                "var x = 0; false && (x = 1)",
                ErrorCode::TypeMismatch,
                "(x = 1)",
            ),
            (
                "let x = 1; if false { x = 2 }",
                ErrorCode::InvalidAssignment,
                "x = 2",
            ),
            ("\"a\" < \"b\"", ErrorCode::TypeMismatch, "\"a\" < \"b\""),
            ("{} >= 1", ErrorCode::TypeMismatch, "{} >= 1"),
            ("(1 < 2) < 3", ErrorCode::TypeMismatch, "(1 < 2) < 3"),
            (
                "var x = 1; var y = 2; x = y = 3",
                ErrorCode::TypeMismatch,
                "x = y = 3",
            ),
        ] {
            let source = format!("fn f(p: Int32) {attribute} {{ {body} }}");
            let error = crate::core::prepared::check_source(&source, &crate::ModuleSources::new())
                .unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{source}: {error}");
            assert_eq!(error.code(), code, "{source}: {error}");
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(marked),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn reject_invalid_jumps_and_structurally_unreachable_expressions() {
    use crate::ErrorPhase;
    for attribute in ["", "@suspends"] {
        for (ret, body, code, marked) in [
            ("Int32", "return", ErrorCode::TypeMismatch, "return"),
            ("Unit", "return 1", ErrorCode::TypeMismatch, "return 1"),
            ("Bool", "return 1", ErrorCode::TypeMismatch, "return 1"),
            (
                "String",
                "return false",
                ErrorCode::TypeMismatch,
                "return false",
            ),
            (
                "Int32",
                "return (1;)",
                ErrorCode::TypeMismatch,
                "return (1;)",
            ),
            ("Unit", "break", ErrorCode::InvalidControlFlow, "break"),
            (
                "Unit",
                "continue",
                ErrorCode::InvalidControlFlow,
                "continue",
            ),
            (
                "Unit",
                "if false { break }",
                ErrorCode::InvalidControlFlow,
                "break",
            ),
            (
                "Unit",
                "true || (continue);",
                ErrorCode::InvalidControlFlow,
                "(continue)",
            ),
            ("Int32", "return 1; 2", ErrorCode::UnreachableCode, "2"),
            ("Int32", "{ return 1; }; 2", ErrorCode::UnreachableCode, "2"),
            (
                "Int32",
                "if true { return 1 } else { return 2 }; 3",
                ErrorCode::UnreachableCode,
                "3",
            ),
            (
                "Int32",
                "let x = (return 1); 2",
                ErrorCode::UnreachableCode,
                "2",
            ),
            ("Int32", "(return 1) + 2", ErrorCode::UnreachableCode, "2"),
            (
                "Int32",
                "true + (return 1)",
                ErrorCode::TypeMismatch,
                "true + (return 1)",
            ),
            (
                "Int32",
                "\"text\" < (return 1)",
                ErrorCode::TypeMismatch,
                "\"text\" < (return 1)",
            ),
            (
                "Int32",
                "(return 1) && false",
                ErrorCode::UnreachableCode,
                "false",
            ),
            (
                "Int32",
                "if (return 1) { 2 } else { 3 }",
                ErrorCode::UnreachableCode,
                "{ 2 }",
            ),
            (
                "Int32",
                "pair(return 1, 2)",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Unit",
                "while true { break; 2 }",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Unit",
                "while true { continue; 2 }",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Unit",
                "while (break) { 2 }",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Int32",
                "while (return 1) { 2 }",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Unit",
                "while (continue) {}; 2",
                ErrorCode::UnreachableCode,
                "2",
            ),
            (
                "Int32",
                "let x: Missing = (return 1)",
                ErrorCode::UnknownType,
                "let x: Missing = (return 1)",
            ),
            (
                "Int32",
                "let x = 1; x = (return 2)",
                ErrorCode::InvalidAssignment,
                "x = (return 2)",
            ),
            (
                "Int32",
                "missing = (return 2)",
                ErrorCode::UnknownName,
                "missing = (return 2)",
            ),
        ] {
            let source = format!("fn pair(a: Int32, b: Int32) -> Int32 {{ a + b }} fn f() {attribute} -> {ret} {{ {body} }}");
            let error = crate::core::prepared::check_source(&source, &crate::ModuleSources::new())
                .unwrap_err();
            assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{source}: {error}");
            assert_eq!(error.code(), code, "{source}: {error}");
            assert_eq!(
                error.span().unwrap().slice(&source),
                Some(marked),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn jumps_cannot_inherit_caller_loops_or_skip_scope_argument_checks() {
    for (source, code) in [
        ("fn child() @suspends { break } fn f() @suspends { while true { join { child() } } }", ErrorCode::InvalidControlFlow),
        ("fn child() { continue } fn f() { while true { child() } }", ErrorCode::InvalidControlFlow),
        ("fn child(x: Int32) @suspends {} fn f() @suspends { join { child(return), child(1) } }", ErrorCode::UnreachableCode),
        ("fn child(x: Int32, y: Int32) @suspends {} fn f() @suspends { join { child(return, 1) } }", ErrorCode::UnreachableCode),
        ("fn f() -> Int32 { if true { return 1 } }", ErrorCode::TypeMismatch),
    ] {
        let ast = crate::parser::parse(source).unwrap();
        for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
            assert_eq!(crate::vm::VmEngine::with_mode(mode).load_source(source).unwrap_err().code(), code, "{source}");
        }
        assert_eq!(crate::aot::AotCompiler::new("invalid_flow").unwrap().compile_module(&ast).unwrap_err().code(), code, "{source}");
    }
}

#[test]
fn skipped_paths_still_require_suspending_callers() {
    for body in [
        "if false { wait(); }",
        "if true {} else { wait(); }",
        "false && (wait(); true);",
        "true || (wait(); false);",
        "return wait()",
        "if false { return wait() }",
    ] {
        let source = format!("fn wait() @suspends {{}} fn f() {{ {body} }}");
        let ast = crate::parser::parse(&source).unwrap();
        let mut errors = Vec::new();
        for mode in [crate::vm::VmMode::Jit, crate::vm::VmMode::Interpreter] {
            errors.push(
                crate::vm::VmEngine::with_mode(mode)
                    .load_source(&source)
                    .unwrap_err(),
            );
        }
        errors.push(
            crate::aot::AotCompiler::new("skipped_call")
                .unwrap()
                .compile_module(&ast)
                .unwrap_err(),
        );
        for error in &errors {
            assert_eq!(error.phase(), crate::ErrorPhase::TypeCheck);
            assert_eq!(error.code(), ErrorCode::InvalidCall);
            assert_eq!(error.span().unwrap().slice(&source), Some("wait()"));
        }
        assert_eq!(errors[0], errors[1]);
        assert_eq!(errors[1], errors[2]);
    }
}

#[test]
fn jit_and_aot_reject_incorrect_annotations_and_reserved_names() {
    for attribute in ["", "@suspends"] {
        for (source, message) in [
            (
                format!("fn f() {attribute} {{ let x: Bool = 1 }}"),
                "Type annotation mismatch",
            ),
            (
                format!("fn f() {attribute} {{ let x: Int32 = {{}} }}"),
                "Type annotation mismatch",
            ),
            (
                format!("fn joky_cps_frame_new() {attribute} {{}}"),
                "Reserved runtime",
            ),
            (
                format!("fn __joky_cps_slots() {attribute} {{}}"),
                "Reserved runtime",
            ),
        ] {
            let ast = crate::parser::parse(&source).unwrap();
            let error = crate::backends::jit::compile_with_registry(
                &ast,
                &[],
                &HashMap::new(),
                vec![],
                crate::runtime::suspension::ModuleId(1),
                &crate::core::resolver::BindingRegistry::default(),
                &TypeRegistry::default(),
            )
            .err()
            .expect("JIT must reject unsupported semantics");
            assert!(error.message().contains(message), "{source}: {error}");
            let error = crate::aot::AotCompiler::new("invalid")
                .unwrap()
                .compile_module(&ast)
                .unwrap_err();
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }
}

#[test]
fn jit_and_aot_preserve_expression_error_codes_and_spans() {
    let source = "fn f() -> Int32 { let value: Bool = 1 }";
    let ast = crate::parser::parse(source).unwrap();
    let expected_start = source.find("let value").unwrap();
    let expected_end = source.find('1').unwrap() + 1;
    let jit_error = crate::backends::jit::compile_with_registry(
        &ast,
        &[],
        &HashMap::new(),
        vec![],
        crate::runtime::suspension::ModuleId(2),
        &crate::core::resolver::BindingRegistry::default(),
        &TypeRegistry::default(),
    )
    .err()
    .expect("JIT should reject the mismatched annotation");
    let aot_error = crate::aot::AotCompiler::new("diagnostics")
        .unwrap()
        .compile_module(&ast)
        .unwrap_err();
    for error in [jit_error, aot_error] {
        assert_eq!(error.code(), ErrorCode::TypeMismatch);
        assert_eq!(
            error.span(),
            Some(crate::Span::new(expected_start, expected_end))
        );
    }
}

#[test]
fn import_layouts_reject_invalid_sizes_and_non_imports() {
    let ast = crate::parser::parse(
        r#"
        extern fn host(x: Int32) @suspends -> Int32
        extern fn sync_host(x: Int32) -> Int32
        fn local(x: Int32) @suspends -> Int32 { host(x) }
    "#,
    )
    .unwrap();
    for (name, slots) in [
        ("host", 0),
        ("host", usize::MAX),
        ("missing", 1),
        ("local", 2),
        ("sync_host", 1),
    ] {
        assert!(ModulePlan::with_import_layouts_and_types(
            &ast,
            &HashMap::from([(name.into(), slots)]),
            &TypeRegistry::default(),
        )
        .is_err());
    }
    let plan = ModulePlan::with_import_layouts_and_types(
        &ast,
        &HashMap::from([("host".into(), 10)]),
        &TypeRegistry::default(),
    )
    .unwrap();
    assert_eq!(plan.functions["host"].frame_slots, 10);
}

#[test]
fn enum_diagnostics_point_to_patterns_payloads_and_incompatible_arm_values() {
    for (body, marked, code) in [
        (
            "enum E { A, B } fn f(e: E) { match e { A => {} } }",
            "match e { A => {} }",
            ErrorCode::NonExhaustiveMatch,
        ),
        (
            "enum E { A, B } fn f(e: E) { match e { A => {}, A => {} } }",
            "A",
            ErrorCode::UnreachableCode,
        ),
        (
            "enum E { A } fn f(e: E) { match e { Wrong.A => {} } }",
            "Wrong.A",
            ErrorCode::InvalidPattern,
        ),
        (
            "enum E { A(Int32) } fn f(e: E) { match e { A => {} } }",
            "A",
            ErrorCode::InvalidPattern,
        ),
        (
            "enum E { A(Int32) } fn f() -> E { E.A(true) }",
            "true",
            ErrorCode::TypeMismatch,
        ),
        (
            "enum E { A, B } fn f() -> Int32 { match E.A { A => 1, B => false } }",
            "false",
            ErrorCode::TypeMismatch,
        ),
        (
            "enum E { A } fn f() -> Int32 { match (return 1) { A => 2 } }",
            "2",
            ErrorCode::UnreachableCode,
        ),
    ] {
        let source = format!("// 🙂\n{body}");
        let types = TypeRegistry::default();
        let ast = types
            .resolve_aliases(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let error = ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &types)
            .err()
            .unwrap();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert_eq!(
            error.span().unwrap().slice(&source),
            Some(marked),
            "{source}"
        );
    }
}

#[test]
fn match_alternatives_validate_coverage_and_locate_invalid_patterns() {
    for (source, marked, code) in [
        (
            "fn f(n: Int32) { match n { 1 | 0x1 if false => {}, _ => {} } }",
            "0x1",
            ErrorCode::UnreachableCode,
        ),
        (
            "fn f(n: UInt8) { match n { 1..=4 => {}, 0 | 2..=3 => {}, _ => {} } }",
            "2..=3",
            ErrorCode::UnreachableCode,
        ),
        (
            "fn f(n: UInt8) { match n { 0..128 | 128..=255 if true => {} } }",
            "match n { 0..128 | 128..=255 if true => {} }",
            ErrorCode::NonExhaustiveMatch,
        ),
        (
            "enum E { A(Int32), B } fn f(e: E) { match e { A(value) | B => {} } }",
            "A(value)",
            ErrorCode::InvalidPattern,
        ),
    ] {
        let types = TypeRegistry::default();
        let ast = types
            .resolve_aliases(&crate::parser::parse(source).unwrap())
            .unwrap();
        let error = ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &types)
            .err()
            .unwrap();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert_eq!(
            error.span().unwrap().slice(source),
            Some(marked),
            "{source}"
        );
    }
}

#[test]
fn tuple_diagnostics_locate_pattern_leaves_and_constructor_exits() {
    for (body, marked, code) in [
        (
            "fn f() { let (x, (x,)) = (1, (2,)) }",
            "x",
            ErrorCode::DuplicateDefinition,
        ),
        (
            "fn f() { let (x, (y,)) = (1, 2) }",
            "(y,)",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let (x,) = (1, 2) }",
            "(x,)",
            ErrorCode::TypeMismatch,
        ),
        (
            "fn f() { let (x,) = (1,); x = 2 }",
            "x = 2",
            ErrorCode::InvalidAssignment,
        ),
        (
            "fn f() -> (Int32, Bool) { (return (1, true), false) }",
            "false",
            ErrorCode::UnreachableCode,
        ),
    ] {
        let source = format!("// 🙂\n{body}");
        let types = TypeRegistry::new();
        let ast = types
            .resolve_aliases(&crate::parser::parse(&source).unwrap())
            .unwrap();
        let error = ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &types)
            .err()
            .unwrap();
        assert_eq!(error.code(), code, "{source}: {error}");
        assert_eq!(
            error.span().unwrap().slice(&source),
            Some(marked),
            "{source}"
        );
    }
}

#[test]
fn generic_instances_are_cached_nominally_and_keep_failure_effects_internal() {
    let source = r#"
        type Alias = Int64
        struct R { value: Int32 }
        struct S { value: Int32 }
        fn id<T>(value: T) -> T { value }
        fn unused<U>(value: U) -> U { value }
        fn fail<T>(value: T, n: Int32) -> T { 1 / n; value }
        fn recurse<T>(value: T, n: Int32) -> T { if n == 0 { value } else { recurse(value, n - 1) } }
        fn a() -> Int64 { id::<Alias>(1) }
        fn b() -> Int64 { id::<Int64>(2) }
        fn c() -> R { id(R { value: 3 }) }
        fn d() -> S { id(S { value: 4 }) }
        fn e(n: Int32) -> Int32 { fail(recurse(42, 4), n) }
    "#;
    let types = TypeRegistry::default().with_prelude(false);
    let module = types
        .resolve_aliases(&crate::parser::parse(source).unwrap())
        .unwrap();
    let plan = ModulePlan::with_import_layouts_and_types(&module, &HashMap::new(), &types).unwrap();
    let instances: Vec<_> = plan
        .functions
        .iter()
        .filter(|(name, info)| {
            info.internal
                && ["_id", "_fail", "_recurse"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        })
        .collect();
    assert_eq!(instances.len(), 5);
    assert_eq!(
        instances
            .iter()
            .filter(|(name, _)| name.ends_with("_id"))
            .count(),
        3
    );
    assert!(!plan.functions.contains_key("id"));
    assert!(!plan.functions.keys().any(|name| name.ends_with("_unused")));
    assert!(plan.functions["e"].may_fail);
    assert!(!plan.functions["a"].may_fail);
    let a_call = plan.bodies["a"]
        .ops
        .iter()
        .find_map(|op| match op {
            super::Op::Call { name, .. } => Some(name),
            _ => None,
        })
        .unwrap();
    let b_call = plan.bodies["b"]
        .ops
        .iter()
        .find_map(|op| match op {
            super::Op::Call { name, .. } => Some(name),
            _ => None,
        })
        .unwrap();
    assert_eq!(a_call, b_call);
    assert_eq!(
        plan.functions[a_call].signature.params,
        [crate::Type::Int64]
    );
    let second =
        ModulePlan::with_import_layouts_and_types(&module, &HashMap::new(), &types).unwrap();
    let mut first_names: Vec<_> = plan.functions.keys().collect();
    first_names.sort();
    let mut second_names: Vec<_> = second.functions.keys().collect();
    second_names.sort();
    assert_eq!(first_names, second_names);
}
