use super::tests::{compile, run};
use super::*;
use crate::backends::interp::{BytecodeModule, Comparison, Execution, Frame, Poll, Value};
use crate::parser::ast::BinOp;
use crate::runtime::suspension::ModuleId;
use crate::ErrorCode;
use std::rc::Rc;

fn compare(symbol: &str, a: i32, b: i32) -> bool {
    match symbol {
        "==" => a == b,
        "!=" => a != b,
        "<" => a < b,
        "<=" => a <= b,
        ">" => a > b,
        ">=" => a >= b,
        _ => unreachable!(),
    }
}

#[test]
fn fused_signed_comparisons_cover_both_paths_and_integer_boundaries() {
    for (symbol, stop_a, stop_b) in [
        ("==", 0, 1),
        ("!=", 0, 0),
        ("<", 1, 0),
        ("<=", 1, 0),
        (">", 0, 1),
        (">=", 0, 1),
    ] {
        let source = format!(
            r#"
            fn f(a: Int32, b: Int32) -> Int32 {{
                var x = a var y = b var result = 0
                while x {symbol} y {{ result = 1 x = {stop_a} y = {stop_b} }}
                result
            }}
        "#
        );
        let original = Rc::new(compile(&source, "f", false));
        let optimized = Rc::new(compile(&source, "f", true));
        assert!(optimized
            .code
            .iter()
            .any(|op| matches!(op, Instruction::BranchCompare { .. })));
        for a in [i32::MIN, -1, 0, 1, i32::MAX] {
            for b in [i32::MIN, -1, 0, 1, i32::MAX] {
                let expected = Ok(i32::from(compare(symbol, a, b)));
                assert_eq!(run(original.clone(), &[a, b]), expected);
                assert_eq!(
                    run(optimized.clone(), &[a, b]),
                    expected,
                    "{a} {symbol} {b}"
                );
            }
        }
    }
}

#[test]
fn fused_immediate_comparisons_and_materialized_bools_agree() {
    for symbol in ["==", "!=", "<", "<=", ">", ">="] {
        let stop = match symbol {
            "==" | "<=" => "1",
            "!=" | "<" | ">" => "0",
            _ => "(0 - 1)",
        };
        let source = format!(
            r#"
            fn f(a: Int32) -> Int32 {{
                var x = a var result = 0
                while x {symbol} 0 {{ result = 1 x = {stop} }}
                result
            }}
        "#
        );
        let optimized = Rc::new(compile(&source, "f", true));
        assert!(optimized
            .code
            .iter()
            .any(|op| matches!(op, Instruction::BranchCompareImm { .. })));
        for a in [i32::MIN, -1, 0, 1, i32::MAX] {
            assert_eq!(
                run(optimized.clone(), &[a]),
                Ok(i32::from(compare(symbol, a, 0)))
            );
        }
        // A comparison returned as Bool still uses BinaryImm; it is not a branch.
        let source = format!("fn f(a: Int32) -> Bool {{ a {symbol} 0 }}");
        let function = Rc::new(compile(&source, "f", true));
        assert!(matches!(
            &function.code[..],
            [Instruction::BinaryImm { .. }]
        ));
        let module = empty_module();
        for a in [i32::MIN, -1, 0, 1, i32::MAX] {
            let frame = Frame::new(function.clone(), [Value::Int32(a)].iter()).unwrap();
            assert!(
                matches!(Execution::new(frame, crate::vm::InterpreterLimits::default(), false).poll(&module), Poll::Complete(Value::Bool(value)) if value == compare(symbol, a, 0))
            );
        }
    }
}

fn empty_module() -> BytecodeModule {
    BytecodeModule {
        version: ModuleId(0),
        functions: HashMap::new(),
        callees: Vec::new(),
    }
}

#[test]
fn comparison_results_with_other_uses_stay_materialized() {
    let source = r#"
        fn f(a: Int32) -> Bool {
            var x = a
            let saved = x < 1
            while saved { x = x + 1 }
            saved
        }
    "#;
    let function = compile(source, "f", true);
    assert!(!function.code.iter().any(|op| matches!(
        op,
        Instruction::BranchCompare { .. } | Instruction::BranchCompareImm { .. }
    )));
    // Explicitly exercise an adjacent compare/branch whose Bool is also the
    // function result: adjacency alone does not allow dropping its write.
    let mut function = compile("fn f(a: Int32, b: Int32) -> Bool { a < b }", "f", false);
    function.code.push(Instruction::Branch {
        condition: function.result.unwrap(),
        then_pc: 2,
        else_pc: 2,
    });
    function
        .spans
        .resize(function.code.len(), function.info.span);
    fuse::comparison_branches(&mut function);
    assert_eq!(function.code.len(), 2);
    assert!(matches!(function.code[0], Instruction::Binary { .. }));
}

#[test]
fn comparison_fusion_preserves_branch_entries_and_terminal_targets() {
    let mut function = compile(
        "fn f(a: Int32, b: Int32) -> Int32 { var x = a x + b }",
        "f",
        false,
    );
    function.result = Some(0);
    function.code = vec![
        Instruction::Const {
            dst: 2,
            value: ConstantValue::Bool(false),
        },
        Instruction::Jump(3),
        Instruction::Binary {
            dst: 2,
            op: BinOp::Lt,
            lhs: 0,
            rhs: 1,
        },
        Instruction::Branch {
            condition: 2,
            then_pc: 4,
            else_pc: 5,
        },
        Instruction::Const {
            dst: 0,
            value: ConstantValue::Int32(99),
        },
    ];
    function
        .spans
        .resize(function.code.len(), function.info.span);
    fuse::comparison_branches(&mut function);
    assert!(matches!(function.code[3], Instruction::Branch { .. }));
    assert_eq!(run(Rc::new(function), &[42, 100]), Ok(42));

    // Entry into the comparison is safe; removal must map the old terminal PC.
    let mut function = compile(
        "fn f(a: Int32, b: Int32) -> Int32 { var x = a x + b }",
        "f",
        false,
    );
    function.result = Some(0);
    function.code = vec![
        Instruction::Jump(1),
        Instruction::Binary {
            dst: 2,
            op: BinOp::Lt,
            lhs: 0,
            rhs: 1,
        },
        Instruction::Branch {
            condition: 2,
            then_pc: 3,
            else_pc: 4,
        },
        Instruction::Const {
            dst: 0,
            value: ConstantValue::Int32(99),
        },
    ];
    function
        .spans
        .resize(function.code.len(), function.info.span);
    fuse::comparison_branches(&mut function);
    assert_eq!(function.code.len(), 3);
    assert!(matches!(
        function.code[1],
        Instruction::BranchCompare {
            then_pc: 2,
            else_pc: 3,
            ..
        }
    ));
    let function = Rc::new(function);
    assert_eq!(run(function.clone(), &[42, 100]), Ok(99));
    assert_eq!(run(function, &[42, 0]), Ok(42));
}

#[test]
fn bool_equality_matches_before_and_after_branch_fusion() {
    for (symbol, stop) in [("==", "!y"), ("!=", "y")] {
        let source = format!(
            r#"
            fn f(a: Bool, b: Bool) -> Int32 {{
                var x = a var y = b var result = 0
                while x {symbol} y {{ result = 1 x = {stop} }}
                result
            }}
        "#
        );
        for optimized in [false, true] {
            let function = Rc::new(compile(&source, "f", optimized));
            assert_eq!(
                function
                    .code
                    .iter()
                    .any(|op| matches!(op, Instruction::BranchCompare { .. })),
                optimized
            );
            for a in [false, true] {
                for b in [false, true] {
                    let frame =
                        Frame::new(function.clone(), [Value::Bool(a), Value::Bool(b)].iter())
                            .unwrap();
                    let result =
                        Execution::new(frame, crate::vm::InterpreterLimits::default(), false)
                            .poll(&empty_module());
                    let expected = i32::from(if symbol == "==" { a == b } else { a != b });
                    assert!(
                        matches!(result, Poll::Complete(Value::Int32(value)) if value == expected)
                    );
                }
            }
        }
        let source = format!("fn f(a: Bool, b: Bool) -> Bool {{ a {symbol} b }}");
        for optimized in [false, true] {
            let function = Rc::new(compile(&source, "f", optimized));
            assert!(function
                .code
                .iter()
                .any(|op| matches!(op, Instruction::Binary { .. })));
            for a in [false, true] {
                for b in [false, true] {
                    let frame =
                        Frame::new(function.clone(), [Value::Bool(a), Value::Bool(b)].iter())
                            .unwrap();
                    let result =
                        Execution::new(frame, crate::vm::InterpreterLimits::default(), false)
                            .poll(&empty_module());
                    let expected = if symbol == "==" { a == b } else { a != b };
                    assert!(
                        matches!(result, Poll::Complete(Value::Bool(value)) if value == expected)
                    );
                }
            }
        }
    }
}

#[test]
fn bool_equality_does_not_accept_mixed_types_or_bool_arithmetic() {
    for (instruction, args) in [
        (
            Instruction::Binary {
                dst: 0,
                op: BinOp::Eq,
                lhs: 0,
                rhs: 1,
            },
            [Value::Bool(true), Value::Int32(1)],
        ),
        (
            Instruction::Binary {
                dst: 0,
                op: BinOp::Add,
                lhs: 0,
                rhs: 1,
            },
            [Value::Bool(true), Value::Bool(false)],
        ),
        (
            Instruction::BranchCompare {
                op: Comparison::Eq,
                lhs: 0,
                rhs: 1,
                then_pc: 1,
                else_pc: 1,
            },
            [Value::Int32(0), Value::Bool(false)],
        ),
        (
            Instruction::Unary {
                dst: 0,
                op: crate::parser::ast::UnaryOp::Neg,
                operand: 0,
            },
            [Value::Bool(true), Value::Bool(false)],
        ),
        (
            Instruction::Unary {
                dst: 0,
                op: crate::parser::ast::UnaryOp::Not,
                operand: 0,
            },
            [Value::Int32(1), Value::Int32(0)],
        ),
    ] {
        let mut function = compile("fn f(a: Int32, b: Int32) -> Int32 { a }", "f", false);
        function.code = vec![instruction];
        let frame = Frame::new(Rc::new(function), args.iter()).unwrap();
        assert!(
            matches!(Execution::new(frame, crate::vm::InterpreterLimits::default(), false).poll(&empty_module()), Poll::Failed(error) if error.code() == ErrorCode::InvalidState)
        );
    }
}

#[test]
fn specialized_instructions_keep_operand_type_checks() {
    for instruction in [
        Instruction::BinaryImm {
            dst: 0,
            op: BinOp::Add,
            lhs: 0,
            rhs: 1,
        },
        Instruction::BranchCompare {
            op: Comparison::Lt,
            lhs: 0,
            rhs: 0,
            then_pc: 1,
            else_pc: 1,
        },
        Instruction::BranchCompareImm {
            op: Comparison::Lt,
            lhs: 0,
            rhs: 1,
            then_pc: 1,
            else_pc: 1,
        },
    ] {
        let mut function = compile("fn f(a: Int32) -> Int32 { a }", "f", false);
        function.code = vec![instruction];
        let frame = Frame::new(Rc::new(function), [Value::Bool(true)].iter()).unwrap();
        assert!(
            matches!(Execution::new(frame, crate::vm::InterpreterLimits::default(), false).poll(&empty_module()), Poll::Failed(error) if error.code() == ErrorCode::InvalidState)
        );
    }
}
