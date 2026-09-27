use super::ModulePlan;
use crate::{ErrorCode, TypeRegistry};
use std::collections::HashMap;

#[test]
fn all_backends_reject_invalid_assignment_loop_and_comparison_semantics() {
    use crate::{
        vm::{VmEngine, VmMode},
        ErrorPhase,
    };
    for attribute in ["", "@suspends"] {
        for (body, code, marked) in [
            ("let x = 1 x = 2", ErrorCode::InvalidAssignment, "x = 2"),
            ("p = 2", ErrorCode::InvalidAssignment, "p = 2"),
            ("x = 2", ErrorCode::UnknownName, "x = 2"),
            ("var x = 1 x = true", ErrorCode::TypeMismatch, "x = true"),
            (
                "var x: Bool = 1",
                ErrorCode::TypeMismatch,
                "var x: Bool = 1",
            ),
            ("var x = 1 x = {}", ErrorCode::TypeMismatch, "x = {}"),
            ("var x = {} x = 1", ErrorCode::TypeMismatch, "x = 1"),
            (
                "var x = 1 { let x = 2 x = 3 }",
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
            ("while false { var x = 1 } x", ErrorCode::UnknownName, "x"),
            (
                "while { var x = 1 false } { x }",
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
            ("{} == {}", ErrorCode::TypeMismatch, "{} == {}"),
            ("-true", ErrorCode::TypeMismatch, "-true"),
            ("-{}", ErrorCode::TypeMismatch, "-{}"),
            ("-\"text\"", ErrorCode::TypeMismatch, "-\"text\""),
            ("!1", ErrorCode::TypeMismatch, "!1"),
            ("!{}", ErrorCode::TypeMismatch, "!{}"),
            ("!\"text\"", ErrorCode::TypeMismatch, "!\"text\""),
            ("\"a\" != \"b\"", ErrorCode::TypeMismatch, "\"a\" != \"b\""),
            ("{} >= 1", ErrorCode::TypeMismatch, "{} >= 1"),
            ("(1 < 2) < 3", ErrorCode::TypeMismatch, "(1 < 2) < 3"),
            (
                "var x = 1 var y = 2 x = y = 3",
                ErrorCode::TypeMismatch,
                "x = y = 3",
            ),
        ] {
            let source = format!("fn f(p: Int32) {attribute} {{ {body} }}");
            let ast = crate::parser::parse(&source).unwrap();
            let mut errors = Vec::new();
            for mode in [VmMode::Jit, VmMode::Interpreter] {
                errors.push(VmEngine::with_mode(mode).load_source(&source).unwrap_err());
            }
            errors.push(
                crate::aot::AotCompiler::new("invalid_control_flow")
                    .unwrap()
                    .compile_module(&ast)
                    .unwrap_err(),
            );
            for error in &errors {
                assert_eq!(error.phase(), ErrorPhase::TypeCheck, "{source}: {error}");
                assert_eq!(error.code(), code, "{source}: {error}");
                assert_eq!(
                    error.span().unwrap().slice(&source),
                    Some(marked),
                    "{source}: {error}"
                );
            }
            assert_eq!(errors[0], errors[1]);
            assert_eq!(errors[1], errors[2]);
        }
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
