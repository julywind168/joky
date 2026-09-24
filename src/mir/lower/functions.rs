//! Function-level HIR to MIR lowering setup.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_function(
    function: &CoreFunction,
    id: MirFunctionId,
    function_ids: &HashMap<String, MirFunctionId>,
    function_names: &HashSet<String>,
    function_parameters: &HashMap<MirFunctionId, Vec<String>>,
    function_bodies: &HashMap<String, &CoreFunction>,
    closure_ids: &HashMap<String, MirFunctionId>,
    types: &CheckedTypes,
    struct_defaults: &[Vec<Option<CoreExpr>>],
    class_defaults: &[Vec<Option<CoreExpr>>],
    handler_function_ids: &HashMap<crate::syntax::NodeId, MirFunctionId>,
    source_spans: &HashMap<crate::syntax::NodeId, crate::Span>,
) -> Result<MirFunction, Diagnostic> {
    let entry = MirBlockId(0);
    let mut lowerer = Lowerer {
        source_spans,
        current_span: source_spans.get(&function.body.id).copied(),
        value_spans: Vec::new(),
        mutable_locals: HashMap::new(),
        source_locals: HashMap::new(),
        assignments: HashSet::new(),
        capture_state: None,
        blocks: vec![MirBlock {
            id: entry,
            scoped: false,
            scope_depth: 0,
            statements: Vec::new(),
            terminator: None,
        }],
        block_cown_depths: vec![0],
        block_task_depths: vec![1],
        cown_leases: Vec::new(),
        current: entry,
        scope_depth: 0,
        next_value: 0,
        next_scope: 1,
        next_task: 0,
        next_continuation: 0,
        value_types: Vec::new(),
        value_ownership: Vec::new(),
        continuations: Vec::new(),
        loops: Vec::new(),
        handlers: Vec::new(),
        resumable_handlers: Vec::new(),
        inline_functions: Vec::new(),
        struct_defaults,
        class_defaults,
        function_ids,
        function_parameters,
        function_bodies,
        closure_ids,
        types,
        locals: Vec::new(),
        entry_locals: HashSet::new(),
        bindings: vec![HashMap::new()],
        closure_bindings: HashMap::new(),
        receiver_local: None,
        task_scopes: vec![MirScopeId(0)],
        branch_regions: HashSet::new(),
        function_name: function.name.clone(),
        is_task: function.name.starts_with("__task_"),
        return_type: function.return_type,
        handler_function_ids,
    };
    let receiver_local = function.receiver.map(|ty| {
        let ownership = if types.is_owned(ty)
            && function.receiver_mode == crate::syntax::ReceiverMode::Borrowed
        {
            MirOwnership::Borrowed
        } else {
            ownership_for_type(ty, types)
        };
        lowerer.new_local_with_ownership("self", ty, ownership)
    });
    lowerer.receiver_local = receiver_local;
    if let Some(local) = receiver_local {
        lowerer.bind_local("self", local);
        lowerer.entry_locals.insert(local);
    }
    let parameters = function
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let ownership = function
                .parameter_ownership
                .get(index)
                .and_then(|mode| *mode)
                .map(|mode| match mode {
                    crate::hir::CoreParameterOwnership::Borrowed => MirOwnership::Borrowed,
                    crate::hir::CoreParameterOwnership::Owned => MirOwnership::Owned,
                })
                .unwrap_or_else(|| {
                    if index < function.borrowed_parameters {
                        MirOwnership::Borrowed
                    } else if function.foreign.is_some()
                        && matches!(parameter.ty, Type::Function(_))
                    {
                        // A callback parameter is a single trampoline-address
                        // word; the closure environment is released at the
                        // call site and never crosses into C.
                        MirOwnership::Copy
                    } else {
                        ownership_for_type(parameter.ty, types)
                    }
                });
            let local = lowerer.new_local_with_ownership(&parameter.name, parameter.ty, ownership);
            lowerer.bind_local(&parameter.name, local);
            lowerer.entry_locals.insert(local);
            MirParameter {
                local,
                name: parameter.name.clone(),
                ty: parameter.ty,
                ownership,
            }
        })
        .collect::<Vec<_>>();
    if let Some(state) = &function.closure_state {
        lowerer.capture_state = Some((parameters[0].local, state.clone()));
    }
    // Closure environments lend their captures to each call. Give copy/shared
    // captures a local lifetime so resumptions never spill an environment
    // borrow. Owned captures retain the existing borrowing restrictions.
    for parameter in parameters.iter().take(function.borrowed_parameters) {
        if types.is_owned(parameter.ty) {
            continue;
        }
        let read = lowerer.next_value(parameter.ty);
        lowerer.push_statement(MirStatement::Read {
            destination: read,
            local: parameter.local,
        });
        let value = if types.is_shared(parameter.ty) {
            let copy = lowerer.next_value(parameter.ty);
            lowerer.push_statement(MirStatement::Dup {
                destination: copy,
                value: read,
            });
            copy
        } else {
            read
        };
        let local = lowerer.new_local(&parameter.name, parameter.ty);
        lowerer.bind_local(&parameter.name, local);
        let destination = lowerer.next_value(Type::Unit);
        lowerer.push_statement(MirStatement::Bind {
            local,
            value: Some(value),
            destination,
        });
    }
    let result = if function.foreign.is_some() {
        // Declaration-only MIR. Native codegen supplies the C ABI adapter.
        lowerer.terminate(MirTerminator::Unreachable)?;
        None
    } else {
        lowerer.lower_value(&function.body, function_names, types)?
    };
    if lowerer.is_open() {
        lowerer.terminate(MirTerminator::Return(result))?;
    }
    // Phase 2: determine if this function may suspend
    let is_suspending = function.name.starts_with("@dyn/")
        || lowerer
            .continuations
            .iter()
            .any(|item| item.kind.is_suspending());

    let mut lowered = MirFunction {
        source: MirFunctionSource {
            module: None,
            body: source_spans.get(&function.body.id).copied(),
            values: lowerer.value_spans,
            mutable_locals: lowerer.mutable_locals,
            mutable_capture: function
                .closure_state
                .as_ref()
                .and_then(|_| source_spans.get(&function.body.id).copied()),
        },
        declared_effects: function.declared_effects.clone(),
        foreign: function.foreign.clone(),
        id,
        module: MirModuleId(function.module.0),
        name: function.name.clone(),
        external_symbol: None,
        imported_region_contract: None,
        visibility: function.visibility,
        receiver: function.receiver,
        parameters,
        receiver_local,
        locals: lowerer.locals,
        return_type: function.return_type,
        is_task: lowerer.is_task,
        is_suspending,
        entry,
        blocks: lowerer.blocks,
        value_types: lowerer.value_types,
        value_ownership: lowerer.value_ownership,
        continuations: lowerer.continuations,
    };
    crate::mir::local_ssa::prepare_assignments(&mut lowered, &lowerer.assignments)?;
    insert_scope_edge_drops(
        &mut lowered.blocks,
        &lowered.locals,
        &lowered.parameters,
        lowered.receiver_local,
        &mut lowered.value_types,
        &mut lowered.value_ownership,
    );
    crate::mir::local_ssa::normalize(&mut lowered);
    finalize_function_continuation_spills(&mut lowered);
    Ok(lowered)
}

pub(crate) fn finalize_function_continuation_spills(function: &mut MirFunction) {
    let entry_locals = function
        .parameters
        .iter()
        .map(|parameter| parameter.local)
        .chain(function.receiver_local)
        .collect();
    finalize_continuation_spills(
        &function.blocks,
        &function.locals,
        &function.value_ownership,
        &mut function.continuations,
        function.entry,
        &entry_locals,
    );
}

fn finalize_continuation_spills(
    blocks: &[MirBlock],
    locals: &[MirLocal],
    value_ownership: &[MirOwnership],
    continuations: &mut [MirContinuation],
    entry: MirBlockId,
    entry_locals: &HashSet<MirLocalId>,
) {
    if continuations.is_empty() {
        return;
    }
    let dominators = dominators_for(blocks, entry);
    let live_in = live_values_at_block_entries(blocks);
    let live_locals = live_locals_at_block_entries(blocks);
    let local_values = local_values_at_block_entries(blocks, entry, entry_locals);
    let definitions = blocks
        .iter()
        .flat_map(|block| {
            block
                .statements
                .iter()
                .enumerate()
                .filter_map(|(index, statement)| {
                    statement_destination(statement).map(|value| (value, (block.id, index)))
                })
        })
        .collect::<HashMap<_, _>>();
    for metadata in continuations {
        let continuation = metadata.clone();
        let Some(suspend_block) = blocks.get(continuation.suspend_block.0) else {
            continue;
        };
        let Some(suspend_index) =
            suspend_block
                .statements
                .iter()
                .enumerate()
                .find_map(|(index, statement)| match statement {
                    MirStatement::Suspend {
                        continuation: id, ..
                    }
                    | MirStatement::TaskWait {
                        continuation: id, ..
                    }
                    | MirStatement::CownAcquire {
                        continuation: id, ..
                    }
                    | MirStatement::Call {
                        continuation: Some(id),
                        ..
                    }
                    | MirStatement::MethodCall {
                        continuation: Some(id),
                        ..
                    }
                    | MirStatement::CallIndirect {
                        continuation: Some(id),
                        ..
                    } if *id == continuation.id => Some(index),
                    _ => None,
                })
        else {
            continue;
        };
        let mut initialized = local_values[continuation.suspend_block.0].clone();
        for statement in suspend_block.statements.iter().take(suspend_index) {
            update_local_values(&mut initialized, statement);
        }
        let used = live_in[continuation.resume_block.0].clone();
        let used_locals = live_locals[continuation.resume_block.0].clone();
        let mut spills = used
            .into_iter()
            .filter(|value| {
                definitions.get(value).is_some_and(|(defined, index)| {
                    dominators[continuation.suspend_block.0].contains(defined)
                        && (*defined != continuation.suspend_block || *index < suspend_index)
                })
            })
            .collect::<Vec<_>>();
        spills.sort_by_key(|value| value.0);
        metadata.spill_slots = spills
            .iter()
            .enumerate()
            .map(|(slot, value)| MirSpillSlot {
                value: *value,
                slot,
                ownership: value_ownership[value.0],
            })
            .collect();
        metadata.spill_values = spills;
        let mut frame_locals = used_locals
            .into_iter()
            .filter(|local| local.0 < continuation.locals_before_suspend)
            .filter_map(|local| {
                // A handler arm allocates its parameter locals before
                // lowering the body, yet the arm Phi initializes them
                // only after the suspension. They have no value to
                // save here; the handler edge supplies that value on
                // resume. Entry locals are the sole unbound locals
                // whose values originate outside MIR statements.
                initialized
                    .get(&local)
                    .and_then(|value| locals.get(local.0).map(|item| (item, *value)))
            })
            .map(|(local, value)| (local.id, local.ty, local.ownership, value))
            .collect::<Vec<_>>();
        frame_locals.sort_by_key(|(local, _, _, _)| local.0);
        metadata.frame_slots = frame_locals
            .into_iter()
            .enumerate()
            .map(|(slot, (local, ty, ownership, value))| MirFrameSlot {
                local,
                slot,
                value,
                ty,
                ownership,
            })
            .collect();
    }
}

// A later Bind starts a new lifetime, including a lease in the next loop iteration.
fn live_locals_at_block_entries(blocks: &[MirBlock]) -> Vec<HashSet<MirLocalId>> {
    let mut incoming = vec![HashSet::new(); blocks.len()];
    loop {
        let mut changed = false;
        for block in blocks.iter().rev() {
            let mut live = HashSet::new();
            for target in terminator_targets(block.terminator.as_ref()) {
                live.extend(incoming[target.0].iter().copied());
            }
            for statement in block.statements.iter().rev() {
                match statement {
                    MirStatement::Bind { local, .. } => {
                        live.remove(local);
                    }
                    MirStatement::Read { local, .. }
                    | MirStatement::BorrowLocal { local, .. }
                    | MirStatement::TakeLocal { local, .. }
                    | MirStatement::DropLocal { local, .. } => {
                        live.insert(*local);
                    }
                    _ => {}
                }
            }
            if live != incoming[block.id.0] {
                incoming[block.id.0] = live;
                changed = true;
            }
        }
        if !changed {
            return incoming;
        }
    }
}

/// SSA normalization supplies a `Bind` after every local-value merge. Track
/// those reaching bindings through the CFG so moves on non-dominating paths
/// cannot resurrect an old binding (including an implicit entry parameter).
fn local_values_at_block_entries(
    blocks: &[MirBlock],
    entry: MirBlockId,
    entry_locals: &HashSet<MirLocalId>,
) -> Vec<HashMap<MirLocalId, Option<MirValueId>>> {
    let reachable = reachable_blocks_from(blocks, entry);
    let mut predecessors = vec![Vec::new(); blocks.len()];
    for block in blocks.iter().filter(|block| reachable.contains(&block.id)) {
        for target in terminator_targets(block.terminator.as_ref()) {
            predecessors[target.0].push(block.id);
        }
    }
    let entry_values: HashMap<_, _> = entry_locals.iter().map(|local| (*local, None)).collect();
    let mut incoming = vec![HashMap::new(); blocks.len()];
    let mut outgoing: Vec<Option<HashMap<MirLocalId, Option<MirValueId>>>> =
        vec![None; blocks.len()];
    loop {
        let mut changed = false;
        for block in blocks.iter().filter(|block| reachable.contains(&block.id)) {
            let values = if block.id == entry {
                entry_values.clone()
            } else {
                let mut states = predecessors[block.id.0]
                    .iter()
                    .filter_map(|predecessor| outgoing[predecessor.0].as_ref());
                let Some(first) = states.next() else {
                    continue;
                };
                let mut common: HashMap<_, _> = first.clone();
                for state in states {
                    common.retain(|local, value| state.get(local) == Some(value));
                }
                common
            };
            incoming[block.id.0] = values.clone();
            let mut values = values;
            for statement in &block.statements {
                update_local_values(&mut values, statement);
            }
            if outgoing[block.id.0].as_ref() != Some(&values) {
                outgoing[block.id.0] = Some(values);
                changed = true;
            }
        }
        if !changed {
            return incoming;
        }
    }
}

fn update_local_values(
    values: &mut HashMap<MirLocalId, Option<MirValueId>>,
    statement: &MirStatement,
) {
    match statement {
        MirStatement::Bind { local, value, .. } => {
            values.insert(*local, *value);
        }
        MirStatement::TakeLocal { local, .. } | MirStatement::DropLocal { local, .. } => {
            values.remove(local);
        }
        _ => {}
    }
}

fn reachable_blocks_from(blocks: &[MirBlock], entry: MirBlockId) -> HashSet<MirBlockId> {
    let mut reachable = HashSet::new();
    let mut pending = vec![entry];
    while let Some(block_id) = pending.pop() {
        if block_id.0 >= blocks.len() || !reachable.insert(block_id) {
            continue;
        }
        let Some(terminator) = &blocks[block_id.0].terminator else {
            continue;
        };
        match terminator {
            MirTerminator::Goto { target, .. } => pending.push(*target),
            MirTerminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                pending.push(*then_block);
                pending.push(*else_block);
            }
            MirTerminator::Return(_) | MirTerminator::Unreachable => {}
        }
    }
    reachable
}

/// Standard backwards liveness kills definitions on loop back-edges. Merely
/// collecting all reachable operands spills already-consumed List cursors and
/// other temporaries from a previous iteration, then cleans dangling pointers
/// when a later iteration is cancelled.
fn live_values_at_block_entries(blocks: &[MirBlock]) -> Vec<HashSet<MirValueId>> {
    let mut live_in = vec![HashSet::new(); blocks.len()];
    loop {
        let mut changed = false;
        for block in blocks.iter().rev() {
            let mut live = HashSet::new();
            for successor in terminator_targets(block.terminator.as_ref()) {
                live.extend(live_in[successor.0].iter().copied());
                for statement in &blocks[successor.0].statements {
                    if let MirStatement::Phi { incoming, .. } = statement {
                        live.extend(
                            incoming
                                .iter()
                                .filter(|(predecessor, _)| *predecessor == block.id)
                                .map(|(_, value)| *value),
                        );
                    }
                }
            }
            match &block.terminator {
                Some(MirTerminator::Goto { arguments, .. }) => live.extend(arguments),
                Some(MirTerminator::Branch { condition, .. }) => {
                    live.insert(*condition);
                }
                Some(MirTerminator::Return(Some(value))) => {
                    live.insert(*value);
                }
                _ => {}
            }
            for statement in block.statements.iter().rev() {
                if let Some(destination) = statement_destination(statement) {
                    live.remove(&destination);
                }
                if !matches!(statement, MirStatement::Phi { .. }) {
                    live.extend(crate::mir::verifier::statement_operands(statement));
                }
            }
            if live != live_in[block.id.0] {
                live_in[block.id.0] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}

pub(crate) fn statement_destination(statement: &MirStatement) -> Option<MirValueId> {
    match statement {
        MirStatement::Unit { destination }
        | MirStatement::Const { destination, .. }
        | MirStatement::Read { destination, .. }
        | MirStatement::BorrowLocal { destination, .. }
        | MirStatement::TakeLocal { destination, .. }
        | MirStatement::Unary { destination, .. }
        | MirStatement::Binary { destination, .. }
        | MirStatement::Call { destination, .. }
        | MirStatement::FunctionValue { destination, .. }
        | MirStatement::DynamicValue { destination, .. }
        | MirStatement::DynamicUpcast { destination, .. }
        | MirStatement::CallIndirect { destination, .. }
        | MirStatement::Project { destination, .. }
        | MirStatement::EnumTag { destination, .. }
        | MirStatement::EnumProject { destination, .. }
        | MirStatement::Tuple { destination, .. }
        | MirStatement::EnumConstruct { destination, .. }
        | MirStatement::Construct { destination, .. }
        | MirStatement::Store { destination, .. }
        | MirStatement::MethodCall { destination, .. }
        | MirStatement::RuntimeCall { destination, .. }
        | MirStatement::HandlerRequest { destination, .. }
        | MirStatement::ResumableRequest { destination, .. }
        | MirStatement::Suspend { destination, .. }
        | MirStatement::TaskWait { destination, .. }
        | MirStatement::CownAcquire { destination, .. }
        | MirStatement::TaskPoll { destination }
        | MirStatement::TaskCancelled { destination }
        | MirStatement::TaskAbort { destination, .. }
        | MirStatement::TaskFailureOperation { destination, .. }
        | MirStatement::TaskFailurePayload { destination, .. }
        | MirStatement::TaskFailureRethrow { destination, .. }
        | MirStatement::TaskJoin { destination, .. }
        | MirStatement::RaceSelect { destination, .. }
        | MirStatement::Dup { destination, .. }
        | MirStatement::Move { destination, .. }
        | MirStatement::Drop { destination, .. }
        | MirStatement::Deinit { destination, .. }
        | MirStatement::DropLocal { destination, .. }
        | MirStatement::Bind { destination, .. }
        | MirStatement::Phi { destination, .. } => Some(*destination),
        _ => None,
    }
}

fn dominators_for(blocks: &[MirBlock], entry: MirBlockId) -> Vec<HashSet<MirBlockId>> {
    let reachable = reachable_blocks_from(blocks, entry);
    let mut dominators = vec![reachable.clone(); blocks.len()];
    if entry.0 >= blocks.len() {
        return dominators;
    }
    dominators[entry.0] = HashSet::from([entry]);
    let mut changed = true;
    while changed {
        changed = false;
        for block in blocks {
            if block.id == entry || !reachable.contains(&block.id) {
                continue;
            }
            let predecessors = blocks
                .iter()
                .filter(|candidate| {
                    reachable.contains(&candidate.id)
                        && terminator_targets(candidate.terminator.as_ref()).contains(&block.id)
                })
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>();
            if predecessors.is_empty() {
                continue;
            }
            let mut next = dominators[predecessors[0].0].clone();
            for predecessor in &predecessors[1..] {
                next = next
                    .intersection(&dominators[predecessor.0])
                    .copied()
                    .collect();
            }
            next.insert(block.id);
            if next != dominators[block.id.0] {
                dominators[block.id.0] = next;
                changed = true;
            }
        }
    }
    dominators
}

#[cfg(test)]
#[path = "functions_tests.rs"]
mod tests;

fn terminator_targets(terminator: Option<&MirTerminator>) -> Vec<MirBlockId> {
    match terminator {
        Some(MirTerminator::Goto { target, .. }) => vec![*target],
        Some(MirTerminator::Branch {
            then_block,
            else_block,
            ..
        }) => vec![*then_block, *else_block],
        Some(MirTerminator::Return(_) | MirTerminator::Unreachable) | None => Vec::new(),
    }
}
