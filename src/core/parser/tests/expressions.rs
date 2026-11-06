use super::*;

#[test]
fn tuples_distinguish_grouping_lists_and_irrefutable_patterns() {
    let source =
        "// 🙂\nfn f(p: (Int32, Bool,)) -> (Int32,) { let (x, _): (Int32, Bool) = p; (x; x + 1,) }";
    let ast = parse(source).unwrap();
    let Item::FnDef(f) = &ast.items[0] else {
        panic!()
    };
    assert_eq!(f.params[0].ty.to_string(), "(Int32, Bool)");
    assert_eq!(f.ret_ty.to_string(), "(Int32,)");
    let Expr::Block { exprs, .. } = &f.body else {
        panic!()
    };
    let Expr::Sequence { exprs, .. } = &exprs[0] else {
        panic!()
    };
    let Expr::Destructure { pattern, value, .. } = &exprs[0] else {
        panic!()
    };
    assert_eq!(pattern.span().slice(source), Some("(x, _)"));
    assert_eq!(value.span().slice(source), Some("p"));
    let Expr::Tuple { elements, span } = &exprs[1] else {
        panic!()
    };
    assert_eq!(span.slice(source), Some("(x; x + 1,)"));
    assert!(
        matches!(&elements[0], Expr::Sequence { exprs, trailing: false, .. } if exprs.len() == 2)
    );
    for source in [
        "fn f() { let (x) = 1\n let (_, (y,)) = (x, (2,)) }",
        "fn f() { var (x, (_, y)) = (1, (2, 3)) }",
        "fn f(p: ((Int32))) -> (Int32) { (p) }",
        "fn f() { let _ = 1\n var _ = 2 }",
        "fn f() { () }",
        "fn f() { ( /* Unit */ ) }",
        "fn f(p: (Unit,)) -> (Unit,) { ((),) }",
        "fn f() { let pair = (R { x: 1 }, E.Value(2)) }",
        "fn f(p: (Int32, Bool)) { let (x, y) = p; (x, y) }",
        "fn f() { let p = (1,2)\n p.0 }",
        "fn f() { let p = ((1,),)\n p.0.0 }",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
    for source in [
        "fn f(x: ()) {}",
        "type T = ()",
        "fn f() { let () = {} }",
        "fn f() { (1 2) }",
        "fn f() { (1,,2) }",
        "fn f() { (,1) }",
        "fn f(x: (Int32 Bool)) {}",
        "fn f(x: (Int32,,)) {}",
        "fn f() { let (x y) = (1,2) }",
        "fn f() { let (x,,y) = (1,2) }",
        "fn f() { let (1, y) = (1,2) }",
        "fn f() { let E.Value(x) = e }",
        "fn f() { let (E.Value(x),) = e }",
        "fn f() { let (x: Int32,) = (1,) }",
        "fn f() { let (x, ..) = (1,2) }",
        "fn f() { let (x,) }",
        "fn f() { (x, y) = (1,2) }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    // New recursive grammar forms reject excessive syntax nesting explicitly.
    let ty = format!("fn f(x: {}Int32{}) {{}}", "(".repeat(300), ",)".repeat(300));
    assert!(parse(&ty).unwrap_err().message.contains("256"));
    let pattern = format!(
        "fn f() {{ let {}x{} = 1 }}",
        "(".repeat(300),
        ",)".repeat(300)
    );
    assert!(parse(&pattern).unwrap_err().message.contains("256"));
}

#[test]
fn lists_require_commas_and_report_the_first_unseparated_token() {
    for (body, marked, message) in [
        ("fn f(x: Int32 y: Int32) {}", "y", "Expected ',' or ')'"),
        ("extern fn f(x: Int32 y: Bool)", "y", "Expected ',' or ')'"),
        ("fn f() { g(1 2) }", "2", "Expected ',' or ')'"),
        ("fn f() { g(1; 2 3) }", "3", "Expected ',' or ')'"),
        ("fn f() { g(1; 2; 3 4) }", "4", "Expected ',' or ')'"),
        ("fn f() { g(1 数) }", "数", "Expected ',' or ')'"),
        ("fn f(,x: Int32) {}", ",", "Expected an identifier"),
        ("fn f(x: Int32,,) {}", ",", "Expected an identifier"),
        ("fn f() { g(,1) }", ",", "Expected an expression"),
        ("fn f() { g(1,,2) }", ",", "Expected an expression"),
        ("fn f() { g(1", "", "Expected ',' or ')'"),
        ("fn f(x: Int32", "", "Expected ',' or ')'"),
        ("fn f() { g(1,", "", "Expected an expression"),
    ] {
        let source = format!("// é🙂\n{body}");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.line, 2, "{source}");
        assert_eq!(error.span.slice(&source), Some(marked), "{source}");
        assert!(error.message.contains(message), "{source}: {error}");
    }
    for body in [
        "fn f(x: Int32\ny: Int32) {}",
        "fn f() { g(1\n2) }",
        "fn f(x: Int32 // comma required\ny: Int32) {}",
        "fn f() { g(1 // comma required\n2) }",
    ] {
        let error = parse(body).unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.message.contains("Expected ',' or ')'"));
    }
}

#[test]
fn incomplete_semicolon_expressions_report_located_errors() {
    for (source, message) in [
        ("fn f() { var x = ; }", "Expected an expression"),
        ("fn f() { var x = 0; x = ; }", "Expected an expression"),
        ("fn f() { (1; + 2) }", "Expected an expression"),
        ("fn f() { (1;", "Expected `)`"),
        ("fn f() { 1;", "Expected `}`"),
        ("fn f() { while true; {} }", "Expected `{`"),
        ("fn f(x: Int32; y: Int32) {}", "Expected ',' or ')'"),
    ] {
        let error = parse(source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert!(error.span.slice(source).is_some());
    }
}

#[test]
fn malformed_mutable_bindings_and_loops_return_parse_errors() {
    for (source, message) in [
        ("fn f() { var x }", "Expected `=`"),
        ("fn f() { var x: Int32 }", "Expected `=`"),
        ("fn f() { var x = }", "Expected an expression"),
        ("fn f() { while true 1 }", "Expected `{`"),
        ("fn f() { while }", "Expected an expression"),
        ("fn f() { 1 = 2 }", "Assignment target must be a variable"),
        ("fn f() { f() = 2 }", "Assignment target must be a variable"),
        ("fn f() { var x = 0\n x = }", "Expected an expression"),
    ] {
        let error = parse(source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert!(error.span.slice(source).is_some());
    }
}

#[test]
fn malformed_input_returns_located_diagnostics() {
    for (source, line, message) in [
        ("\nfn f() { $ }", 2, "Unexpected character"),
        ("\nfn f() { ? }", 2, "Expected an expression"),
        ("\nfn f() { # }", 2, "Expected an expression"),
        (
            "\n\nfn f() { 18446744073709551616 }",
            3,
            "outside UInt64 magnitude range",
        ),
        ("\nfn f() { \"unfinished", 2, "Unterminated string literal"),
        ("\nfn f() { \"unfinished\\", 2, "Unterminated string escape"),
        ("fn f() {\nlet x =\n}", 3, "Expected an expression"),
        ("fn f() {\n", 1, "Expected `}`"),
    ] {
        let diagnostic = parse(source).unwrap_err();
        assert_eq!(diagnostic.line, line, "{source:?}");
        assert!(diagnostic.message.contains(message), "{diagnostic}");
        assert!(diagnostic.to_string().contains(&line.to_string()));
    }
}

#[test]
fn concurrent_branches_use_commas_or_newlines() {
    for body in [
        "join { first(), second(), }",
        "race { first()\nsecond() }",
        "join { first() /* separator\n */ second(), { third(); fourth() } }",
        "race { first(1 | 2), { join { third()\nfourth() }; true } }",
    ] {
        let source = format!("fn f() @suspends {{ {body} }}");
        parse(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
    for body in [
        "join { first() second() }",
        "race { first(); second() }",
        "join { , first() }",
        "race { first(),, second() }",
    ] {
        assert!(
            parse(&format!("fn f() @suspends {{ {body} }}")).is_err(),
            "{body}"
        );
    }
    for body in [
        "join { | first() | second() }",
        "race {\n | first()\n | second() }",
    ] {
        let source = format!("fn f() @suspends {{ {body} }}");
        let error = parse(&source).unwrap_err();
        assert!(error.message.contains("Use commas or newlines"));
        assert_eq!(error.span.slice(&source), Some("|"));
    }
}

#[test]
fn expression_depth_accepts_its_boundary_and_preserves_the_vm_after_rejection() {
    let source = |depth| {
        format!(
            "fn value() -> Int32 {{ {}42{} }}",
            "(".repeat(depth),
            ")".repeat(depth)
        )
    };
    let mut vm = crate::vm::VmEngine::new_interpreter();
    vm.load_source(&source(31)).unwrap();
    let error = vm.load_source(&source(2000)).unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert!(error.span().is_some());
    assert_eq!(vm.call_int32_0("value").unwrap(), 42);
}

#[test]
fn embedded_module_depth_is_checked_before_cloning_the_ast() {
    let mut ast = parse("fn f() {}").unwrap();
    let Item::FnDef(f) = &mut ast.items[0] else {
        unreachable!()
    };
    for _ in 0..100 {
        let span = f.body.span();
        let inner = std::mem::replace(
            &mut f.body,
            Expr::Block {
                exprs: vec![],
                span,
            },
        );
        f.body = Expr::Block {
            exprs: vec![inner],
            span,
        };
    }
    let error = crate::vm::VmEngine::new_interpreter()
        .load_module(&ast)
        .unwrap_err();
    assert_eq!(error.code(), crate::ErrorCode::ResourceLimit);
    assert!(error.span().is_some());
}
