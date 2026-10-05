use super::*;

#[test]
fn branch_pipes_require_delimiters_for_bitwise_or() {
    for source in [
            "enum E { A B } fn f(e: E) -> Int32 { match e { | A => (1 | 2) | B => { 4 | 8 } } }",
            "enum E { A B } fn f(e: E) { match e { | A => g(1 | 2) | B => g(4 | 8) } }",
            "enum E { A B } struct R { x: Int32 } fn f(e: E) { match e { | A => R { x: 1 | 2 } | B => R { x: 4 | 8 } } }",
            "enum E { A B } fn f(e: E) { match e { | A => (1 | 2, 3 | 4) | B => (5, 6) } }",
            "enum E { A B } fn f(e: E) { match e { | A => let x = 1; (x | 2) | B => 3 } }",
            "enum E { A B } fn f(e: E) { match e { | A => if (1 | 2) == 3 { 1 | 2 } else { 0 } | B => 4 } }",
            "fn f() @suspends { parallel { | g(1 | 2) | g(4 | 8); } }",
            "enum E { A B } fn f(e: E) { match e { | A => match e { | A => 1 | B => 2 } | B => 3 } }",
        ] { parse(source).unwrap(); }
    for source in [
        "enum E { A B } fn f(e: E) { match e { | A => 1 | 2 | B => 3 } }",
        "enum E { A B } fn f(e: E) { match e { | A => 1 | B } }",
        "fn f() { 1 | }",
        "fn f() { ~ }",
        "fn f() { 1 << }",
        "fn f() { 1 >> }",
        "fn f() { 1 ^ }",
        "fn f() { 1 & }",
        "fn f() { 1 < < 2 }",
        "fn f() { 1 <<< 2 }",
        "fn f() { 1 >>= 2 }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    let source = "// 🙂\nfn f() -> Int64 { -0x8000_0000_0000_0000 }";
    let module = parse(source).unwrap();
    let Item::FnDef(f) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &f.body else {
        panic!()
    };
    assert!(matches!(
        exprs[0],
        Expr::Literal {
            literal: Literal::Integer {
                magnitude: 9223372036854775808,
                negative: true
            },
            ..
        }
    ));
    assert_eq!(
        exprs[0].span().slice(source),
        Some("-0x8000_0000_0000_0000")
    );
}

#[test]
fn jumps_require_grouping_inside_operator_operands() {
    for body in [
        "1 + return 2",
        "true && break",
        "!continue",
        "return +",
        "break = 1",
    ] {
        let source = format!("fn f() {{ {body} }}");
        let error = parse(&source).unwrap_err();
        assert!(error.span.slice(&source).is_some(), "{error}");
    }
    for body in [
        "1 + (return 2)",
        "true && (break)",
        "!(continue)",
        "return return 2",
    ] {
        parse(&format!("fn f() {{ {body} }}")).unwrap();
    }
}

#[test]
fn logical_precedence_and_else_if_spans_are_preserved() {
    let source =
        "fn f(数: Int32) { if 数 > 0 || false && !true { 1 } else if 数 == 0 { 2 } else { 3 } }";
    let module = parse(source).unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function")
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!("expected block")
    };
    let Expr::If {
        condition,
        then_branch,
        else_branch,
        span,
    } = &exprs[0]
    else {
        panic!("expected if")
    };
    assert_eq!(
        span.slice(source),
        Some("if 数 > 0 || false && !true { 1 } else if 数 == 0 { 2 } else { 3 }")
    );
    assert_eq!(then_branch.span().slice(source), Some("{ 1 }"));
    let Expr::Logical {
        op: LogicalOp::Or,
        lhs,
        rhs,
        ..
    } = condition.as_ref()
    else {
        panic!("expected disjunction")
    };
    assert!(matches!(lhs.as_ref(), Expr::BinOp { op: BinOp::Gt, .. }));
    assert!(matches!(
        rhs.as_ref(),
        Expr::Logical {
            op: LogicalOp::And,
            ..
        }
    ));
    let Expr::If {
        else_branch: Some(other),
        ..
    } = else_branch.as_ref().unwrap().as_ref()
    else {
        panic!("expected else if")
    };
    assert_eq!(other.span().slice(source), Some("{ 3 }"));
}

#[test]
fn incomplete_conditionals_and_logical_operators_report_located_errors() {
    for (body, message) in [
        ("if", "Expected an expression"),
        ("if true 1", "Expected LBrace"),
        ("if true {} else", "Expected LBrace"),
        ("if true {} else 1", "Expected LBrace"),
        ("if true {}; else {}", "Expected an expression"),
        ("else {}", "Expected an expression"),
        ("if true; {}", "Expected LBrace"),
        ("true &&", "Expected an expression"),
        ("|| true", "Expected an expression"),
        ("true ||| false", "Expected an expression"),
        ("parallel { || f() }", "Expected RBrace"),
        ("true &", "Expected an expression"),
    ] {
        let source = format!("fn f() {{ {body} }}");
        let error = parse(&source).unwrap_err();
        assert!(error.message.contains(message), "{source}: {error}");
        assert!(error.span.slice(&source).is_some());
    }
    parse("fn f() { if true {}\nelse {} parallel { | f() | f() } }").unwrap();
}

#[test]
fn integer_literals_preserve_untyped_magnitudes_and_negative_spans() {
    for expression in [
        "2147483648",
        "-2147483648",
        "9223372036854775807",
        "-9223372036854775808",
        "-(2147483648)",
    ] {
        parse(&format!("fn f() -> Int64 {{ {expression} }}")).unwrap();
    }
    for spelling in ["18446744073709551616", "999999999999999999999999999999"] {
        let source = format!("// 🙂\nfn f() {{ {spelling} }}");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.message.contains("outside UInt64 magnitude range"));
        assert_eq!(error.span.slice(&source), Some(spelling));
    }
    let source = "fn f() { -9223372036854775808 }";
    let module = parse(source).unwrap();
    let Item::FnDef(f) = &module.items[0] else {
        panic!()
    };
    let Expr::Block { exprs, .. } = &f.body else {
        panic!()
    };
    assert!(matches!(
        exprs[0],
        Expr::Literal {
            literal: Literal::Integer {
                magnitude: 9223372036854775808,
                negative: true
            },
            ..
        }
    ));
    assert_eq!(exprs[0].span().slice(source), Some("-9223372036854775808"));
}

#[test]
fn unary_precedence_and_utf8_spans_are_preserved() {
    let source = "fn f(数: Int32) -> Int32 { 1 + -数 * --2 }";
    let module = parse(source).unwrap();
    let Item::FnDef(function) = &module.items[0] else {
        panic!("expected function")
    };
    let Expr::Block { exprs, .. } = &function.body else {
        panic!("expected block")
    };
    let Expr::BinOp {
        op: BinOp::Add,
        rhs,
        ..
    } = &exprs[0]
    else {
        panic!("expected addition")
    };
    let Expr::BinOp {
        op: BinOp::Mul,
        lhs,
        rhs,
        ..
    } = rhs.as_ref()
    else {
        panic!("expected product")
    };
    assert_eq!(lhs.span().slice(source), Some("-数"));
    let Expr::Unary {
        op: UnaryOp::Neg,
        operand,
        ..
    } = rhs.as_ref()
    else {
        panic!("expected negation")
    };
    assert_eq!(rhs.span().slice(source), Some("--2"));
    assert_eq!(operand.span().slice(source), Some("-2"));
}

#[test]
fn comparisons_require_explicit_grouping_and_unary_operators_require_operands() {
    for expression in [
        "1 < 2 < 3",
        "1 < 2 == true",
        "true == false != true",
        "true != 1 < 2",
    ] {
        let source = format!("fn f() {{ {expression} }}");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.message, "Comparison chains require parentheses");
        assert!(matches!(error.span.slice(&source), Some("<" | "==" | "!=")));
    }
    for expression in ["(1 < 2) == true", "true == (1 < 2)", "!(true != false)"] {
        parse(&format!("fn f() -> Bool {{ {expression} }}")).unwrap();
    }
    for expression in ["-", "!", "1 + -", "(1; !)", "1 * !"] {
        let source = format!("fn f() {{ {expression} }}");
        let error = parse(&source).unwrap_err();
        assert!(error.message.contains("Expected an expression"), "{error}");
    }
}
