//! Manual bytecode inspection; excluded from the release interpreter.
use super::*;
use std::{collections::BTreeMap, mem::size_of};

fn describe(instruction: &Instruction) -> String {
    match instruction {
        Instruction::DynCopy { lanes } => format!("DynCopy {lanes:?}"),
        Instruction::DynMethod { method, .. } => format!("DynMethod #{method}"),
        Instruction::Text { .. } => "text".into(),
        Instruction::DomainBits {
            dst, src, decode, ..
        } => format!("DomainBits r{dst}, r{src}, decode={decode}"),
        Instruction::Float { .. } => "float".into(),
        Instruction::Assert { condition, message } => format!("assert %{condition}, %{message}"),
        Instruction::Panic { message } => format!("panic %{message}"),
        Instruction::IndirectCall(call) => {
            format!("IndirectCall r{}, {:?}", call.callee, call.args)
        }
        Instruction::Cleanup { .. } => "Cleanup".into(),
        Instruction::Place(_) => "Place".into(),
        Instruction::Memory(_) => "Memory".into(),
        Instruction::Buffer(_) => "Buffer".into(),
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
            ConstantValue::DynTable(table) => {
                format!("Const r{dst}, dyn table ({} slots)", table.entries.len())
            }
            ConstantValue::Function(target) => format!("Const r{dst}, fn#{target}"),
            ConstantValue::NullPtr => format!("Const r{dst}, Ptr.null()"),
            ConstantValue::EmptyReference => format!("EmptyReference r{dst}"),
            ConstantValue::Int64(value) => format!("Const64 r{dst}, {value}"),
            ConstantValue::Int8(value) => format!("ConstI8 r{dst}, {value}"),
            ConstantValue::Int16(value) => format!("ConstI16 r{dst}, {value}"),
            ConstantValue::UInt8(value) => format!("ConstU8 r{dst}, {value}"),
            ConstantValue::UInt16(value) => format!("ConstU16 r{dst}, {value}"),
            ConstantValue::UInt32(value) => format!("ConstU32 r{dst}, {value}"),
            ConstantValue::Rune(value) => format!("ConstRune r{dst}, {value}"),
            ConstantValue::UInt64(value) => format!("ConstU64 r{dst}, {value}"),
            ConstantValue::Float32(value) => format!("Const r{dst}, {value:?}f32"),
            ConstantValue::Float64(value) => format!("Const r{dst}, {value:?}f64"),
            ConstantValue::Int32(value) => format!("Const r{dst}, {value}"),
            ConstantValue::Bool(value) => format!("Const r{dst}, {value}"),
            ConstantValue::String(value) => format!("Const r{dst}, {value:?}"),
        },
        Instruction::Binary { dst, op, lhs, rhs } => {
            format!("Binary {op:?} r{dst}, r{lhs}, r{rhs}")
        }
        Instruction::Convert {
            dst,
            target,
            operand,
        } => {
            format!("Convert {target:?} r{dst}, r{operand}")
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
        Instruction::AggregateCall(call) => format!("AggregateCall #{}", call.target),
        Instruction::Call { target, .. } => format!("Call #{target}"),
        Instruction::Scope {
            kind,
            branches,
            result,
            results,
        } => {
            let name = if *kind == ScopeKind::Race {
                "Race"
            } else {
                "Scope"
            };
            format!(
                "{name} {} branches -> {result:?} {results:?}",
                branches.len()
            )
        }
        Instruction::ScopeFinish => "ScopeFinish".into(),
        Instruction::TaskScopeEnter => "TaskScopeEnter".into(),
        Instruction::TaskBranch { target, args } => {
            let inputs: Vec<_> = args
                .iter()
                .map(|slot| slot.map_or('%'.to_string(), |slot| format!("r{slot}")))
                .collect();
            format!("TaskBranch #{target} {}", inputs.join(", "))
        }
        Instruction::TaskJoin => "TaskJoin".into(),
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
        let function = lower_function(
            &plan.functions[name],
            &plan.bodies[name],
            &HashMap::new(),
            &mut HashMap::new(),
            &mut Default::default(),
        )
        .unwrap();
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
