//! Manual bytecode inspection; excluded from the release interpreter.
use super::*;
use std::{collections::BTreeMap, mem::size_of};

fn describe(instruction: &Instruction) -> String {
    match instruction {
        Instruction::Copy { dst, src } => format!("Copy r{dst}, r{src}"),
        Instruction::Jump(pc) => format!("Jump {pc}"),
        Instruction::Branch {
            condition,
            then_pc,
            else_pc,
        } => {
            format!("Branch r{condition}, {then_pc}, {else_pc}")
        }
        Instruction::Const { dst, value } => match value {
            ConstantValue::Int32(value) => format!("Const r{dst}, {value}"),
            ConstantValue::Bool(value) => format!("Const r{dst}, {value}"),
            ConstantValue::String(value) => format!("Const r{dst}, {value:?}"),
        },
        Instruction::Binary { dst, op, lhs, rhs } => {
            format!("Binary {op:?} r{dst}, r{lhs}, r{rhs}")
        }
        Instruction::Unary { dst, op, operand } => {
            format!("Unary {op:?} r{dst}, r{operand}")
        }
        Instruction::BinaryImm { dst, op, lhs, rhs } => {
            format!("BinaryImm {op:?} r{dst}, r{lhs}, {rhs}")
        }
        Instruction::BranchCompare {
            op,
            lhs,
            rhs,
            then_pc,
            else_pc,
        } => {
            format!("BranchCompare {op:?} r{lhs}, r{rhs}, {then_pc}, {else_pc}")
        }
        Instruction::BranchCompareImm {
            op,
            lhs,
            rhs,
            then_pc,
            else_pc,
        } => {
            format!("BranchCompareImm {op:?} r{lhs}, {rhs}, {then_pc}, {else_pc}")
        }
        Instruction::RecordCall(call) => format!("RecordCall #{}", call.target),
        Instruction::Call { target, .. } => format!("Call #{target}"),
        Instruction::Scope { branches } => format!("Scope {} branches", branches.len()),
    }
}

#[test]
#[ignore = "Print benchmark bytecode and storage sizes for manual inspection"]
fn dump_while_bytecode() {
    let ast = crate::parser::parse(include_str!("../../../benches/programs/while.jk")).unwrap();
    let plan =
        ModulePlan::with_import_layouts_and_types(&ast, &HashMap::new(), &TypeRegistry::default())
            .unwrap();
    println!(
        "Instruction bytes={} Value bytes={}",
        size_of::<Instruction>(),
        size_of::<Value>()
    );
    for name in ["while_sum", "while_recurrence", "while_sequence"] {
        let function =
            lower_function(&plan.functions[name], &plan.bodies[name], &HashMap::new()).unwrap();
        println!(
            "\n{name}: instructions={} slots={} slot_bytes={} result={:?}",
            function.code.len(),
            function.info.frame_slots,
            function.info.frame_slots * size_of::<Value>(),
            function.result
        );
        for (pc, instruction) in function.code.iter().enumerate() {
            println!("  {pc:02}: {}", describe(instruction));
        }
        for (end, instruction) in function.code.iter().enumerate() {
            if let Instruction::Jump(start) = instruction {
                if *start < end {
                    let mut counts = BTreeMap::<String, usize>::new();
                    for op in &function.code[*start..=end] {
                        let text = describe(op);
                        *counts
                            .entry(text.split_whitespace().next().unwrap().into())
                            .or_default() += 1;
                    }
                    // These particular kernels have one straight-line body.
                    // This count is not a general control-flow path analysis.
                    println!(
                        "  loop {start}..={end}: dispatches={} counts={counts:?}",
                        end - start + 1
                    );
                }
            }
        }
    }
}
