use super::*;

#[test]
fn same_line_expressions_require_a_separator() {
    for source in [
        "fn f() { let x = 1 x }",
        "fn f() { first() second() }",
        "fn f() { while false {} 42 }",
        "fn f() { 1 /* same line */ 2 }",
    ] {
        let error = parse(source).unwrap_err();
        assert!(
            error.message.contains("Expected ';' or a newline"),
            "{error}"
        );
    }
    for source in [
        "fn f() { let x = 1; x }",
        "fn f() { let x = 1\nx }",
        "fn f() { first(); second() }",
        "fn f() { while false {}; 42 }",
        "fn f() { 1 /* real\nnewline */ 2 }",
    ] {
        parse(source).unwrap();
    }
}

#[test]
fn call_suffixes_apply_to_all_expressions() {
    for source in [
        "fn f() { (callback)(41) }",
        "fn f() { choose()(41) }",
        "fn f() { callbacks[0](41) }",
        "fn f() { optional!(41) }",
        "fn f() { (if true { first } else { second })(41) }",
        "fn f() { { callback }(41) }",
        "fn f() { (|x: Int32| { x + 1 })(41) }",
    ] {
        let module = parse(source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!()
        };
        let Expr::Block { exprs, .. } = &function.body else {
            panic!()
        };
        assert!(matches!(&exprs[..], [Expr::Invoke { .. }]), "{source}");
    }
}

fn block(source: &str) -> Vec<Expr> {
    let mut parser = Parser::from_source(source).unwrap();
    let Expr::Block { exprs, .. } = parser.parse_block().unwrap() else {
        panic!("expected block")
    };
    assert_eq!(parser.peek(), &TokenKind::Eof);
    exprs
}

#[test]
fn complete_expressions_end_before_next_line_operators() {
    let source = "{ let r = &mut *p\n*r = *r - 1\n*r\n-2\n&value }";
    let exprs = block(source);
    assert_eq!(exprs.len(), 5);
    assert!(matches!(&exprs[0], Expr::Let { .. }));
    assert!(matches!(&exprs[1], Expr::FieldAssign { .. }));
    assert!(matches!(
        &exprs[2],
        Expr::Unary {
            op: UnaryOp::Deref,
            ..
        }
    ));
    assert_eq!(exprs[3].span().slice(source), Some("-2"));
    assert!(matches!(
        &exprs[4],
        Expr::Unary {
            op: UnaryOp::Borrow,
            ..
        }
    ));
    for source in ["{ a - b }", "{ a * b }", "{ a & b }"] {
        assert!(matches!(&block(source)[0], Expr::BinOp { .. }));
    }
    for source in ["{ 1\n+ 2 }", "{ a\n&& b }", "{ a\n= b }"] {
        let mut parser = Parser::from_source(source).unwrap();
        assert!(parser.parse_block().is_err(), "{source}");
    }
}

#[test]
fn block_comments_preserve_newline_separation_rules() {
    assert_eq!(block("{ 42/* same line */-1 }").len(), 1);
    assert_eq!(block("{ 42/* outer\n/* nested */ */-1 }").len(), 2);
    assert_eq!(block("{ (42/*\r\n*/-1) }").len(), 1);
    for gap in ["/*\n*/", "/* outer\r\n/* nested */ */"] {
        let source =
            format!("struct P {{ x: Int32{gap}y: Int32 }} fn f() {{ P {{ x: 1{gap}y: 2 }} }}");
        assert!(parse(&source).is_ok(), "{source}");
    }
    for source in [
        "struct P { x: Int32/* gap */y: Int32 }",
        "fn f() { P { x: 1/* gap */y: 2 } }",
        "fn f() { pick(1/*\n*/2) }",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
}

#[test]
fn incomplete_expressions_and_parentheses_continue_across_lines() {
    for source in [
        "{ let x =\n1 +\n2\nx }",
        "{ let x = (1\n+ 2)\nx }",
        "{ let x = pick(1\n+ 2,\n4)\nx }",
        "{ var x = 1\nx =\n2\nx }",
        "{ let x = a &&\nb\nx }",
        "{ let x = a <\nb\nx }",
    ] {
        let exprs = block(source);
        assert_eq!(exprs.last().unwrap().span().slice(source), Some("x"));
    }
    let exprs = block("{ pick({ let p = &mut value\n*p = 42\n*p })\n-1 }");
    let Expr::Call { args, .. } = &exprs[0] else {
        panic!()
    };
    let Expr::Block { exprs: nested, .. } = &args[0] else {
        panic!()
    };
    assert_eq!(nested.len(), 3);
    assert_eq!(exprs.len(), 2);
    let exprs = block("{ pick(R { x: value\n y: -1 })\n-2 }");
    let Expr::Call { args, .. } = &exprs[0] else {
        panic!()
    };
    assert!(matches!(&args[0], Expr::Record { fields, .. } if fields.len() == 2));
    assert_eq!(exprs.len(), 2);
}

#[test]
fn leading_dots_continue_fields_and_methods_across_comments_and_blank_lines() {
    for receiver in [
        "value",
        "make()",
        "(make())",
        "{ value }",
        "Choice.Value(42)",
        "if flag { value } else { value }",
        "42",
        "\"value\"",
    ] {
        for gap in ["\n", "\r\n", "\n\n", " // 🙂 comment\n\n"] {
            let chain = format!("{receiver}{gap}.next::<Int32>(\n42,\n){gap}.finish()");
            let source = format!("{{ {chain}\n-1 }}");
            let exprs = block(&source);
            assert_eq!(exprs.len(), 2, "{source}");
            assert_eq!(exprs[0].span().slice(&source), Some(chain.as_str()));
            assert!(matches!(&exprs[0], Expr::MethodCall(call) if call.name == "finish"));
        }
    }
    assert_eq!(block("{ value\n.field\n.child }").len(), 1);
    assert_eq!(block("{ value.\nmethod() }").len(), 1);
    for source in ["{ value;\n.method() }", "{\n.method() }", "{ value\n. }"] {
        let mut parser = Parser::from_source(source).unwrap();
        assert!(parser.parse_block().is_err(), "{source}");
    }
}

#[test]
fn call_parentheses_follow_complete_names_only_on_the_same_line() {
    for source in [
        "{ f\n(42) }",
        "{ value.method\n(42) }",
        "{ f::<Int32>\n(42) }",
    ] {
        assert_eq!(block(source).len(), 2, "{source}");
    }
    assert_eq!(block("{ (f\n(42)) }").len(), 1);
    assert_eq!(block("{ pick(f\n(42)) }").len(), 1);
    assert_eq!(block("{ (|| -> Int32 { 42 })\n(1) }").len(), 2);
    assert_eq!(block("{ R\n{} }").len(), 2);
    assert!(matches!(&block("{ R {} }")[0], Expr::Record { .. }));
}

#[test]
fn newline_return_is_bare_and_newline_tail_values_are_preserved() {
    let exprs = block("{ return // no value\n42 }");
    assert_eq!(exprs.len(), 2);
    assert!(matches!(&exprs[0], Expr::Return { value: None, .. }));
    for source in ["{ return (\n42\n) }", "{ (return\n42) }"] {
        assert!(matches!(
            &block(source)[0],
            Expr::Return { value: Some(_), .. }
        ));
    }
    assert!(matches!(&block("{ 42\n }")[0], Expr::Literal { .. }));
    assert!(matches!(
        &block("{ 42;\n }")[0],
        Expr::Sequence { trailing: true, .. }
    ));
}

#[test]
fn multiline_interpolation_ends_on_its_closing_line() {
    let source = "{ \"{({\n42\n})}\" + \"tail\"\n-1 }";
    let exprs = block(source);
    assert_eq!(exprs.len(), 2);
    assert!(matches!(&exprs[0], Expr::BinOp { op: BinOp::Add, .. }));
}

#[test]
fn postfix_unwrap_chains_and_respects_expression_boundaries() {
    let source = "{ value?!.field[0]!\n!flag\nother != last }";
    let exprs = block(source);
    assert_eq!(exprs.len(), 3);
    assert_eq!(exprs[0].span().slice(source), Some("value?!.field[0]!"));
    assert!(matches!(
        exprs[0],
        Expr::Unary {
            op: UnaryOp::Unwrap,
            ..
        }
    ));
    assert!(matches!(
        exprs[1],
        Expr::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));
    assert!(matches!(exprs[2], Expr::BinOp { op: BinOp::Ne, .. }));
    assert_eq!(block("{ value\n!flag }").len(), 2);
    assert_eq!(block("{ value/*\n*/!flag }").len(), 2);
    assert!(Parser::from_source("{ value\n? }")
        .unwrap()
        .parse_block()
        .is_err());
    assert_eq!(block("{ value!\n.method()? }").len(), 1);
}

#[test]
fn leading_subtraction_in_lists_requires_an_explicit_continuation() {
    for body in [
        "[10, 20\n-5, 30]",
        "f(20 /*\r\n*/ -5)",
        "(20\n-5, 30)",
        "(10, 20\n-5)",
        "[..x\n-y]",
    ] {
        let source = format!("fn test() {{ {body} }}");
        let error = parse(&source).unwrap_err();
        assert_eq!(error.span.slice(&source), Some("-"));
        assert!(error.message.contains("comma"), "{error}");
    }
    for body in [
        "(20\n-5)",
        "[(20\n-5), -1]",
        "[20 -\n5]",
        "[20,\n-5]",
        "f(xs[i\n-1])",
        "(f(20),\n-5)",
    ] {
        parse(&format!("fn test() {{ {body} }}")).unwrap();
    }
}
