//! Conservative bytecode-local optimizations; shared typed IR stays unchanged.
use super::{BytecodeFunction, ConstantValue, Instruction};
use std::collections::HashMap;

#[cfg(test)]
mod tests;

pub(super) fn optimize(function: &mut BytecodeFunction) {
    let code = &mut function.code;
    let mut boundaries = vec![false; code.len() + 1];
    boundaries[0] = true;
    for (pc, instruction) in code.iter().enumerate() {
        match instruction {
            Instruction::Jump(target) => {
                boundaries[*target] = true;
                boundaries[pc + 1] = true;
            }
            Instruction::Branch {
                then_pc, else_pc, ..
            } => {
                boundaries[*then_pc] = true;
                boundaries[*else_pc] = true;
                boundaries[pc + 1] = true;
            }
            // Do not propagate snapshots across calls, suspension or scope.
            Instruction::Call { .. } | Instruction::Scope { .. } => {
                boundaries[pc] = true;
                boundaries[pc + 1] = true;
            }
            _ => {}
        }
    }

    let mut versions = vec![0usize; function.info.frame_slots];
    let mut copies = HashMap::<usize, (usize, usize)>::new();
    for (pc, instruction) in code.iter_mut().enumerate() {
        if boundaries[pc] {
            copies.clear();
        }
        // Record the source's current version. A later write invalidates the
        // fact without changing snapshots already stored. One lookup is enough
        // for this conservative pass; it does not chase longer alias chains.
        if !matches!(
            instruction,
            Instruction::Call { .. } | Instruction::Scope { .. }
        ) {
            visit_reads(instruction, |slot| {
                if let Some(&(source, version)) = copies.get(slot) {
                    if versions[source] == version {
                        *slot = source;
                    }
                }
            });
        }
        visit_writes(instruction, |slot| {
            versions[slot] += 1;
            copies.remove(&slot);
        });
        if let Instruction::Copy { dst, src } = instruction {
            if dst != src {
                copies.insert(*dst, (*src, versions[*src]));
            }
        }
    }

    // Count every static read, including other blocks, loop backedges, call
    // arguments and the implicit return. This deliberately overestimates
    // liveness: a snapshot used outside its block must remain materialized.
    let mut reads = vec![0usize; function.info.frame_slots];
    let mut writes = vec![0usize; function.info.frame_slots];
    for instruction in code.iter_mut() {
        visit_reads(instruction, |slot| reads[*slot] += 1);
        visit_writes(instruction, |slot| writes[slot] += 1);
    }
    if let Some(result) = function.result {
        reads[result] += 1;
    }
    let mut keep = vec![true; code.len()];
    for (pc, instruction) in code.iter().enumerate().rev() {
        if let Instruction::Copy { dst, src } = instruction {
            if dst == src || reads[*dst] == 0 {
                keep[pc] = false;
                reads[*src] -= 1;
                writes[*dst] -= 1;
            }
        }
    }

    // Redirect a scalar producer followed by its sole consuming copy. No
    // instruction, control-flow entry or resume boundary may intervene. The
    // evaluator reads both operands (and checks division) before writing dst,
    // so an in-place assignment preserves both snapshots and failure order.
    let mut previous = None;
    for pc in 0..code.len() {
        if boundaries[pc] {
            previous = None;
        }
        if !keep[pc] {
            continue;
        }
        if let (Some(producer_pc), Instruction::Copy { dst, src }) = (previous, &code[pc]) {
            let (dst, src) = (*dst, *src);
            if reads[src] == 1 && writes[src] == 1 {
                if let Some(target) = scalar_destination(&mut code[producer_pc]) {
                    if *target == src {
                        *target = dst;
                        keep[pc] = false;
                        reads[src] -= 1;
                        writes[src] -= 1;
                        continue;
                    }
                }
            }
        }
        previous = Some(pc);
    }

    // Every old PC, including removed instructions and code.len(), maps to the
    // next surviving instruction. Resume uses the final immutable instruction
    // stream and never observes the intermediate PCs.
    let mut remap = Vec::with_capacity(code.len() + 1);
    let mut next = 0;
    for &live in &keep {
        remap.push(next);
        next += usize::from(live);
    }
    remap.push(next);
    let mut pc = 0;
    code.retain(|_| {
        let live = keep[pc];
        pc += 1;
        live
    });
    for instruction in code {
        match instruction {
            Instruction::Jump(target) => *target = remap[*target],
            Instruction::Branch {
                then_pc, else_pc, ..
            } => {
                *then_pc = remap[*then_pc];
                *else_pc = remap[*else_pc];
            }
            _ => {}
        }
    }
}

fn scalar_destination(instruction: &mut Instruction) -> Option<&mut usize> {
    match instruction {
        Instruction::Binary { dst, .. }
        | Instruction::Const {
            dst,
            value: ConstantValue::Int32(_) | ConstantValue::Bool(_),
        } => Some(dst),
        _ => None,
    }
}

fn visit_reads(instruction: &mut Instruction, mut visit: impl FnMut(&mut usize)) {
    match instruction {
        Instruction::Copy { src, .. } => visit(src),
        Instruction::Branch { condition, .. } => visit(condition),
        Instruction::Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        Instruction::Call { args, .. } => {
            for slot in args.iter_mut().flatten() {
                visit(slot);
            }
        }
        Instruction::Scope { branches } => {
            for branch in branches {
                for slot in branch.args.iter_mut().flatten() {
                    visit(slot);
                }
            }
        }
        Instruction::Const { .. } | Instruction::FunctionRef { .. } | Instruction::Jump(_) => {}
    }
}

fn visit_writes(instruction: &Instruction, mut visit: impl FnMut(usize)) {
    match instruction {
        Instruction::Copy { dst, .. }
        | Instruction::Const { dst, .. }
        | Instruction::FunctionRef { dst }
        | Instruction::Binary { dst, .. } => visit(*dst),
        Instruction::Call { result, .. } => {
            if let Some(result) = result {
                visit(*result);
            }
        }
        Instruction::Scope { branches } => {
            for branch in branches {
                visit(branch.done);
            }
        }
        Instruction::Branch { .. } | Instruction::Jump(_) => {}
    }
}
