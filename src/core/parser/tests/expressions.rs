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
        "fn f() { let (x) = 1 let (_, (y,)) = (x, (2,)) }",
        "fn f() { var (x, (_, y)) = (1, (2, 3)) }",
        "fn f(p: ((Int32))) -> (Int32) { (p) }",
        "fn f() { let _ = 1 var _ = 2 }",
        "fn f(p: (Unit,)) -> (Unit,) { ({},) }",
        "fn f() { let pair = (R { x: 1 }, E.Value(2)) }",
        "fn f(p: (Int32, Bool)) { let (x, y) = p; (x, y) }",
        "fn f() { let p = (1,2) p.0 }",
        "fn f() { let p = ((1,),) p.0.0 }",
    ] {
        assert!(parse(source).is_ok(), "{source}");
    }
    for source in [
        "fn f() { () }",
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
fn trailing_commas_preserve_sequences_and_optional_semicolons() {
    let source = r#"
            extern fn pick(x: Int32, y: Int32,)
            fn f(x: Int32, // A comment does not replace a comma.
                 y: Int32,) {
                let a = 1 let b = 2
                pick(a; b, pick(x, y,),)
                pick((a; b), (x; y;),)
            }
        "#;
    let module = parse(source).unwrap();
    let Item::ExternFn(external) = &module.items[0] else {
        panic!()
    };
    assert_eq!(external.params.len(), 2);
    let Item::FnDef(function) = &module.items[1] else {
        panic!()
    };
    assert_eq!(function.params.len(), 2);
    let Expr::Block { exprs, .. } = &function.body else {
        panic!()
    };
    assert_eq!(exprs.len(), 4);
    for (call, trailing) in [(&exprs[2], false), (&exprs[3], true)] {
        let Expr::Call { args, .. } = call else {
            panic!()
        };
        assert_eq!(args.len(), 2);
        let Expr::Sequence {
            exprs,
            trailing: false,
            ..
        } = &args[0]
        else {
            panic!()
        };
        assert_eq!(exprs.len(), 2);
        if trailing {
            assert!(matches!(&args[1], Expr::Sequence { trailing: true, .. }));
        } else {
            let Expr::Call { args, .. } = &args[1] else {
                panic!()
            };
            assert_eq!(args.len(), 2);
        }
    }
    parse("fn empty() {} fn single(x: Int32,) { empty() single(x,) }").unwrap();
}

#[test]
fn return_preserves_newlines_grouping_and_sequence_boundaries() {
    for (body, returned) in [
        ("return 数 +\n1; 9", "数 +\n1"),
        ("return (数; 2); 9", "(数; 2)"),
        ("return (数;); 9", "(数;)"),
    ] {
        let source = format!("fn f(数: Int32) {{ {body} }}");
        let module = parse(&source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        let Expr::Sequence {
            exprs, trailing, ..
        } = &exprs[0]
        else {
            panic!()
        };
        assert!(!trailing);
        assert_eq!(exprs.len(), 2);
        let Expr::Return {
            value: Some(value),
            span,
        } = &exprs[0]
        else {
            panic!()
        };
        assert_eq!(value.span().slice(&source), Some(returned));
        assert_eq!(span.slice(&source), Some(body.split("; 9").next().unwrap()));
        assert_eq!(exprs[1].span().slice(&source), Some("9"));
    }
    for source in [
        "return",
        "return;",
        "(return)",
        "f(return, 1)",
        "parallel { | return | f() }",
    ] {
        let mut parser = Parser::from_source(source).unwrap();
        parser.parse_expr().unwrap();
        assert_eq!(parser.peek(), &TokenKind::Eof);
    }
    for source in ["return", "return;", "return)", "return,", "return}"] {
        let mut parser = Parser::from_source(source).unwrap();
        assert!(matches!(
            parser.parse_assignment().unwrap(),
            Expr::Return { value: None, .. }
        ));
    }
    let mut parser = Parser::from_source("return|").unwrap();
    parser.allow_pipe = false;
    assert!(matches!(
        parser.parse_assignment().unwrap(),
        Expr::Return { value: None, .. }
    ));
}

#[test]
fn semicolons_preserve_statement_boundaries_and_sequence_spans() {
    let source = "fn f() { var x = 0; x = 1; (x = x + 1; x < 4) }";
    let module = parse(source).unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function")
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!("expected block")
    };
    let Expr::Sequence {
        exprs,
        trailing,
        span,
    } = &exprs[0]
    else {
        panic!("expected sequence")
    };
    assert!(!trailing);
    assert_eq!(
        span.slice(source),
        Some("var x = 0; x = 1; (x = x + 1; x < 4)")
    );
    assert_eq!(exprs.len(), 3);
    let Expr::Var { value, .. } = &exprs[0] else {
        panic!("expected var")
    };
    assert_eq!(value.span().slice(source), Some("0"));
    let Expr::Assign { value, .. } = &exprs[1] else {
        panic!("expected assignment")
    };
    assert_eq!(value.span().slice(source), Some("1"));
    let Expr::Sequence {
        exprs,
        trailing,
        span,
    } = &exprs[2]
    else {
        panic!("expected parenthesized sequence")
    };
    assert!(!trailing);
    assert_eq!(span.slice(source), Some("(x = x + 1; x < 4)"));
    assert_eq!(exprs[0].span().slice(source), Some("x = x + 1"));
    assert_eq!(exprs[1].span().slice(source), Some("x < 4"));
}

#[test]
fn semicolons_stop_at_call_delimiters_and_allow_top_level_separators() {
    let source =
        ";import joky/vm; extern fn pick(x: Unit, y: Int32); fn f() { pick((1;), (2; 3)); };;";
    let module = parse(source).unwrap();
    assert_eq!(module.imports.len(), 1);
    assert_eq!(module.items.len(), 2);
    assert_eq!(module.imports[0].span.slice(source), Some("import joky/vm"));
    let Item::FnDef(function) = &module.items[1] else {
        panic!("expected function")
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!("expected block")
    };
    let Expr::Sequence {
        exprs,
        trailing: true,
        ..
    } = &exprs[0]
    else {
        panic!("expected trailing semicolon")
    };
    let Expr::Call { args, .. } = &exprs[0] else {
        panic!("expected call")
    };
    assert_eq!(args.len(), 2);
    assert!(matches!(&args[0], Expr::Sequence { trailing: true, .. }));
    assert!(matches!(
        &args[1],
        Expr::Sequence {
            trailing: false,
            ..
        }
    ));
}

#[test]
fn incomplete_semicolon_expressions_report_located_errors() {
    for (source, message) in [
        ("fn f() { var x = ; }", "Expected an expression"),
        ("fn f() { var x = 0; x = ; }", "Expected an expression"),
        ("fn f() { (1; + 2) }", "Expected an expression"),
        ("fn f() { (1;", "Expected RParen"),
        ("fn f() { 1;", "Expected RBrace"),
        ("fn f() { while true; {} }", "Expected LBrace"),
        ("fn f(x: Int32; y: Int32) {}", "Expected ',' or ')'"),
    ] {
        let error = parse(source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert!(error.span.slice(source).is_some());
    }
}

#[test]
fn mutable_bindings_loops_and_assignments_preserve_utf8_spans() {
    let source = "fn f() { var 数: Int32 = 0 while 数 < 2 + 1 { 数 = 数 + 1 } }";
    let module = parse(source).unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function")
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!("expected block")
    };
    assert!(matches!(
        &exprs[0],
        Expr::Var {
            annotation: Some(Type::Int32),
            ..
        }
    ));
    assert_eq!(exprs[0].span().slice(source), Some("var 数: Int32 = 0"));
    let Expr::While {
        condition,
        body,
        span,
    } = &exprs[1]
    else {
        panic!("expected while")
    };
    assert_eq!(span.slice(source), Some("while 数 < 2 + 1 { 数 = 数 + 1 }"));
    let Expr::BinOp {
        op: BinOp::Lt, rhs, ..
    } = condition.as_ref()
    else {
        panic!("expected comparison")
    };
    assert!(matches!(rhs.as_ref(), Expr::BinOp { op: BinOp::Add, .. }));
    let Expr::Block { exprs, .. } = body.as_ref() else {
        panic!("expected loop body")
    };
    assert!(matches!(&exprs[0], Expr::Assign { name, .. } if name == "数"));
    assert_eq!(exprs[0].span().slice(source), Some("数 = 数 + 1"));
}

#[test]
fn malformed_mutable_bindings_and_loops_return_parse_errors() {
    for (source, message) in [
        ("fn f() { var x }", "Expected Eq"),
        ("fn f() { var x: Int32 }", "Expected Eq"),
        ("fn f() { var x = }", "Expected an expression"),
        ("fn f() { while true 1 }", "Expected LBrace"),
        ("fn f() { while }", "Expected an expression"),
        ("fn f() { 1 = 2 }", "Assignment target must be a variable"),
        ("fn f() { f() = 2 }", "Assignment target must be a variable"),
        ("fn f() { var x = 0 x = }", "Expected an expression"),
    ] {
        let error = parse(source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert!(error.span.slice(source).is_some());
    }
}

#[test]
fn malformed_input_returns_located_diagnostics() {
    for (source, line, message) in [
        ("\nfn f() { ? }", 2, "Unexpected character"),
        ("\nfn f() { # }", 2, "Expected an expression"),
        (
            "\n\nfn f() { 18446744073709551616 }",
            3,
            "outside UInt64 magnitude range",
        ),
        ("\nfn f() { \"unfinished", 2, "Unterminated string literal"),
        ("\nfn f() { \"unfinished\\", 2, "Unterminated string escape"),
        ("fn f() {\nlet x =\n}", 3, "Expected an expression"),
        ("fn f() {\n", 2, "Expected RBrace"),
    ] {
        let diagnostic = parse(source).unwrap_err();
        assert_eq!(diagnostic.line, line, "{source:?}");
        assert!(diagnostic.message.contains(message), "{diagnostic}");
        assert!(diagnostic.to_string().contains(&line.to_string()));
    }
}

#[test]
fn preserves_nested_expression_spans() {
    let source = "fn main() { let value = 1 + 2 }";
    let module = parse(source).expect("Parsing failed");
    let Item::FnDef(function) = &module.items[0] else {
        panic!("Expected function definition");
    };
    assert_eq!(function.span, Span::new(0, source.len()));
    let Expr::Block { exprs, span } = &function.body else {
        panic!("Expected function block");
    };
    assert_eq!(span, &Span::new(source.find('{').unwrap(), source.len()));
    let Expr::Let { value, span, .. } = &exprs[0] else {
        panic!("Expected let expression");
    };
    assert_eq!(span.slice(source), Some("let value = 1 + 2"));
    let Expr::BinOp { lhs, rhs, span, .. } = value.as_ref() else {
        panic!("Expected binary expression");
    };
    assert_eq!(span.slice(source), Some("1 + 2"));
    assert_eq!(lhs.span().slice(source), Some("1"));
    assert_eq!(rhs.span().slice(source), Some("2"));
}

#[test]
fn pipe_closures_distinguish_logical_or_and_parallel_branches() {
    for body in [
        "let f = || { 42 }",
        "return |x: Int32| { x }",
        "let f = (1; |x: Int32| { x })",
        "let f = move || -> Int32 { 42 }",
        "let f = |x: Int32, y: Int32,| { x | y }",
        "let f = |x: fn(Int32) -> Int32| { x(1) }",
        "let f = || { true || false }",
        "let f = || { || { 42 } }",
        "(|| { 42 })()",
        "parallel { | use_fn(|| { 42 }) | use_fn(|x: Int32| { x }) }",
        "race { | use_fn(move || { 42 }) | other() }",
        "match e { A => || { 42 }, B => move || { 0 } }",
    ] {
        let source = format!("fn test() {{ {body} }}");
        parse(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
    for body in [
        "let f = fn() { 42 }",
        "let f = move fn() { 42 }",
        "let f = |x y| { 42 }",
        "let f = |,| { 42 }",
        "let f = || @suspends { 42 }",
        "let f = || 42",
        "let f = |x|",
        "let f = move { 42 }",
    ] {
        let source = format!("fn test() {{ {body} }}");
        assert!(parse(&source).is_err(), "{source}");
    }
}
