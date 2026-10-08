use super::*;

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
    assert_eq!(block("{ (fn() -> Int32 { 42 })\n(1) }").len(), 2);
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
