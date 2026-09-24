use super::super::*;

#[test]
fn parser_builds_if_expressions() {
    let program = parse_program("fn main() { let x = if true 1 else 2; x }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected a let expression");
    };
    assert!(matches!(value.kind, ExprKind::If { .. }));
}

#[test]
fn parser_builds_while_expressions() {
    let program = parse_program("fn main() { while 1 < 2 { println(\"loop\") } }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    assert!(matches!(expressions[0].kind, ExprKind::While { .. }));
}

#[test]
fn parser_handles_loop_and_break() {
    let program = parse_program("fn main() { loop { break 42 } }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Loop { body } = &expressions[0].kind else {
        panic!("expected loop");
    };
    let ExprKind::Block(loop_body) = &body.kind else {
        panic!("expected loop body");
    };
    assert!(matches!(loop_body[0].kind, ExprKind::Break { .. }));
}

#[test]
fn parser_handles_continue() {
    let program = parse_program("fn main() { while true { continue } }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::While { body, .. } = &expressions[0].kind else {
        panic!("expected while");
    };
    let ExprKind::Block(loop_body) = &body.kind else {
        panic!("expected loop body");
    };
    assert!(matches!(loop_body[0].kind, ExprKind::Continue));
}

#[test]
fn parser_handles_enum_variants_and_match() {
    let program = parse_program(
        "enum Shape { Circle(radius: Float32); Rectangle(width: Float32, height: Float32); Triangle } \
         fn main() { \
             let shape = Shape.Circle(radius: 1.0); \
             match shape { \
                 Shape.Circle(radius) => radius; \
                 Shape.Rectangle(width, height) => width + height; \
                 Shape.Triangle => 0.0 \
             } \
         }",
    )
    .unwrap();
    assert_eq!(program.enums[0].name, "Shape");
    assert_eq!(program.enums[0].variants.len(), 3);
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected function block");
    };
    assert!(matches!(expressions[1].kind, ExprKind::Match { .. }));

    let wildcard =
        parse_program("enum Choice { First; Second } fn main() { match Choice.First { _ => 0 } }")
            .unwrap();
    let ExprKind::Block(expressions) = &wildcard.functions[0].body.kind else {
        panic!("expected function block");
    };
    let ExprKind::Match { arms, .. } = &expressions[0].kind else {
        panic!("expected match expression");
    };
    assert!(matches!(arms[0].pattern, Pattern::Wildcard { .. }));

    let nested = parse_program(
        "enum Inner { Value(number: Int32) } \
         enum Outer { Some(value: Inner) } \
         fn main() { \
             match Outer.Some(value: Inner.Value(number: 1)) { \
                 Outer.Some(value: Inner.Value(number)) => number \
             } \
         }",
    )
    .unwrap();
    let ExprKind::Block(expressions) = &nested.functions[0].body.kind else {
        panic!("expected function block");
    };
    let ExprKind::Match { arms, .. } = &expressions[0].kind else {
        panic!("expected match expression");
    };
    let Pattern::EnumVariant { fields, .. } = &arms[0].pattern else {
        panic!("expected outer enum pattern");
    };
    assert!(matches!(fields[0].pattern, Pattern::EnumVariant { .. }));

    let tuples = parse_program(
        "fn main() {
             match Ok((1, 2)) {
                 Ok((left, right)) => left + right\n
                 Err(_) => 0
             }
             match (1, 2) {
                 (left, right) => left + right
             }
         }",
    )
    .unwrap();
    let ExprKind::Block(expressions) = &tuples.functions[0].body.kind else {
        panic!("expected function block");
    };
    let ExprKind::Match { arms, .. } = &expressions[0].kind else {
        panic!("expected Result match");
    };
    let Pattern::EnumVariant { fields, .. } = &arms[0].pattern else {
        panic!("expected Ok pattern");
    };
    assert!(matches!(fields[0].pattern, Pattern::Tuple { .. }));
    let ExprKind::Match { arms, .. } = &expressions[1].kind else {
        panic!("expected tuple match");
    };
    let Pattern::Tuple { elements, .. } = &arms[0].pattern else {
        panic!("expected tuple pattern");
    };
    assert_eq!(elements.len(), 2);
}

#[test]
fn parser_handles_complex_if_expression() {
    let program = parse_program("fn main() { if x < 10 { 1 } else { 2 } }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::If {
        condition,
        then_branch,
        else_branch,
    } = &expressions[0].kind
    else {
        panic!("expected if");
    };
    assert!(matches!(condition.kind, ExprKind::Binary { .. }));
    assert!(matches!(then_branch.kind, ExprKind::Block(_)));
    assert!(matches!(else_branch.kind, ExprKind::Block(_)));
}

#[test]
fn parser_allows_if_branches_on_separate_lines() {
    assert!(parse_program("fn main() { let x = if true\n1\nelse\n2\n}").is_ok());
}

#[test]
fn parser_builds_loop_and_break_expressions() {
    let program = parse_program("fn main() { loop { break 42 } }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Loop { body } = &expressions[0].kind else {
        panic!("expected a loop expression");
    };
    let ExprKind::Block(expressions) = &body.kind else {
        panic!("expected a loop body");
    };
    assert!(matches!(
        expressions[0].kind,
        ExprKind::Break { value: Some(_) }
    ));
}
