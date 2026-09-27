use super::*;
use crate::backends::interp::{
    lower_function, lower_unoptimized, poll_frame, BytecodeModule, Frame, Poll, Value,
};
use crate::compiler::ir::ModulePlan;
use crate::{ErrorCode, TypeRegistry};
use std::rc::Rc;

pub(super) fn compile(source: &str, name: &str, optimized: bool) -> BytecodeFunction {
    let ast = crate::parser::parse(source).unwrap();
    let plan =
        ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &TypeRegistry::default())
            .unwrap();
    let lower = if optimized {
        lower_function
    } else {
        lower_unoptimized
    };
    lower(&plan.functions[name], &plan.bodies[name]).unwrap()
}

pub(super) fn run(function: Rc<BytecodeFunction>, args: &[i32]) -> Result<i32, ErrorCode> {
    let module = BytecodeModule {
        version: crate::runtime::suspension::ModuleId(0),
        functions: HashMap::new(),
        hosts: HashMap::new(),
        _dependencies: Vec::new(),
    };
    let args: Vec<_> = args.iter().copied().map(Value::Int32).collect();
    let mut frame = Frame::new(function, &args).unwrap();
    match poll_frame(&module, &mut frame) {
        Poll::Complete(Value::Int32(value)) => Ok(value),
        Poll::Failed(error) => Err(error.code()),
        _ => panic!("Expected an Int32 result or a structured failure"),
    }
}

#[test]
fn scalar_assignments_eliminate_snapshot_and_writeback_copies() {
    let function = compile(
        "fn f(n: Int32) -> Int32 { var x = n var i = 0 while i < 1 { x = x + x i = i + 1 } x }",
        "f",
        true,
    );
    // The initializer and return still cross our deliberately narrow rules.
    // The arithmetic itself must read and write the same mutable slot.
    assert!(function.code.iter().any(|instruction| matches!(
        instruction,
        Instruction::Binary { dst, lhs, rhs, .. } if dst == lhs && lhs == rhs
    )));
    assert_eq!(
        function
            .code
            .iter()
            .filter(|op| matches!(op, Instruction::Copy { .. }))
            .count(),
        2
    );
    let function = Rc::new(function);
    for n in [0, 1, -1, i32::MIN, i32::MAX] {
        assert_eq!(run(function.clone(), &[n]), Ok(n.wrapping_add(n)));
    }
}

#[test]
fn copy_facts_expire_when_sources_change_or_control_flow_merges() {
    let source = r#"
        fn f(seed: Int32, n: Int32) -> Int32 {
            var x = seed
            let before = x
            var y = x
            x = x + 1
            let middle = y
            y = x
            x = x + 1
            var i = 0
            while i < n {
                let saved = x
                var j = 0
                while j < 2 {
                    x = x + y
                    j = j + 1
                }
                y = saved + { x = x + 1 x }
                i = i + 1
            }
            before + middle + x + y
        }
    "#;
    let original = Rc::new(compile(source, "f", false));
    let optimized = Rc::new(compile(source, "f", true));
    for seed in [0, 1, -7, i32::MIN, i32::MAX] {
        for n in [-1, 0, 1, 2, 7] {
            assert_eq!(
                run(original.clone(), &[seed, n]),
                run(optimized.clone(), &[seed, n])
            );
        }
    }
}

#[test]
fn direct_destinations_keep_other_uses_of_the_temporary() {
    let source = r#"
        fn f(seed: Int32) -> Int32 {
            var x = seed
            let calculated = x + 1
            x = calculated
            x = x + 2
            calculated + x
        }
    "#;
    let function = Rc::new(compile(source, "f", true));
    for seed in [0, -9, i32::MIN, i32::MAX] {
        assert_eq!(
            run(function.clone(), &[seed]),
            Ok(seed.wrapping_add(1).wrapping_add(seed.wrapping_add(3)))
        );
    }
}

#[test]
fn optimized_division_keeps_failures_and_signed_wrapping() {
    let source = "fn f(a: Int32, b: Int32) -> Int32 { var x = a var i = 0 while i < 1 { x = x / b i = i + 1 } x }";
    let function = Rc::new(compile(source, "f", true));
    assert_eq!(run(function.clone(), &[i32::MIN, -1]), Ok(i32::MIN));
    assert_eq!(run(function.clone(), &[-7, 2]), Ok(-3));
    assert_eq!(run(function, &[12, 0]), Err(ErrorCode::RuntimeError));
    // A discarded result must still trap; this pass only removes dead copies.
    let function = Rc::new(compile("fn f() -> Int32 { 1 / 0; 42 }", "f", true));
    assert_eq!(run(function, &[]), Err(ErrorCode::RuntimeError));
}

#[test]
fn copy_removal_remaps_branch_entries_and_terminal_targets() {
    let mut function = compile("fn f(seed: Int32) -> Int32 { seed }", "f", false);
    function.code = vec![
        Instruction::Jump(2),
        Instruction::Const {
            dst: 0,
            value: ConstantValue::Int32(99),
        },
        Instruction::Copy { dst: 0, src: 0 },
        Instruction::Jump(4),
        Instruction::Copy { dst: 0, src: 0 },
    ];
    optimize_copies(&mut function);
    assert_eq!(function.code.len(), 3);
    assert!(matches!(function.code[0], Instruction::Jump(2)));
    assert!(matches!(function.code[2], Instruction::Jump(3)));
    assert_eq!(run(Rc::new(function), &[42]), Ok(42));
}

#[test]
fn direct_destinations_do_not_cross_jump_targets() {
    let mut function = compile("fn f(seed: Int32) -> Int32 { var x = seed x }", "f", false);
    function.code = vec![
        Instruction::Jump(2),
        Instruction::Const {
            dst: 0,
            value: ConstantValue::Int32(99),
        },
        Instruction::Copy { dst: 1, src: 0 },
    ];
    function.result = Some(1);
    optimize_copies(&mut function);
    assert!(matches!(
        function.code[2],
        Instruction::Copy { dst: 1, src: 0 }
    ));
    assert_eq!(run(Rc::new(function), &[42]), Ok(42));
}

#[test]
fn while_kernels_reduce_dispatches_and_match_unoptimized_execution() {
    let source = include_str!("../../../../benches/programs/while.jk");
    for (name, expected) in [
        ("while_sum", 4),
        ("while_recurrence", 6),
        ("while_sequence", 6),
    ] {
        let original = Rc::new(compile(source, name, false));
        let optimized = Rc::new(compile(source, name, true));
        let loop_size = optimized
            .code
            .iter()
            .enumerate()
            .find_map(|(end, instruction)| {
                if let Instruction::Jump(start) = instruction {
                    if *start < end {
                        return Some(end - start + 1);
                    }
                }
                None
            });
        assert_eq!(loop_size, Some(expected), "{name}");
        for seed in [0, 7, -17, i32::MIN, i32::MAX] {
            for n in [-1, 0, 1, 9, 127] {
                assert_eq!(
                    run(original.clone(), &[n, seed]),
                    run(optimized.clone(), &[n, seed]),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn generated_mutation_sequences_match_unoptimized_execution() {
    // Deterministic, dependency-free differential coverage of aliases, chains,
    // overwrites, saved values and operand-side assignments within one block.
    let mut state = 0x5267_u32;
    for case in 0..96 {
        let mut source = String::from("fn f(seed: Int32) -> Int32 { var a = seed var b = seed + 1 var c = seed - 1 let saved = a\n");
        for _ in 0..24 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            source.push_str(match (state >> 16) % 8 {
                0 => "a = b\n",
                1 => "b = a + c\n",
                2 => "c = b + { b = b + 1 b }\n",
                3 => "a = a + { a = c c }\n",
                4 => "b = c\n",
                5 => "c = c * 3\n",
                6 => "{ let old = a a = b b = old }\n",
                _ => "{ let old = b b = a c = old + c }\n",
            });
        }
        source.push_str("a + b + c + saved }");
        let original = Rc::new(compile(&source, "f", false));
        let optimized = Rc::new(compile(&source, "f", true));
        for seed in [0, 1, -17, i32::MIN, i32::MAX] {
            assert_eq!(
                run(original.clone(), &[seed]),
                run(optimized.clone(), &[seed]),
                "case={case} seed={seed}\n{source}"
            );
        }
    }
}

#[test]
fn immediate_arithmetic_preserves_order_wrapping_and_division_errors() {
    use crate::parser::ast::BinOp;
    for (symbol, op) in [
        ("+", BinOp::Add),
        ("-", BinOp::Sub),
        ("*", BinOp::Mul),
        ("/", BinOp::Div),
    ] {
        for rhs in [i32::MIN, -1, 0, 1, 2, i32::MAX] {
            let source = format!("fn f(a: Int32) -> Int32 {{ a {symbol} 7 }}");
            let mut function = compile(&source, "f", false);
            // Exercise the full i32 immediate range without requiring a new
            // source literal syntax or folding the expression `0 - 1`.
            for instruction in &mut function.code {
                if let Instruction::Const {
                    value: ConstantValue::Int32(value),
                    ..
                } = instruction
                {
                    *value = rhs;
                }
            }
            optimize(&mut function);
            assert!(
                matches!(&function.code[..], [Instruction::BinaryImm { op: actual, rhs: value, .. }] if *actual == op && *value == rhs)
            );
            let function = Rc::new(function);
            for lhs in [i32::MIN, -7, -1, 0, 1, 7, i32::MAX] {
                let expected = match op {
                    BinOp::Add => Ok(lhs.wrapping_add(rhs)),
                    BinOp::Sub => Ok(lhs.wrapping_sub(rhs)),
                    BinOp::Mul => Ok(lhs.wrapping_mul(rhs)),
                    BinOp::Div if rhs == 0 => Err(ErrorCode::RuntimeError),
                    BinOp::Div => Ok(lhs.wrapping_div(rhs)),
                    _ => unreachable!(),
                };
                assert_eq!(
                    run(function.clone(), &[lhs]),
                    expected,
                    "{lhs} {symbol} {rhs}"
                );
            }
        }
    }
    for (symbol, divide) in [("-", false), ("/", true)] {
        let function = Rc::new(compile(
            &format!("fn f(a: Int32) -> Int32 {{ 7 {symbol} a }}"),
            "f",
            true,
        ));
        for rhs in [-7_i32, -1, 0, 1, 2, i32::MAX] {
            let expected = if divide {
                if rhs == 0 {
                    Err(ErrorCode::RuntimeError)
                } else {
                    Ok(7_i32.wrapping_div(rhs))
                }
            } else {
                Ok(7_i32.wrapping_sub(rhs))
            };
            assert_eq!(run(function.clone(), &[rhs]), expected);
        }
    }
}

#[test]
fn immediate_facts_expire_at_writes_and_control_flow_boundaries() {
    let source = r#"
        fn f(n: Int32) -> Int32 {
            var k = 3
            var x = 10
            var i = 0
            while i < n { x = x + k k = k + 1 i = i + 1 }
            let saved = k
            k = k + 2
            x = x + k
            x + saved
        }
    "#;
    let original = Rc::new(compile(source, "f", false));
    let optimized = Rc::new(compile(source, "f", true));
    for n in [-1, 0, 1, 7, 100] {
        assert_eq!(run(original.clone(), &[n]), run(optimized.clone(), &[n]));
    }
    // The literal must survive when its only remaining use is the return.
    let function = Rc::new(compile(
        "fn f(a: Int32) -> Int32 { let k = 7 a + k k }",
        "f",
        true,
    ));
    assert_eq!(run(function, &[123]), Ok(7));
}

#[test]
fn immediate_facts_do_not_cross_calls_or_scope() {
    let source = r#"
        extern fn host() -> Int32
        extern fn pending() @suspends
        fn f(a: Int32) @suspends -> Int32 {
            let k = 7
            host()
            let x = a + k
            let m = 9
            scope { | pending() }
            x + m
        }
    "#;
    let function = compile(source, "f", true);
    assert!(!function
        .code
        .iter()
        .any(|op| matches!(op, Instruction::BinaryImm { .. })));
}
