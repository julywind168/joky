use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn insert_scope_edge_drops(
    blocks: &mut Vec<MirBlock>,
    locals: &[MirLocal],
    parameters: &[MirParameter],
    receiver_local: Option<MirLocalId>,
    value_types: &mut Vec<Type>,
    value_ownership: &mut Vec<MirOwnership>,
) {
    let mut predecessors = vec![Vec::<usize>::new(); blocks.len()];
    let mut successors = vec![Vec::<usize>::new(); blocks.len()];
    for (index, block) in blocks.iter().enumerate() {
        if let Some(terminator) = block.terminator.as_ref() {
            let targets = match terminator {
                MirTerminator::Goto { target, .. } => vec![target.0],
                MirTerminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![then_block.0, else_block.0],
                MirTerminator::Return(_) | MirTerminator::Unreachable => Vec::new(),
            };
            for target in targets {
                predecessors[target].push(index);
                successors[index].push(target);
            }
        }
    }

    let mut reachable = HashSet::from([0]);
    let mut pending = vec![0];
    while let Some(index) = pending.pop() {
        for successor in &successors[index] {
            if reachable.insert(*successor) {
                pending.push(*successor);
            }
        }
    }

    let owned_locals = locals
        .iter()
        .filter(|local| matches!(local.ownership, MirOwnership::Owned | MirOwnership::Shared))
        .map(|local| local.id)
        .collect::<HashSet<_>>();
    let mut entry_state = parameters
        .iter()
        .filter(|parameter| {
            matches!(
                parameter.ownership,
                MirOwnership::Owned | MirOwnership::Shared
            )
        })
        .map(|parameter| parameter.local)
        .collect::<HashSet<_>>();
    if let Some(local) = receiver_local {
        if matches!(
            locals[local.0].ownership,
            MirOwnership::Owned | MirOwnership::Shared
        ) {
            entry_state.insert(local);
        }
    }
    let mut incoming = vec![owned_locals.clone(); blocks.len()];
    let mut outgoing = vec![owned_locals.clone(); blocks.len()];
    incoming[0] = entry_state;
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..blocks.len() {
            if !reachable.contains(&index) {
                continue;
            }
            if index != 0 {
                let mut next = owned_locals.clone();
                for predecessor in &predecessors[index] {
                    let mut edge_state = outgoing[*predecessor].clone();
                    edge_state
                        .retain(|local| locals[local.0].scope_depth <= blocks[index].scope_depth);
                    next.retain(|local| edge_state.contains(local));
                }
                if next != incoming[index] {
                    incoming[index] = next;
                    changed = true;
                }
            }
            let mut next = incoming[index].clone();
            for statement in &blocks[index].statements {
                match statement {
                    MirStatement::Bind { local, value, .. }
                        if value.is_some() && owned_locals.contains(local) =>
                    {
                        next.insert(*local);
                    }
                    MirStatement::TakeLocal { local, .. }
                    | MirStatement::DropLocal { local, .. } => {
                        next.remove(local);
                    }
                    _ => {}
                }
            }
            if next != outgoing[index] {
                outgoing[index] = next;
                changed = true;
            }
        }
    }

    let original_block_count = blocks.len();
    for index in 0..original_block_count {
        if !reachable.contains(&index) {
            continue;
        }
        let Some(terminator) = blocks[index].terminator.as_ref() else {
            continue;
        };
        match terminator {
            MirTerminator::Return(_) => {
                let drops = sorted_owned_locals(&outgoing[index]);
                append_local_drops(
                    &mut blocks[index].statements,
                    &drops,
                    value_types,
                    value_ownership,
                );
            }
            MirTerminator::Goto { target, .. } => {
                let target_depth = blocks[target.0].scope_depth;
                let drops = edge_drop_locals(&outgoing[index], target_depth, locals);
                append_local_drops(
                    &mut blocks[index].statements,
                    &drops,
                    value_types,
                    value_ownership,
                );
            }
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                let then_target = *then_block;
                let else_target = *else_block;
                let then_drops =
                    edge_drop_locals(&outgoing[index], blocks[then_target.0].scope_depth, locals);
                let else_drops =
                    edge_drop_locals(&outgoing[index], blocks[else_target.0].scope_depth, locals);
                if then_drops == else_drops {
                    append_local_drops(
                        &mut blocks[index].statements,
                        &then_drops,
                        value_types,
                        value_ownership,
                    );
                } else {
                    let then_cleanup = make_cleanup_block(
                        blocks,
                        then_target,
                        &then_drops,
                        value_types,
                        value_ownership,
                    );
                    let else_cleanup = make_cleanup_block(
                        blocks,
                        else_target,
                        &else_drops,
                        value_types,
                        value_ownership,
                    );
                    let Some(MirTerminator::Branch {
                        then_block,
                        else_block,
                        ..
                    }) = blocks[index].terminator.as_mut()
                    else {
                        unreachable!("branch terminator was inspected above");
                    };
                    *then_block = then_cleanup.unwrap_or(then_target);
                    *else_block = else_cleanup.unwrap_or(else_target);
                }
            }
            MirTerminator::Unreachable => {}
        }
    }
}

fn sorted_owned_locals(locals: &HashSet<MirLocalId>) -> Vec<MirLocalId> {
    let mut locals = locals.iter().copied().collect::<Vec<_>>();
    locals.sort_by_key(|local| std::cmp::Reverse(local.0));
    locals
}

fn edge_drop_locals(
    initialized: &HashSet<MirLocalId>,
    target_depth: usize,
    locals: &[MirLocal],
) -> Vec<MirLocalId> {
    sorted_owned_locals(
        &initialized
            .iter()
            .filter(|local| locals[local.0].scope_depth > target_depth)
            .copied()
            .collect(),
    )
}

fn append_local_drops(
    statements: &mut Vec<MirStatement>,
    locals: &[MirLocalId],
    value_types: &mut Vec<Type>,
    value_ownership: &mut Vec<MirOwnership>,
) {
    for local in locals {
        let destination = MirValueId(value_types.len());
        value_types.push(Type::Unit);
        value_ownership.push(MirOwnership::Copy);
        statements.push(MirStatement::DropLocal {
            destination,
            local: *local,
        });
    }
}

fn make_cleanup_block(
    blocks: &mut Vec<MirBlock>,
    target: MirBlockId,
    drops: &[MirLocalId],
    value_types: &mut Vec<Type>,
    value_ownership: &mut Vec<MirOwnership>,
) -> Option<MirBlockId> {
    if drops.is_empty() {
        return None;
    }
    let id = MirBlockId(blocks.len());
    let mut statements = Vec::new();
    append_local_drops(&mut statements, drops, value_types, value_ownership);
    blocks.push(MirBlock {
        id,
        scoped: false,
        scope_depth: blocks[target.0].scope_depth,
        statements,
        terminator: Some(MirTerminator::Goto {
            target,
            arguments: Vec::new(),
        }),
    });
    Some(id)
}
