use super::*;

/// Borrowed parameters are nonescaping access to mutable objects. Track the
/// owning local through projections so argument evaluation cannot invalidate
/// an earlier borrow before the call actually uses it.
pub(super) fn verify_borrow_lifetimes(
    function: &MirFunction,
    reverse_postorder: &[MirBlockId],
) -> Result<(), Diagnostic> {
    type Origins = HashSet<(MirLocalId, bool)>;
    let borrowed = |value: &MirValueId| function.value_ownership[value.0] == MirOwnership::Borrowed;
    let mut origins = vec![Origins::new(); function.value_types.len()];
    let mut local_origins = vec![Origins::new(); function.locals.len()];
    loop {
        let mut changed = false;
        for block in &function.blocks {
            for statement in &block.statements {
                if let MirStatement::Bind {
                    local,
                    value: Some(value),
                    ..
                } = statement
                {
                    if function.locals[local.0].ownership == MirOwnership::Borrowed {
                        let before = local_origins[local.0].len();
                        local_origins[local.0].extend(origins[value.0].iter().copied());
                        changed |= before != local_origins[local.0].len();
                    }
                }
                let Some(destination) = statement_destination(statement).filter(borrowed) else {
                    continue;
                };
                let sources = match statement {
                    MirStatement::BorrowLocal { local, .. } | MirStatement::Read { local, .. } => {
                        if local_origins[local.0].is_empty() {
                            Origins::from([(*local, false)])
                        } else {
                            local_origins[local.0].clone()
                        }
                    }
                    MirStatement::Project { base, .. }
                    | MirStatement::EnumProject { value: base, .. } => origins[base.0]
                        .iter()
                        .map(|(local, _)| (*local, true))
                        .collect(),
                    MirStatement::Move { value, .. } => origins[value.0].clone(),
                    MirStatement::Phi { incoming, .. } => incoming
                        .iter()
                        .flat_map(|(_, value)| origins[value.0].iter().copied())
                        .collect(),
                    _ => Origins::new(),
                };
                let before = origins[destination.0].len();
                origins[destination.0].extend(sources);
                changed |= before != origins[destination.0].len();
            }
        }
        if !changed {
            break;
        }
    }

    let mut incoming =
        vec![IndexSet::<MirValueId>::empty(function.value_types.len()); function.blocks.len()];
    let mut outgoing = incoming.clone();
    loop {
        let mut changed = false;
        // Liveness flows backward: visit successors before predecessors.
        for id in reverse_postorder.iter().rev() {
            let block = &function.blocks[id.0];
            let mut live = IndexSet::empty(function.value_types.len());
            if let Some(terminator) = &block.terminator {
                for target in terminator_targets(terminator) {
                    live.union_with(&incoming[target.0]);
                }
                match terminator {
                    MirTerminator::Return(Some(value)) if borrowed(value) => {
                        live.insert(*value);
                    }
                    MirTerminator::Goto { arguments, .. } => {
                        live.extend(arguments.iter().filter(|value| borrowed(value)).copied())
                    }
                    _ => {}
                }
            }
            outgoing[id.0] = live.clone();
            for statement in block.statements.iter().rev() {
                if let Some(destination) = statement_destination(statement) {
                    live.remove(&destination);
                }
                live.extend(statement_operands(statement).into_iter().filter(borrowed));
            }
            if live != incoming[id.0] {
                incoming[id.0] = live;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Keep diagnostic selection in block id order.
    let mut ordered = reverse_postorder.to_vec();
    ordered.sort_by_key(|id| id.0);
    for id in &ordered {
        let mut live = outgoing[id.0].clone();
        for statement in function.blocks[id.0].statements.iter().rev() {
            if let Some(destination) = statement_destination(statement) {
                live.remove(&destination);
            }
            if let MirStatement::TakeLocal { local, .. } | MirStatement::DropLocal { local, .. } =
                statement
            {
                if live
                    .iter()
                    .any(|value| origins[value.0].iter().any(|(root, _)| root == local))
                {
                    if let Some(declaration) = function.source.mutable_locals.get(local) {
                        return Err(Diagnostic::semantic(
                            format!(
                                "cannot consume or reassign '{}' while a borrow is still in use",
                                function.locals[local.0].name
                            ),
                            function.statement_span(statement).unwrap_or(*declaration),
                        ));
                    }
                    return Err(Diagnostic::codegen(format!(
                        "cannot consume '{}' while a borrow is still in use",
                        function.locals[local.0].name
                    )));
                }
            }
            let access = matches!(
                statement,
                MirStatement::Call { .. }
                    | MirStatement::MethodCall { .. }
                    | MirStatement::CallIndirect { .. }
                    | MirStatement::RuntimeCall { .. }
                    | MirStatement::Suspend { .. }
                    | MirStatement::HandlerRequest { .. }
                    | MirStatement::ResumableRequest { .. }
                    | MirStatement::Store { .. }
            );
            if access {
                let arguments: Vec<_> = statement_operands(statement)
                    .into_iter()
                    .filter(borrowed)
                    .collect();
                let mut seen = HashSet::new();
                for argument in &arguments {
                    let mut roots: Vec<_> = origins[argument.0]
                        .iter()
                        .map(|(local, _)| *local)
                        .collect();
                    roots.sort_by_key(|local| local.0);
                    roots.dedup();
                    for root in roots {
                        if !seen.insert(root) {
                            if let Some(span) = function.source.mutable_capture {
                                return Err(Diagnostic::semantic(
                                    "overlapping closure environment borrows in a call",
                                    function.statement_span(statement).unwrap_or(span),
                                ));
                            }
                            return Err(Diagnostic::codegen("overlapping class borrows in a call"));
                        }
                        if live.iter().any(|value| {
                            value != *argument && origins[value.0].contains(&(root, true))
                        }) {
                            if let Some(span) = function.source.mutable_capture {
                                return Err(Diagnostic::semantic("cannot access a closure environment while a borrow of its field is still in use", function.statement_span(statement).unwrap_or(span)));
                            }
                            return Err(Diagnostic::codegen(
                                "cannot access a class while a borrow of its field is still in use",
                            ));
                        }
                    }
                }
            }
            live.extend(statement_operands(statement).into_iter().filter(borrowed));
        }
    }
    Ok(())
}
