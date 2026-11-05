//! Backward may-liveness. Only block summaries span the full slot universe.
use super::*;

struct Block {
    start: usize,
    end: usize,
    successors: Vec<usize>,
    predecessors: Vec<usize>,
    uses: Vec<u64>,
    defs: Vec<u64>,
    input: Vec<u64>,
}

fn limit(span: Span) -> Error {
    type_error(&span, ErrorCode::ResourceLimit,
        "Reference liveness exceeds its compilation budget; split the function into smaller functions")
}

pub(super) fn successors(plan: &FunctionPlan) -> Vec<Vec<usize>> {
    let n = plan.ops.len();
    let mut labels = vec![n; plan.label_count];
    for (pc, op) in plan.ops.iter().enumerate() {
        if let Op::Label(label) = op {
            labels[*label] = pc;
        }
    }
    plan.ops
        .iter()
        .enumerate()
        .map(|(pc, op)| match op {
            Op::Panic { .. } => vec![],
            Op::Jump(label) => vec![labels[*label]],
            Op::Branch {
                then_label,
                else_label,
                ..
            } => vec![labels[*then_label], labels[*else_label]],
            _ => vec![pc + 1],
        })
        .collect()
}

pub(super) fn solve(
    plan: &FunctionPlan,
    next: &[Vec<usize>],
    types: &BTreeMap<usize, Type>,
) -> Result<Vec<Set>, Error> {
    let n = next.len();
    let mut leaders = Set::from([0, n]);
    for (pc, successors) in next.iter().enumerate() {
        if successors.as_slice() != [pc + 1] {
            leaders.insert(pc + 1);
            leaders.extend(successors);
        }
    }
    let starts: Vec<_> = leaders.into_iter().collect();
    let indices: HashMap<_, _> = types
        .keys()
        .enumerate()
        .map(|(bit, &slot)| (slot, bit))
        .collect();
    let words = types.len().div_ceil(64);
    // Three dense rows per block, plus the temporary union in the solver.
    if starts.len().saturating_mul(words) > 1_000_000 {
        return Err(limit(plan.span));
    }
    let mut blocks = Vec::with_capacity(starts.len());
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(n);
        let mut uses = vec![0; words];
        let mut defs = vec![0; words];
        for op in &plan.ops[start..end] {
            let (read, written) = operands(op);
            for slot in read {
                let bit = indices[&slot.index];
                let mask = 1 << (bit % 64);
                uses[bit / 64] |= mask & !defs[bit / 64];
            }
            for slot in written {
                let bit = indices[&slot.index];
                defs[bit / 64] |= 1 << (bit % 64);
            }
        }
        let successors = if start == n {
            for slot in plan.result.iter().flat_map(refs) {
                let bit = indices[&slot.index];
                uses[bit / 64] |= 1 << (bit % 64);
            }
            Vec::new()
        } else {
            next[end - 1]
                .iter()
                .map(|pc| starts.binary_search(pc).unwrap())
                .collect()
        };
        blocks.push(Block {
            start,
            end,
            successors,
            predecessors: Vec::new(),
            uses,
            defs,
            input: vec![0; words],
        });
    }
    for i in 0..blocks.len() {
        for successor in blocks[i].successors.clone() {
            blocks[successor].predecessors.push(i);
        }
    }
    let mut budget = 2_000_000 + 32 * n;
    let mut queue: VecDeque<_> = (0..blocks.len()).rev().collect();
    let mut queued = vec![true; blocks.len()];
    while let Some(i) = queue.pop_front() {
        queued[i] = false;
        let block = &blocks[i];
        budget = budget
            .checked_sub(words * (1 + block.successors.len()))
            .ok_or_else(|| limit(plan.op_spans.get(block.start).copied().unwrap_or(plan.span)))?;
        let mut input = block.uses.clone();
        for &s in &block.successors {
            for ((dst, &out), &kill) in input.iter_mut().zip(&blocks[s].input).zip(&block.defs) {
                *dst |= out & !kill;
            }
        }
        if blocks[i].input != input {
            blocks[i].input = input;
            for &pred in &blocks[i].predecessors {
                if !queued[pred] {
                    queue.push_back(pred);
                    queued[pred] = true;
                }
            }
        }
    }
    let slots: Vec<_> = types.keys().copied().collect();
    let mut live = vec![Set::new(); n + 1];
    live[n].extend(plan.result.iter().flat_map(refs).map(|s| s.index));
    // Bound the number of materialized sparse entries across instruction snapshots.
    let mut entries = 1_000_000usize;
    for block in &blocks {
        let mut current = Set::new();
        for &s in &block.successors {
            for (word, &bits) in blocks[s].input.iter().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    current.insert(slots[64 * word + bits.trailing_zeros() as usize]);
                    bits &= bits - 1;
                }
            }
        }
        for pc in (block.start..block.end).rev() {
            let (uses, defs) = operands(&plan.ops[pc]);
            for dst in defs {
                current.remove(&dst.index);
            }
            current.extend(uses.iter().map(|s| s.index));
            entries = entries
                .checked_sub(current.len())
                .ok_or_else(|| limit(plan.op_spans[pc]))?;
            live[pc] = current.clone();
        }
    }
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{prepared::PreparedModule, resolver::BindingRegistry};

    #[test]
    fn block_liveness_matches_instruction_fixed_point() {
        let ast = crate::parser::parse(
            r#"
            fn choose(x:&Int32, y:&Int32, n:Int32) -> &Int32 {
                var i = 0
                while i < n { if i == *y { return x }; i += 1 }
                y
            }
            fn read(x:&Int32) @suspends -> Int32 { *x }
            fn both(x:&Int32, y:&Int32) @suspends -> Int32 {
                let (a,b) = join { read(x), read(y) }
                a + b
            }
        "#,
        )
        .unwrap();
        let registry = TypeRegistry::new().with_prelude(false);
        let module = PreparedModule::new(&ast, &registry, &BindingRegistry::default())
            .unwrap()
            .plan(&HashMap::new())
            .unwrap();
        for name in ["choose", "both"] {
            let plan = &module.bodies[name];
            let next = successors(plan);
            let types = plan
                .ops
                .iter()
                .flat_map(|op| {
                    let (uses, defs) = operands(op);
                    uses.into_iter().chain(defs)
                })
                .chain(plan.result.iter().flat_map(refs))
                .map(|s| (s.index, s.ty))
                .collect();
            let n = next.len();
            let mut expected = vec![Set::new(); n + 1];
            expected[n].extend(plan.result.iter().flat_map(refs).map(|s| s.index));
            loop {
                let mut changed = false;
                for pc in (0..n).rev() {
                    let mut set: Set = next[pc]
                        .iter()
                        .flat_map(|&s| expected[s].iter().copied())
                        .collect();
                    let (uses, defs) = operands(&plan.ops[pc]);
                    for dst in defs {
                        set.remove(&dst.index);
                    }
                    set.extend(uses.iter().map(|s| s.index));
                    if set != expected[pc] {
                        changed = true;
                        expected[pc] = set;
                    }
                }
                if !changed {
                    break;
                }
            }
            assert_eq!(solve(plan, &next, &types).unwrap(), expected, "{name}");
        }
    }
}
