//! Local instruction specialization after copy elimination.
use super::{block_boundaries, compact_instructions, slot_counts, visit_writes};
use crate::backends::interp::{BytecodeFunction, Comparison, ConstantValue, Instruction};
use std::collections::HashMap;

pub(super) fn immediates(function: &mut BytecodeFunction) {
    let boundaries = block_boundaries(&function.code);
    let mut constants = HashMap::<usize, i32>::new();
    for (pc, instruction) in function.code.iter_mut().enumerate() {
        if boundaries[pc] {
            constants.clear();
        }
        // Substitute only a proven right-hand integer constant, before
        // invalidating the destination. Subtraction/division keep operand order;
        // no arithmetic is folded or moved across a call or resume boundary.
        if let Instruction::Binary { dst, op, lhs, rhs } = instruction {
            if let Some(&value) = constants.get(rhs) {
                *instruction = Instruction::BinaryImm {
                    dst: *dst,
                    op: op.clone(),
                    lhs: *lhs,
                    rhs: value,
                };
            }
        }
        visit_writes(instruction, |slot| {
            constants.remove(&slot);
        });
        if let Instruction::Const {
            dst,
            value: ConstantValue::Int32(value),
        } = instruction
        {
            constants.insert(*dst, *value);
        }
    }

    // Keep constants that still feed returns, other blocks or non-immediate
    // consumers. Removing an unused integer load cannot discard a runtime error.
    let (reads, _) = slot_counts(function);
    let keep: Vec<_> = function
        .code
        .iter()
        .map(|instruction| {
            !matches!(instruction, Instruction::Const {
            dst, value: ConstantValue::Int32(_),
        } if reads[*dst] == 0)
        })
        .collect();
    compact_instructions(function, &keep);
}

pub(super) fn comparison_branches(function: &mut BytecodeFunction) {
    let boundaries = block_boundaries(&function.code);
    let (reads, writes) = slot_counts(function);
    let code = &mut function.code;
    let mut keep = vec![true; code.len()];
    for pc in 1..code.len() {
        // An entry into the Branch itself must still consume its existing Bool,
        // and a result used anywhere else must remain materialized. Requiring
        // adjacency also preserves snapshots and suspension/evaluation order.
        if boundaries[pc] {
            continue;
        }
        let Instruction::Branch {
            condition,
            then_pc,
            else_pc,
        } = &code[pc]
        else {
            continue;
        };
        if reads[*condition] != 1 || writes[*condition] != 1 {
            continue;
        }
        let fused = match &code[pc - 1] {
            Instruction::Binary { dst, op, lhs, rhs } if dst == condition => {
                Comparison::from_binary(op).map(|op| Instruction::BranchCompare {
                    op,
                    lhs: *lhs,
                    rhs: *rhs,
                    then_pc: *then_pc,
                    else_pc: *else_pc,
                })
            }
            Instruction::BinaryImm { dst, op, lhs, rhs } if dst == condition => {
                Comparison::from_binary(op).map(|op| Instruction::BranchCompareImm {
                    op,
                    lhs: *lhs,
                    rhs: *rhs,
                    then_pc: *then_pc,
                    else_pc: *else_pc,
                })
            }
            _ => None,
        };
        if let Some(fused) = fused {
            code[pc - 1] = fused;
            keep[pc] = false;
        }
    }
    compact_instructions(function, &keep);
}
