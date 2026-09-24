use super::*;

pub(super) struct FunctionFlow {
    pub(super) reachable: HashSet<MirBlockId>,
    pub(super) reverse_postorder: Vec<MirBlockId>,
    pub(super) predecessors: Vec<Vec<(MirBlockId, Vec<MirValueId>)>>,
    pub(super) dominators: Vec<IndexSet<MirBlockId>>,
    pub(super) available: Vec<IndexSet<MirValueId>>,
}

/// Build the CFG facts shared by statement, phi and terminator validation.
pub(super) fn prepare_flow(function: &MirFunction) -> Result<FunctionFlow, Diagnostic> {
    let reachable = reachable_blocks(function)?;
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    let mut all_defs = HashSet::new();
    let mut block_definitions = vec![Vec::new(); function.blocks.len()];
    for block_id in &reachable {
        let block = &function.blocks[block_id.0];
        for statement in &block.statements {
            let Some(destination) = statement_destination(statement) else {
                continue;
            };
            if !all_defs.insert(destination) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' defines a value more than once",
                    function.name
                )));
            }
            // Leave malformed IDs to the statement/continuation checks so
            // structural diagnostics keep their precedence. Never size a
            // dense set from untrusted IDs.
            if destination.0 < function.value_types.len() {
                block_definitions[block_id.0].push(destination);
            }
        }
        let terminator = block.terminator.as_ref().ok_or_else(|| {
            Diagnostic::codegen(format!(
                "MIR function '{}' has an unterminated reachable block",
                function.name
            ))
        })?;
        for (target, arguments) in terminator_edges(terminator) {
            predecessors[target.0].push((*block_id, arguments));
        }
    }

    let mut reverse_postorder = Vec::with_capacity(reachable.len());
    helpers::rpo_sequence(
        function.entry,
        function,
        &mut HashSet::new(),
        &mut reverse_postorder,
    );
    reverse_postorder.reverse();
    let dominators = compute_dominators(function, &reverse_postorder);
    let mut available = vec![IndexSet::empty(function.value_types.len()); function.blocks.len()];
    let mut order = vec![0; function.blocks.len()];
    for (index, block) in reverse_postorder.iter().enumerate() {
        order[block.0] = index;
    }
    for &block in &reverse_postorder {
        // Strict dominators form a chain. Its last block in reverse postorder
        // is the immediate dominator, whose available definitions are ready.
        // Inherit them once instead of scanning every dominating block's body.
        let parent = dominators[block.0]
            .iter()
            .filter(|dominator| *dominator != block)
            .max_by_key(|dominator| order[dominator.0]);
        let mut definitions = parent
            .map(|parent| available[parent.0].clone())
            .unwrap_or_else(|| IndexSet::empty(function.value_types.len()));
        definitions.extend(block_definitions[block.0].iter().copied());
        available[block.0] = definitions;
    }

    Ok(FunctionFlow {
        reachable,
        reverse_postorder,
        predecessors,
        dominators,
        available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build CFG-only fixtures: these tests exercise flow preparation, not the
    // type rules for the synthetic branch conditions.
    fn graph(edges: &[Vec<usize>], entry: usize) -> MirFunction {
        let program = crate::syntax::parse_program("fn main() {}").unwrap();
        let types = crate::sema::check_program(&program).unwrap();
        let core = crate::hir::CoreProgram::lower(program, types).unwrap();
        let mir = MirProgram::lower(&core).unwrap();
        let mut function = mir
            .functions
            .into_iter()
            .find(|f| f.name == "main")
            .unwrap();
        function.entry = MirBlockId(entry);
        function.value_types = vec![Type::Unit; edges.len() * 2];
        function.value_ownership = vec![MirOwnership::Copy; edges.len() * 2];
        function.blocks = edges
            .iter()
            .enumerate()
            .map(|(id, successors)| MirBlock {
                id: MirBlockId(id),
                scoped: false,
                scope_depth: 0,
                statements: vec![
                    MirStatement::Unit {
                        destination: MirValueId(id * 2),
                    },
                    MirStatement::Unit {
                        destination: MirValueId(id * 2 + 1),
                    },
                ],
                terminator: Some(match successors.as_slice() {
                    [] => MirTerminator::Return(None),
                    [next] => MirTerminator::Goto {
                        target: MirBlockId(*next),
                        arguments: Vec::new(),
                    },
                    [a, b] => MirTerminator::Branch {
                        condition: MirValueId(0),
                        then_block: MirBlockId(*a),
                        else_block: MirBlockId(*b),
                    },
                    _ => unreachable!(),
                }),
            })
            .collect();
        function
    }

    fn reachable_without(
        edges: &[Vec<usize>],
        entry: usize,
        removed: Option<usize>,
    ) -> HashSet<usize> {
        let mut reached = HashSet::new();
        let mut stack = vec![entry];
        while let Some(block) = stack.pop() {
            if Some(block) != removed && reached.insert(block) {
                stack.extend(&edges[block]);
            }
        }
        reached
    }

    fn check_graph(edges: &[Vec<usize>], entry: usize) {
        let function = graph(edges, entry);
        let flow = prepare_flow(&function).unwrap();
        let reachable = reachable_without(edges, entry, None);
        let mut expected = vec![HashSet::new(); edges.len()];
        // An independent dominator oracle: deleting a dominator disconnects
        // the dominated block from entry. This also covers irreducible loops.
        for candidate in 0..edges.len() {
            let without = reachable_without(edges, entry, Some(candidate));
            for &block in &reachable {
                if !without.contains(&block) {
                    expected[block].insert(MirBlockId(candidate));
                }
            }
        }
        for (block, expected_dominators) in expected.iter().enumerate() {
            if !reachable.contains(&block) {
                assert_eq!(flow.available[block].iter().count(), 0);
                continue;
            }
            assert_eq!(
                flow.dominators[block].iter().collect::<HashSet<_>>(),
                expected_dominators.clone()
            );
            let definitions = expected_dominators
                .iter()
                .flat_map(|id| [MirValueId(id.0 * 2), MirValueId(id.0 * 2 + 1)])
                .collect::<HashSet<_>>();
            assert_eq!(
                flow.available[block].iter().collect::<HashSet<_>>(),
                definitions
            );
        }
    }

    #[test]
    fn dense_flow_matches_path_removal_oracle() {
        check_graph(&[vec![]], 0);
        // Diamond, loop back edge, unreachable predecessor, nonzero entry.
        check_graph(
            &[vec![1, 2], vec![3], vec![3], vec![0, 4], vec![], vec![3]],
            0,
        );
        check_graph(&[vec![3], vec![], vec![0, 3], vec![1]], 2);
        // Multiple loop entries, where numbering is not a traversal order.
        check_graph(&[vec![2, 3], vec![2, 4], vec![1], vec![1, 2], vec![]], 0);
        for size in [65, 130] {
            let mut chain = (0..size)
                .map(|id| if id + 1 < size { vec![id + 1] } else { vec![] })
                .collect::<Vec<_>>();
            check_graph(&chain, 0);
            chain[size - 1] = vec![size / 2];
            check_graph(&chain, 0);
        }
        let mut seed = 17u64;
        for _ in 0..32 {
            let mut edges = vec![Vec::new(); 9];
            for successors in &mut edges {
                for _ in 0..2 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let next = ((seed >> 32) % 11) as usize;
                    if next < 9 {
                        successors.push(next);
                    }
                }
            }
            check_graph(&edges, 0);
        }
    }
}
