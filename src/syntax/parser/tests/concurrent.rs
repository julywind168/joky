use super::super::*;

#[test]
fn parser_builds_call_and_block_expressions() {
    let program = parse_program("fn main() { println(\"hello\") }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Call {
        callee, arguments, ..
    } = &expressions[0].kind
    else {
        panic!("expected a call expression");
    };
    assert!(matches!(&callee.kind, ExprKind::Name(name) if name == "println"));
    assert!(matches!(&arguments[0].value.kind, ExprKind::String(value) if value == "hello"));
}

#[test]
fn parser_builds_tagged_collection_literals() {
    let program = parse_program(
        r#"fn main() {
            let xs = List#{1, 2}
            let ys = Set#{1, 2}
            let zs = Map#{"a" => 1, "b" => 2}
        }"#,
    )
    .unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected block");
    };
    assert!(matches!(
        &expressions[0].kind,
        ExprKind::Let { value, .. } if matches!(&value.kind, ExprKind::CollectionLiteral(_))
    ));
    assert!(matches!(
        &expressions[2].kind,
        ExprKind::Let { value, .. } if matches!(&value.kind, ExprKind::CollectionLiteral(_))
    ));
}

#[test]
fn parser_builds_labeled_call_arguments() {
    let program = parse_program("fn main() { add(b: 2, a: 1) }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
        panic!("expected a call expression");
    };
    assert_eq!(arguments[0].label.as_deref(), Some("b"));
    assert_eq!(arguments[1].label.as_deref(), Some("a"));
}

#[test]
fn parser_builds_calls_with_type_values() {
    let program = parse_program("fn main() { \"1.5\".parse(Float64) }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
        panic!("expected a call expression");
    };
    assert_eq!(arguments.len(), 1);
    assert!(matches!(arguments[0].value.kind, ExprKind::Name(ref name) if name == "Float64"));
}

#[test]
fn parser_builds_duration_literal() {
    let program = parse_program("fn main() { 1h10m100s }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    assert!(matches!(expressions[0].kind, ExprKind::Duration(4_300_000)));
}

#[test]
fn parser_builds_do_with_handler_expression() {
    let program = parse_program(
        "fn main() { let value = do { source() } with { Clock.now() => fake_time() }; value }",
    )
    .expect("do-with expression should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    let ExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected let expression");
    };
    let ExprKind::Do { handlers, .. } = &value.kind else {
        panic!("expected do expression");
    };
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].effect, "Clock");
    assert_eq!(handlers[0].operation, "now");
    assert!(handlers[0].parameters.is_empty());
}

#[test]
fn parser_builds_single_cown_when_expression() {
    let program = parse_program(
        "class Counter { var value: Int32 = 0 }\n\
         fn main() { when (counter) |state| { state.value } }",
    )
    .expect("when expression should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    assert!(
        matches!(expressions[0].kind, ExprKind::When { ref cowns, ref bindings, .. } if cowns.len() == 1 && bindings.as_ref().is_some_and(|items| items.len() == 1))
    );
}

#[test]
fn parser_builds_multi_cown_when_bindings() {
    let program = parse_program("fn main() { when (source, target) |src, dst| { src } }")
        .expect("multi-Cown when expression should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    assert!(
        matches!(expressions[0].kind, ExprKind::When { ref cowns, ref bindings, .. } if cowns.len() == 2 && bindings.as_ref().is_some_and(|items| items.len() == 2))
    );
}

#[test]
fn parser_omits_when_bindings_for_simple_cown_names() {
    let program = parse_program("fn main() { when (counter) { counter.value } }")
        .expect("implicit-binding when expression should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    assert!(
        matches!(expressions[0].kind, ExprKind::When { ref cowns, bindings: None, .. } if cowns.len() == 1)
    );
}

#[test]
fn parser_rejects_the_removed_hash_in_binding() {
    let error = parse_program("fn main() { when (counter) { #in state state.value } }")
        .expect_err("removed '#in' binding must be rejected");
    assert!(
        error.to_string().contains("|state|"),
        "diagnostic should suggest the closure parameter form: {error}"
    );
}

#[test]
fn parser_builds_parallel_and_race_arms() {
    let program = parse_program(
        "fn main() { let values = parallel {\n| first()\n| second()\n}; race {\n| first()\n| second()\n} }",
    )
    .expect("concurrent expressions should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected parallel binding");
    };
    assert!(matches!(&value.kind, ExprKind::Parallel(arms) if arms.len() == 2));
    assert!(matches!(&expressions[1].kind, ExprKind::Race(arms) if arms.len() == 2));
}

#[test]
fn parser_builds_branch_expression() {
    let program =
        parse_program("fn main() { branch { work() } }").expect("branch expression should parse");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function body");
    };
    assert!(matches!(&expressions[0].kind, ExprKind::Branch(_)));
}

#[test]
fn concurrent_arm_accepts_parenthesized_bitwise_or_and_trailing_closure() {
    let program = parse_program(
        "fn apply(f: fn(Int32) -> Int32) -> Int32 { f(1) }\nfn main() { parallel {\n| (1 | 2)\n| apply |x| { x }\n} }",
    )
    .expect("parenthesized bitwise or and a trailing closure stay inside the arm");
    let ExprKind::Block(expressions) = &program.functions[1].body.kind else {
        panic!("expected function body");
    };
    let ExprKind::Parallel(arms) = &expressions[0].kind else {
        panic!("expected parallel");
    };
    assert!(matches!(
        arms[0].kind,
        ExprKind::Binary {
            op: BinaryOp::BitOr,
            ..
        }
    ));
    assert!(matches!(arms[1].kind, ExprKind::Call { .. }));
}

#[test]
fn parser_rejects_malformed_concurrent_arms() {
    for source in [
        "fn main() { parallel {} }",
        "fn main() { race { work() } }",
        "fn main() { parallel { | first() | second() } }",
    ] {
        assert!(parse_program(source).is_err(), "accepted: {source}");
    }
}
