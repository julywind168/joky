use super::super::*;

#[test]
fn casts_bind_as_suffixes_and_checked_casts_can_propagate() {
    use crate::syntax::CastMode;

    let program = parse_program(
        "fn main() { let a = x + y as Int64; let b = (-x) as% UInt32; \
         let c = x as? UInt8 ?; let d = x as% Int8 as Int64; let e = -x as Int64; \
         let f = (x as? UInt8).is_err() }",
    )
    .unwrap();
    let ExprKind::Block(body) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let value = |index: usize| match &body[index].kind {
        ExprKind::Let { value, .. } => value.as_ref(),
        _ => panic!("binding"),
    };
    let ExprKind::Binary {
        op: BinaryOp::Add,
        right,
        ..
    } = &value(0).kind
    else {
        panic!("cast binds before addition")
    };
    assert!(matches!(
        right.kind,
        ExprKind::Cast {
            mode: CastMode::Lossless,
            ..
        }
    ));
    let ExprKind::Cast {
        value: operand,
        mode: CastMode::Wrapping,
        ..
    } = &value(1).kind
    else {
        panic!("parenthesized negative operand")
    };
    assert!(matches!(
        operand.kind,
        ExprKind::Unary {
            op: UnaryOp::Negate,
            ..
        }
    ));
    let ExprKind::Unwrap {
        value: checked,
        propagate: true,
    } = &value(2).kind
    else {
        panic!("checked cast propagation")
    };
    assert!(matches!(
        checked.kind,
        ExprKind::Cast {
            mode: CastMode::Checked,
            ..
        }
    ));
    let ExprKind::Cast {
        value: inner,
        mode: CastMode::Lossless,
        ..
    } = &value(3).kind
    else {
        panic!("left-associated cast chain")
    };
    assert!(matches!(
        inner.kind,
        ExprKind::Cast {
            mode: CastMode::Wrapping,
            ..
        }
    ));
    let ExprKind::Unary {
        expression: operand,
        op: UnaryOp::Negate,
    } = &value(4).kind
    else {
        panic!("unparenthesized cast binds before negation")
    };
    assert!(matches!(operand.kind, ExprKind::Cast { .. }));
    assert!(matches!(value(5).kind, ExprKind::Call { .. }));
}

#[test]
fn saturating_casts_bind_before_bitwise_or_and_parallel_arm_pipes() {
    use crate::syntax::CastMode;

    let program = parse_program(
        r#"
        fn main() {
            let bits = x as| UInt8 | y
            let tasks = parallel {
                | x as| UInt8
                | y as| UInt8
            }
        }
        "#,
    )
    .unwrap();
    let ExprKind::Block(body) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Let { value, .. } = &body[0].kind else {
        panic!("binding")
    };
    let ExprKind::Binary {
        op: BinaryOp::BitOr,
        left,
        ..
    } = &value.kind
    else {
        panic!("bitwise or remains a binary operator")
    };
    assert!(matches!(
        left.kind,
        ExprKind::Cast {
            mode: CastMode::Saturating,
            ..
        }
    ));
    let ExprKind::Let { value, .. } = &body[1].kind else {
        panic!("binding")
    };
    let ExprKind::Parallel(arms) = &value.kind else {
        panic!("parallel arms")
    };
    assert_eq!(arms.len(), 2);
    for arm in arms {
        assert!(matches!(
            arm.kind,
            ExprKind::Cast {
                mode: CastMode::Saturating,
                ..
            }
        ));
    }
}

#[test]
fn arithmetic_modes_bind_like_their_unsuffixed_operators() {
    use crate::syntax::{ArithmeticMode, ArithmeticOp};

    let program = parse_program(
        r#"
        fn main() {
            let a = x +? y *| z
            let b = parallel {
                | x +| y
                | z +% w
            }
        }
        "#,
    )
    .unwrap();
    let ExprKind::Block(body) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Let { value, .. } = &body[0].kind else {
        panic!("binding")
    };
    let ExprKind::Binary { op, left, right } = &value.kind else {
        panic!("binary")
    };
    assert_eq!(
        *op,
        BinaryOp::Arithmetic(ArithmeticOp::Add, ArithmeticMode::Checked)
    );
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::Arithmetic(ArithmeticOp::Multiply, ArithmeticMode::Saturating),
            ..
        }
    ));
    assert!(matches!(left.kind, ExprKind::Name(_)));
    let ExprKind::Let { value, .. } = &body[1].kind else {
        panic!("binding")
    };
    let ExprKind::Parallel(arms) = &value.kind else {
        panic!("parallel arms")
    };
    assert!(matches!(
        arms[0].kind,
        ExprKind::Binary {
            op: BinaryOp::Arithmetic(ArithmeticOp::Add, ArithmeticMode::Saturating),
            ..
        }
    ));
    assert!(matches!(
        arms[1].kind,
        ExprKind::Binary {
            op: BinaryOp::Arithmetic(ArithmeticOp::Add, ArithmeticMode::Wrapping),
            ..
        }
    ));
}

#[test]
fn bitwise_precedence_stays_outside_range_endpoints() {
    let program = parse_program(
        "fn apply(f: fn(Int32) -> Int32) -> Int32 { f(1) }\nfn main() { let bits = 1 + 2 << 1 & 3 | 4; let range = 1 .. 2 & 3; let called = apply |x| { x } }",
    )
    .unwrap();
    let ExprKind::Block(body) = &program.functions[1].body.kind else {
        panic!()
    };
    let ExprKind::Let { value, .. } = &body[0].kind else {
        panic!()
    };
    let ExprKind::Binary {
        op: BinaryOp::BitOr,
        left,
        ..
    } = &value.kind
    else {
        panic!("bitwise or is the loosest of the chain")
    };
    assert!(matches!(
        left.kind,
        ExprKind::Binary {
            op: BinaryOp::BitAnd,
            ..
        }
    ));
    let ExprKind::Let { value, .. } = &body[1].kind else {
        panic!()
    };
    let ExprKind::Binary {
        op: BinaryOp::BitAnd,
        left,
        ..
    } = &value.kind
    else {
        panic!("a .. b & c")
    };
    assert!(matches!(left.kind, ExprKind::Range { .. }));
    let ExprKind::Let { value, .. } = &body[2].kind else {
        panic!()
    };
    assert!(matches!(value.kind, ExprKind::Call { .. }));
}

#[test]
fn range_precedence_steps_and_lexical_boundaries() {
    let program = parse_program(
        "fn main() { let r = -1..=2 + 3 by -2; let x = 1.5; let by = 2; let s = 0..5 by by }",
    )
    .unwrap();
    let ExprKind::Block(body) = &program.functions[0].body.kind else {
        panic!()
    };
    let ExprKind::Let { value, .. } = &body[0].kind else {
        panic!()
    };
    let ExprKind::Range {
        start,
        end,
        step,
        inclusive,
    } = &value.kind
    else {
        panic!()
    };
    assert!(*inclusive);
    assert!(matches!(start.kind, ExprKind::Unary { .. }));
    assert!(matches!(
        end.kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
    assert!(matches!(
        step.as_ref().unwrap().kind,
        ExprKind::Unary { .. }
    ));
    let ExprKind::Let { value, .. } = &body[1].kind else {
        panic!()
    };
    assert!(matches!(value.kind, ExprKind::Float(_)));
    for source in ["0..1..2", "0...2", "..2", "0..", "0..=", "0..2 by"] {
        assert!(
            parse_program(&format!("fn main() {{ let r = {source} }}")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn echo_has_expression_precedence_and_original_pipeline_location() {
    let program = parse_program_named(
        "fn main() {\n    echo 1 + 2 * 3\n    echo(4) + 5\n    6\n        |> echo\n        |> next\n}",
        "src/main.jk",
    ).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
        panic!("echo")
    };
    assert!(matches!(
        arguments[0].value.kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
    assert!(
        matches!(&arguments[1].value.kind, ExprKind::String(value) if value == "src/main.jk:2:5")
    );
    assert!(matches!(
        expressions[1].kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
    let ExprKind::Call { arguments, .. } = &expressions[2].kind else {
        panic!("pipeline")
    };
    let ExprKind::Call { arguments, .. } = &arguments[0].value.kind else {
        panic!("echo stage")
    };
    assert!(
        matches!(&arguments[1].value.kind, ExprKind::String(value) if value == "src/main.jk:5:12")
    );
}

#[test]
fn echo_parentheses_accept_one_tuple_operand() {
    for (source, count) in [
        ("echo (1, 2)", 2),
        ("echo(1, 2)", 2),
        ("echo(1,)", 1),
        ("echo()", 0),
    ] {
        let program = parse_program(&format!("fn main() {{ {source} }}")).unwrap();
        let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
            panic!("block")
        };
        let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
            panic!("echo")
        };
        assert_eq!(arguments.len(), 2, "operand and source location");
        let ExprKind::Tuple(elements) = &arguments[0].value.kind else {
            panic!("tuple operand: {source}")
        };
        assert_eq!(elements.len(), count, "{source}");
    }
}

#[test]
fn echo_is_reserved_and_requires_an_operand() {
    for source in [
        "fn echo() {}",
        "fn main() { let echo = 1 }",
        "fn main() { let f = echo }",
        "fn main() { echo }",
    ] {
        assert!(parse_program(source).is_err(), "{source}");
    }
}

#[test]
fn parser_builds_interpolated_strings_and_escapes_braces() {
    let program = parse_program("fn main() { \"hello {{name}} {value + 1}!\" }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::InterpolatedString(parts) = &expressions[0].kind else {
        panic!("interpolated string")
    };
    assert_eq!(parts.len(), 2);
    assert!(
        matches!(&parts[0], (text, Some(expression)) if text == "hello {name} " && matches!(expression.kind, ExprKind::Binary { .. }))
    );
    assert!(matches!(&parts[1], (text, None) if text == "!"));
}

#[test]
fn parser_maps_interpolation_expression_spans_to_source() {
    let source = "fn main() {\n    println(\"line\\n{value + 1}!\")\n}";
    let program = parse_program(source).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
        panic!("println call")
    };
    let ExprKind::InterpolatedString(parts) = &arguments[0].value.kind else {
        panic!("interpolated string")
    };
    let expression = parts[0].1.as_ref().expect("interpolation expression");
    let start = source.find("value + 1").expect("source expression");
    assert_eq!(expression.span.start(), start);
    assert_eq!(expression.span.end(), start + "value + 1".len());
}

#[test]
fn parser_keeps_raw_strings_literal() {
    let program = parse_program(r###"fn main() { r"\d+ {value} C:\tmp" }"###).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    assert!(
        matches!(&expressions[0].kind, ExprKind::String(text) if text == r"\d+ {value} C:\tmp")
    );
}

#[test]
fn parser_interpolates_multiline_strings_after_stripping_indentation() {
    let program =
        parse_program("fn main() {\n    \"\"\"\n    id = {value + 1}\n    \"\"\"\n}").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::InterpolatedString(parts) = &expressions[0].kind else {
        panic!("interpolated string")
    };
    assert!(matches!(&parts[0], (text, Some(_)) if text == "id = "));
    assert!(matches!(&parts[1], (text, None) if text.is_empty()));
}

#[test]
fn parser_maps_multiline_interpolation_spans_to_source() {
    let source = "fn main() {\n    println(\"\"\"\n    line\\n{value + 1}!\n    \"\"\")\n}";
    let program = parse_program(source).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("block")
    };
    let ExprKind::Call { arguments, .. } = &expressions[0].kind else {
        panic!("println call")
    };
    let ExprKind::InterpolatedString(parts) = &arguments[0].value.kind else {
        panic!("interpolated string")
    };
    let expression = parts[0].1.as_ref().expect("interpolation expression");
    let start = source.find("value + 1").expect("source expression");
    assert_eq!(expression.span.start(), start);
    assert_eq!(expression.span.end(), start + "value + 1".len());
}

#[test]
fn parser_builds_option_result_postfix_operations() {
    let program = parse_program("fn main() { value!; other? }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    assert!(matches!(
        expressions[0].kind,
        ExprKind::Unwrap {
            propagate: false,
            ..
        }
    ));
    assert!(matches!(
        expressions[1].kind,
        ExprKind::Unwrap {
            propagate: true,
            ..
        }
    ));
}

#[test]
fn parser_keeps_byte_strings_literal_without_interpolation() {
    let program = parse_program(r#"fn main() { b"a{x}\n" }"#).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    assert!(matches!(
        &expressions[0].kind,
        ExprKind::Bytes(value) if value == "a{x}\n"
    ));
}

#[test]
fn parser_keeps_operator_precedence_inside_programs() {
    let program = parse_program("fn main() { 2 + 3 * 4 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Binary { op, right, .. } = &expressions[0].kind else {
        panic!("expected a binary expression");
    };
    assert_eq!(*op, BinaryOp::Add);
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::Multiply,
            ..
        }
    ));
}

#[test]
fn parser_requires_separators_between_expressions() {
    assert!(parse_program("fn main() { println(\"a\") println(\"b\") }").is_err());
    assert!(parse_program("fn main() { println(\"a\"); println(\"b\") }").is_ok());
    assert!(parse_program("fn main() { println(\"a\")\nprintln(\"b\") }").is_ok());
}

#[test]
fn parser_allows_multiline_bindings_without_semicolons() {
    let source = "fn main() {\n    let x = 100\n    let x = 200\n    println(x)\n}";
    assert!(parse_program(source).is_ok());
}

#[test]
fn parser_allows_multiline_member_chains() {
    let source = "fn main() {\n    value\n        .first()\n        .second()\n}";
    assert!(parse_program(source).is_ok());
}

#[test]
fn parser_handles_operator_precedence() {
    let program = parse_program("fn main() { 1 + 2 * 3 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Binary {
        op: BinaryOp::Add,
        left,
        right,
    } = &expressions[0].kind
    else {
        panic!("expected addition at top level");
    };
    assert!(matches!(left.kind, ExprKind::Integer(1)));
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::Multiply,
            ..
        }
    ));
}

#[test]
fn parser_handles_comparison_operators() {
    let program = parse_program("fn main() { 1 < 2 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    assert!(matches!(
        expressions[0].kind,
        ExprKind::Binary {
            op: BinaryOp::Less,
            ..
        }
    ));
}

#[test]
fn parser_handles_unary_negation() {
    let program = parse_program("fn main() { -42 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    assert!(matches!(
        expressions[0].kind,
        ExprKind::Unary {
            op: UnaryOp::Negate,
            ..
        }
    ));
}

#[test]
fn parser_handles_logical_precedence_and_not() {
    let program = parse_program("fn main() { !false || true && false }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Binary {
        op: BinaryOp::Or,
        left,
        right,
    } = &expressions[0].kind
    else {
        panic!("expected logical or at top level");
    };
    assert!(matches!(
        left.kind,
        ExprKind::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));
}

#[test]
fn parser_handles_nested_blocks() {
    let program = parse_program("fn main() { { { 42 } } }").unwrap();
    let ExprKind::Block(outer) = &program.functions[0].body.kind else {
        panic!("expected outer block");
    };
    let ExprKind::Block(middle) = &outer[0].kind else {
        panic!("expected middle block");
    };
    let ExprKind::Block(inner) = &middle[0].kind else {
        panic!("expected inner block");
    };
    assert!(matches!(inner[0].kind, ExprKind::Integer(42)));
}

#[test]
fn parser_handles_parenthesized_expressions() {
    let program = parse_program("fn main() { (1 + 2) * 3 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Binary {
        op: BinaryOp::Multiply,
        left,
        ..
    } = &expressions[0].kind
    else {
        panic!("expected multiplication");
    };
    assert!(matches!(
        left.kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
}

#[test]
fn parser_handles_tuples_and_fields() {
    let program = parse_program("fn main() { let pair = (1, 2); pair.1 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    assert!(
        matches!(&expressions[0].kind, ExprKind::Let { value, .. } if matches!(value.kind, ExprKind::Tuple(_)))
    );
    assert!(matches!(
        expressions[1].kind,
        ExprKind::Field {
            access: FieldAccess::Index(1),
            ..
        }
    ));
}

#[test]
fn parser_handles_structs_and_fields() {
    let program = parse_program(
        "struct Point { let x: Int32, let y: Int32 } fn main() { let point = Point(x: 1, y: 2); point.x }",
    )
    .unwrap();
    assert_eq!(program.structs[0].name, "Point");
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    assert!(
        matches!(&expressions[0].kind, ExprKind::Let { value, .. } if matches!(value.kind, ExprKind::Call { .. }))
    );
    assert!(matches!(
        &expressions[1].kind,
        ExprKind::Field { access: FieldAccess::Name(name), .. } if name == "x"
    ));
}

#[test]
fn parser_rejects_braced_type_construction() {
    assert!(parse_program("fn main() { Point { x: 1 } }").is_err());
}

#[test]
fn parser_requires_let_on_struct_fields() {
    assert!(parse_program("struct Point { x: Int32 } fn main() { Point(x: 1) }").is_err());
    assert!(parse_program("struct Point { let x: Int32 } fn main() { Point(x: 1) }").is_ok());
}

#[test]
fn parser_parses_assignment_expressions() {
    assert!(parse_program("fn main() { let x = 0; x = x + 1 }").is_ok());
}

#[test]
fn parser_builds_anonymous_and_move_closures() {
    let program = parse_program(
        "fn main() { let f = fn (x: Int32) -> Int32 { x + 1 }; let g = move fn () -> Int32 { 1 } }",
    )
    .unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    let ExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected first let");
    };
    assert!(matches!(
        value.kind,
        ExprKind::Closure {
            move_capture: false,
            ..
        }
    ));
    let ExprKind::Let { value, .. } = &expressions[1].kind else {
        panic!("expected second let");
    };
    assert!(matches!(
        value.kind,
        ExprKind::Closure {
            move_capture: true,
            ..
        }
    ));
}

#[test]
fn parser_builds_comparison_expressions() {
    let program = parse_program("fn main() { let result = 1 <= 2 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    let ExprKind::Let { value, .. } = &expressions[0].kind else {
        panic!("expected a let expression");
    };
    assert!(matches!(
        value.kind,
        ExprKind::Binary {
            op: BinaryOp::LessEqual,
            ..
        }
    ));
}

#[test]
fn parser_builds_all_literal_types() {
    let program = parse_program(r#"fn main() { 42; 3.14; "hello"; true; false }"#).unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block");
    };
    assert!(matches!(expressions[0].kind, ExprKind::Integer(_)));
    assert!(matches!(expressions[1].kind, ExprKind::Float(_)));
    assert!(matches!(expressions[2].kind, ExprKind::String(_)));
    assert!(matches!(expressions[3].kind, ExprKind::Boolean(true)));
    assert!(matches!(expressions[4].kind, ExprKind::Boolean(false)));
}

#[test]
fn parser_builds_let_expressions() {
    let program = parse_program("fn main() { let x = 100; let x = 200; x }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    assert!(matches!(
        &expressions[0].kind,
        ExprKind::Let { name, value, .. }
            if name == "x" && matches!(value.kind, ExprKind::Integer(100))
    ));
    assert!(matches!(
        &expressions[1].kind,
        ExprKind::Let { name, value, .. }
            if name == "x" && matches!(value.kind, ExprKind::Integer(200))
    ));
    assert!(matches!(&expressions[2].kind, ExprKind::Name(name) if name == "x"));
}

#[test]
fn parser_builds_binding_type_annotations() {
    let program =
        parse_program("fn main() { let msg: String = \"hello\"; let count: Int32 = 1 }").unwrap();
    let ExprKind::Block(expressions) = &program.functions[0].body.kind else {
        panic!("expected a block expression");
    };
    assert!(matches!(
        &expressions[0].kind,
        ExprKind::Let { annotation: Some(annotation), .. } if annotation.to_string() == "String"
    ));
    assert!(matches!(
        &expressions[1].kind,
        ExprKind::Let { annotation: Some(annotation), .. } if annotation.to_string() == "Int32"
    ));
}
