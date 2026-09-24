use super::*;

pub(super) fn verify_task_structure(
    function: &MirFunction,
    reachable: &HashSet<MirBlockId>,
    signatures: &HashMap<MirFunctionId, MirSignature>,
) -> Result<(), Diagnostic> {
    let mut scopes = HashSet::from([MirScopeId(0)]);
    let mut exited_scopes = HashSet::new();
    let mut tasks = HashMap::<MirTaskId, (MirScopeId, MirFunctionId, Vec<MirCallArgument>)>::new();
    let mut joined = HashSet::new();
    let mut claimed = HashSet::new();
    let mut cancelled = HashSet::new();
    // RaceSelect performs both winner result transfer and loser cleanup as one
    // runtime operation. Keep race membership separate from the ordinary task
    // protocol so a race task cannot be consumed by join/claim/cancel as well.
    let mut race_membership = HashMap::<MirTaskId, MirScopeId>::new();
    let mut races_started = HashSet::<(MirScopeId, Vec<MirTaskId>)>::new();
    let mut races_selected = HashSet::<(MirScopeId, Vec<MirTaskId>)>::new();

    // Collect race ownership before validating consumers. MIR block order is
    // not a semantic ordering guarantee, so a malformed join placed before a
    // RaceStart must still be rejected as a race-task consumption.
    for block in function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
    {
        for statement in &block.statements {
            if let MirStatement::RaceStart { scope, tasks: race } = statement {
                for task in race {
                    if let Some(previous_scope) = race_membership.insert(*task, *scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' adds task t{} to more than one race (s{} and s{})",
                            function.name, task.0, previous_scope.0, scope.0
                        )));
                    }
                }
            }
        }
    }

    for block in function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
    {
        for statement in &block.statements {
            match statement {
                MirStatement::ScopeEnter { scope, .. } => {
                    if !scopes.insert(*scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' enters task scope s{} more than once",
                            function.name, scope.0
                        )));
                    }
                }
                MirStatement::ScopeExit { scope } => {
                    if !scopes.contains(scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' exits unknown task scope s{}",
                            function.name, scope.0
                        )));
                    }
                    // A single lexical scope can have several CFG cleanup
                    // exits (for example, normal and failure edges). Track
                    // the fact that it has an exit, but do not reject those
                    // structurally distinct cleanup paths.
                    exited_scopes.insert(*scope);
                }
                MirStatement::TaskCreate {
                    scope,
                    task,
                    function: task_function,
                    arguments,
                } => {
                    if !scopes.contains(scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' creates task t{} outside task scope s{}",
                            function.name, task.0, scope.0
                        )));
                    }
                    let signature = signatures.get(task_function).ok_or_else(|| {
                        Diagnostic::codegen(format!(
                            "MIR function '{}' creates task t{} with unknown function id {}",
                            function.name, task.0, task_function.0
                        ))
                    })?;
                    verify_task_arguments(function, *task, arguments, signature)?;
                    if let Some(argument) = arguments.iter().find(|argument| {
                        function
                            .value_ownership
                            .get(argument.value.0)
                            .is_some_and(|ownership| *ownership == MirOwnership::Borrowed)
                    }) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' captures borrowed value %{} into task t{}",
                            function.name, argument.value.0, task.0
                        )));
                    }
                    if tasks
                        .insert(*task, (*scope, *task_function, arguments.clone()))
                        .is_some()
                    {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' creates task t{} more than once",
                            function.name, task.0
                        )));
                    }
                }
                MirStatement::TaskJoin {
                    destination,
                    scope,
                    task,
                } => {
                    let Some((task_scope, task_function, _)) = tasks.get(task) else {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' joins unknown task t{}",
                            function.name, task.0
                        )));
                    };
                    if task_scope != scope {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' joins task t{} from the wrong scope",
                            function.name, task.0
                        )));
                    }
                    if race_membership.contains_key(task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' joins race task t{}; RaceSelect owns its cleanup",
                            function.name, task.0
                        )));
                    }
                    let result = signatures[task_function].return_type;
                    check_destination_type(function, *destination, result)?;
                    if !joined.insert(*task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' joins task t{} more than once",
                            function.name, task.0
                        )));
                    }
                }
                MirStatement::TaskCancel { scope, task } => {
                    let Some((task_scope, _, _)) = tasks.get(task) else {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' cancels unknown task t{}",
                            function.name, task.0
                        )));
                    };
                    if task_scope != scope {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' cancels task t{} from the wrong scope",
                            function.name, task.0
                        )));
                    }
                    if race_membership.contains_key(task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' cancels race task t{}; RaceSelect owns its cleanup",
                            function.name, task.0
                        )));
                    }
                    if !cancelled.insert(*task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' cancels task t{} more than once",
                            function.name, task.0
                        )));
                    }
                }
                MirStatement::TaskClaimResult { scope, task } => {
                    let Some((task_scope, _, _)) = tasks.get(task) else {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' claims unknown task t{}",
                            function.name, task.0
                        )));
                    };
                    if task_scope != scope {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' claims task t{} from the wrong scope",
                            function.name, task.0
                        )));
                    }
                    if race_membership.contains_key(task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' claims race task t{}; RaceSelect owns its result",
                            function.name, task.0
                        )));
                    }
                    if !claimed.insert(*task) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' claims task t{} more than once",
                            function.name, task.0
                        )));
                    }
                }
                MirStatement::RaceStart { scope, tasks: race } => {
                    let result = verify_race_tasks(function, *scope, race, &tasks, signatures)?;
                    let _ = result;
                    if !races_started.insert((*scope, race.clone())) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' starts the same race more than once",
                            function.name
                        )));
                    }
                }
                MirStatement::RaceSelect {
                    destination,
                    scope,
                    tasks: race,
                } => {
                    let result = verify_race_tasks(function, *scope, race, &tasks, signatures)?;
                    check_destination_type(function, *destination, result)?;
                    if !races_selected.insert((*scope, race.clone())) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' selects the same race more than once",
                            function.name
                        )));
                    }
                }
                MirStatement::TaskWait { scope, .. }
                | MirStatement::TaskFailureOperation { scope, .. }
                | MirStatement::TaskFailurePayload { scope, .. }
                | MirStatement::TaskFailureClaim { scope }
                | MirStatement::TaskFailureRethrow { scope, .. }
                    if !scopes.contains(scope) =>
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' reads failure from unknown task scope s{}",
                        function.name, scope.0
                    )));
                }
                _ => {}
            }
        }
    }

    for scope in scopes {
        if !exited_scopes.contains(&scope) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' does not exit task scope s{}",
                function.name, scope.0
            )));
        }
    }
    for race in &races_started {
        if !races_selected.contains(race) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' starts a race without selecting its result",
                function.name
            )));
        }
    }
    for race in &races_selected {
        if !races_started.contains(race) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' selects a race that was not started",
                function.name
            )));
        }
    }
    // A joined normal task owns a result slot that must be explicitly claimed
    // on the normal completion path. Failure paths are exempt: the enclosing
    // TaskFailureClaim transfers the scope payload and runtime cleans up the
    // remaining task slots during scope exit.
    for task in &joined {
        if !claimed.contains(task) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' joins task t{} without claiming its result",
                function.name, task.0
            )));
        }
    }
    for task in &claimed {
        if !joined.contains(task) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' claims task t{} without joining it",
                function.name, task.0
            )));
        }
    }
    for task in &cancelled {
        if joined.contains(task) || claimed.contains(task) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' cancels task t{} after it was joined or claimed",
                function.name, task.0
            )));
        }
    }
    Ok(())
}

/// Check task operations against the lexical task-scope state along every CFG
/// path. The older structural pass validates IDs globally; this pass catches
/// an operation moved past `ScopeExit` on one path, which would otherwise let
/// a task handle be used after its owner scope has been torn down.
pub(super) fn verify_task_scope_lifetimes(
    function: &MirFunction,
    reachable: &HashSet<MirBlockId>,
) -> Result<(), Diagnostic> {
    let mut incoming = vec![None; function.blocks.len()];
    incoming[function.entry.0] = Some(HashSet::from([MirScopeId(0)]));
    let mut work = VecDeque::from([function.entry]);
    while let Some(block_id) = work.pop_front() {
        let mut active = incoming[block_id.0]
            .clone()
            .expect("queued task block has an incoming scope state");
        for statement in &function.blocks[block_id.0].statements {
            match statement {
                MirStatement::ScopeEnter { scope, .. } => {
                    if !active.insert(*scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' enters task scope s{} while it is already active",
                            function.name, scope.0
                        )));
                    }
                }
                MirStatement::ScopeExit { scope } => {
                    if !active.remove(scope) {
                        return Err(Diagnostic::codegen(format!(
                            "MIR function '{}' exits task scope s{} outside its active lifetime",
                            function.name, scope.0
                        )));
                    }
                }
                MirStatement::TaskCreate { scope, .. }
                | MirStatement::TaskJoin { scope, .. }
                | MirStatement::TaskClaimResult { scope, .. }
                | MirStatement::TaskCancel { scope, .. }
                | MirStatement::RaceStart { scope, .. }
                | MirStatement::RaceSelect { scope, .. }
                | MirStatement::TaskWait { scope, .. }
                | MirStatement::TaskFailureOperation { scope, .. }
                | MirStatement::TaskFailurePayload { scope, .. }
                | MirStatement::TaskFailureClaim { scope }
                | MirStatement::TaskFailureRethrow { scope, .. }
                    if !active.contains(scope) =>
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' uses task scope s{} outside its active lifetime",
                        function.name, scope.0
                    )));
                }
                _ => {}
            }
        }
        let Some(terminator) = function.blocks[block_id.0].terminator.as_ref() else {
            continue;
        };
        for target in terminator_targets(terminator) {
            if !reachable.contains(&target) {
                continue;
            }
            if let Some(previous) = &incoming[target.0] {
                if previous != &active {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' merges inconsistent task-scope lifetime at bb{}",
                        function.name, target.0
                    )));
                }
            } else {
                incoming[target.0] = Some(active.clone());
                work.push_back(target);
            }
        }
    }
    Ok(())
}

/// Enforce the value-level task escape rule along every CFG path. A
/// `TaskJoin` destination borrows scope-owned result storage until the
/// matching `TaskClaimResult` transfers the value out of the scope, so a read
/// of that destination is only defined after its claim: earlier reads alias
/// storage the scope still owns and will free at `ScopeExit`. `RaceSelect`
/// transfers the winner value in the same runtime step that ends the race,
/// so its destination is readable immediately.
pub(super) fn verify_task_result_reads(
    function: &MirFunction,
    reachable: &HashSet<MirBlockId>,
    predecessors: &[Vec<(MirBlockId, Vec<MirValueId>)>],
) -> Result<(), Diagnostic> {
    let mut join_destinations = HashMap::<MirValueId, MirTaskId>::new();
    for block in function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
    {
        for statement in &block.statements {
            if let MirStatement::TaskJoin {
                destination, task, ..
            } = statement
            {
                join_destinations.insert(*destination, *task);
            }
        }
    }
    if join_destinations.is_empty() {
        return Ok(());
    }
    // Forward must-analysis, computed in two steps. Step one runs the claim
    // dataflow to a fixpoint: the set of join destinations whose claim has
    // been seen on the current path, intersected at merge points, so a
    // destination claimed on only some paths stays unclaimed after the merge.
    // Step two validates every read against the final sets, which keeps phi
    // edges exact on loops: a phi operand is read on its incoming edge and is
    // therefore checked against the predecessor's outgoing set.
    let mut claimed_incoming = vec![None; function.blocks.len()];
    let mut block_outgoing = vec![None; function.blocks.len()];
    claimed_incoming[function.entry.0] = Some(HashSet::new());
    let mut work = VecDeque::from([function.entry]);
    while let Some(block_id) = work.pop_front() {
        let mut claimed = claimed_incoming[block_id.0]
            .clone()
            .expect("queued task-result block has incoming claim state");
        let block = &function.blocks[block_id.0];
        for statement in &block.statements {
            if let MirStatement::TaskClaimResult { task, .. } = statement {
                if let Some(destination) = join_destinations
                    .iter()
                    .find_map(|(destination, joined)| (*joined == *task).then_some(*destination))
                {
                    claimed.insert(destination);
                }
            }
        }
        block_outgoing[block_id.0] = Some(claimed.clone());
        let Some(terminator) = block.terminator.as_ref() else {
            continue;
        };
        for target in terminator_targets(terminator) {
            if !reachable.contains(&target) {
                continue;
            }
            let unchanged = match &mut claimed_incoming[target.0] {
                Some(previous) => {
                    let before = previous.len();
                    previous.retain(|value| claimed.contains(value));
                    before == previous.len()
                }
                slot => {
                    *slot = Some(claimed.clone());
                    false
                }
            };
            if !unchanged {
                work.push_back(target);
            }
        }
    }
    for block in function
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
    {
        let mut claimed = claimed_incoming[block.id.0]
            .clone()
            .expect("reachable block was visited by the claim dataflow");
        for statement in &block.statements {
            if let MirStatement::TaskClaimResult { task, .. } = statement {
                if let Some(destination) = join_destinations
                    .iter()
                    .find_map(|(destination, joined)| (*joined == *task).then_some(*destination))
                {
                    claimed.insert(destination);
                }
            }
            if let MirStatement::Phi { incoming, .. } = statement {
                for (predecessor, value) in incoming {
                    // This pass runs before general phi validation. Never
                    // trust the incoming block index or assume it was visited:
                    // it must be a reachable CFG predecessor of this block.
                    let predecessor_claims = block_outgoing
                        .get(predecessor.0)
                        .and_then(Option::as_ref)
                        .filter(|_| {
                            predecessors[block.id.0]
                                .iter()
                                .any(|(source, _)| source == predecessor)
                        })
                        .ok_or_else(|| {
                            Diagnostic::codegen(format!(
                                "MIR function '{}' has an invalid Phi predecessor bb{} -> bb{}",
                                function.name, predecessor.0, block.id.0
                            ))
                        })?;
                    check_task_result_read(
                        function,
                        &join_destinations,
                        predecessor_claims,
                        *value,
                    )?;
                }
                continue;
            }
            for operand in statement_operands(statement) {
                check_task_result_read(function, &join_destinations, &claimed, operand)?;
            }
        }
        if let Some(terminator) = block.terminator.as_ref() {
            for operand in terminator_operands(terminator) {
                check_task_result_read(function, &join_destinations, &claimed, operand)?;
            }
        }
    }
    Ok(())
}

fn check_task_result_read(
    function: &MirFunction,
    join_destinations: &HashMap<MirValueId, MirTaskId>,
    claimed: &HashSet<MirValueId>,
    operand: MirValueId,
) -> Result<(), Diagnostic> {
    if claimed.contains(&operand) {
        return Ok(());
    }
    if let Some((_, task)) = join_destinations.get_key_value(&operand) {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' reads task t{} result %{} before the scope claims it",
            function.name, task.0, operand.0
        )));
    }
    Ok(())
}

fn terminator_operands(terminator: &MirTerminator) -> Vec<MirValueId> {
    match terminator {
        MirTerminator::Goto { arguments, .. } => arguments.clone(),
        MirTerminator::Branch { condition, .. } => vec![*condition],
        MirTerminator::Return(value) => value.iter().copied().collect(),
        MirTerminator::Unreachable => Vec::new(),
    }
}

fn verify_task_arguments(
    function: &MirFunction,
    task: MirTaskId,
    arguments: &[MirCallArgument],
    signature: &MirSignature,
) -> Result<(), Diagnostic> {
    if arguments.len() != signature.parameter_types.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' creates task t{} with wrong argument count",
            function.name, task.0
        )));
    }
    let mut seen = vec![false; signature.parameter_types.len()];
    for argument in arguments {
        let Some(expected) = signature.parameter_types.get(argument.parameter).copied() else {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' creates task t{} with invalid parameter index",
                function.name, task.0
            )));
        };
        if std::mem::replace(&mut seen[argument.parameter], true) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' creates task t{} with duplicate argument",
                function.name, task.0
            )));
        }
        check_value_type(function, argument.value, expected)?;
    }
    Ok(())
}

fn verify_race_tasks(
    function: &MirFunction,
    scope: MirScopeId,
    race: &[MirTaskId],
    tasks: &HashMap<MirTaskId, (MirScopeId, MirFunctionId, Vec<MirCallArgument>)>,
    signatures: &HashMap<MirFunctionId, MirSignature>,
) -> Result<Type, Diagnostic> {
    if race.is_empty() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has an empty race",
            function.name
        )));
    }
    let mut seen = HashSet::new();
    let mut result = None;
    for task in race {
        let Some((task_scope, task_function, _)) = tasks.get(task) else {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' races unknown task t{}",
                function.name, task.0
            )));
        };
        if *task_scope != scope || !seen.insert(*task) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has invalid race task t{}",
                function.name, task.0
            )));
        }
        let task_result = signatures[task_function].return_type;
        if let Some(expected) = result {
            if expected != task_result {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' races tasks with different result types",
                    function.name
                )));
            }
        } else {
            result = Some(task_result);
        }
    }
    Ok(result.expect("non-empty race has a result type"))
}
