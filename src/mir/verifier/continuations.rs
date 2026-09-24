use std::collections::HashSet;

use super::*;

pub(super) fn verify_continuations(
    function: &MirFunction,
    types: &TypeTable,
    dominators: &[IndexSet<MirBlockId>],
) -> Result<(), Diagnostic> {
    let mut seen = HashSet::new();
    let reachable = reachable_blocks(function)?;
    for continuation in &function.continuations {
        if !seen.insert(continuation.id) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has duplicate continuation c{}",
                function.name, continuation.id.0
            )));
        }
        let Some(suspend_block) = function.blocks.get(continuation.suspend_block.0) else {
            return Err(Diagnostic::codegen(
                "MIR continuation suspend block is out of bounds",
            ));
        };
        let Some(_resume_block) = function.blocks.get(continuation.resume_block.0) else {
            return Err(Diagnostic::codegen(
                "MIR continuation resume block is out of bounds",
            ));
        };
        if !reachable.contains(&continuation.suspend_block)
            || !reachable.contains(&continuation.resume_block)
        {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} references an unreachable block",
                continuation.id.0
            )));
        }
        if continuation.locals_before_suspend > function.locals.len() {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} records locals beyond the function local table",
                continuation.id.0
            )));
        }
        if matches!(
            continuation.kind,
            MirContinuationKind::TaskWait | MirContinuationKind::CownAcquire
        ) {
            let requests = function.blocks.iter().flat_map(|b| &b.statements).filter(|s| matches!(s, MirStatement::TaskWait { continuation: id, .. } | MirStatement::CownAcquire { continuation: id, .. } if *id == continuation.id)).count();
            let kind_matches = matches!(
                (continuation.kind, suspend_block.statements.last()),
                (
                    MirContinuationKind::TaskWait,
                    Some(MirStatement::TaskWait { .. })
                ) | (
                    MirContinuationKind::CownAcquire,
                    Some(MirStatement::CownAcquire { .. })
                )
            );
            if !kind_matches
                || requests != 1
                || continuation.operation.is_some()
                || continuation.callee.is_some()
                || !matches!(suspend_block.statements.last(), Some(MirStatement::TaskWait { destination, continuation: id, .. } | MirStatement::CownAcquire { destination, continuation: id, .. }) if *id == continuation.id && Some(*destination) == continuation.resume_destination && function.value_types.get(destination.0) == Some(&Type::Unit))
            {
                return Err(Diagnostic::codegen("invalid internal wait continuation"));
            }
        } else if continuation.is_function_call() {
            verify_function_call(function, continuation, continuation.callee)?;
        } else {
            let Some(operation_id) = continuation.operation else {
                return Err(Diagnostic::codegen(format!(
                    "MIR effect continuation c{} has no operation",
                    continuation.id.0
                )));
            };
            let Some(operation) = types.effects().operation_info(operation_id) else {
                return Err(Diagnostic::codegen(
                    "MIR continuation references an unknown operation",
                ));
            };
            let has_suspend_request = function.blocks.iter().any(|block| {
                block.statements.iter().any(|statement| {
                    matches!(
                        statement,
                        MirStatement::Suspend {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
            });
            let has_resumable_request = function.blocks.iter().any(|block| {
                block.statements.iter().any(|statement| {
                    matches!(
                        statement,
                        MirStatement::ResumableRequest {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
            });
            let has_handler_request = function.blocks.iter().any(|block| {
                block.statements.iter().any(|statement| {
                    matches!(
                        statement,
                        MirStatement::HandlerRequest {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
            });
            let kind_matches_operation = continuation
                .kind
                .matches_operation(operation.mode, operation.suspends)
                && match continuation.kind {
                    crate::mir::MirContinuationKind::TaskWait
                    | crate::mir::MirContinuationKind::CownAcquire => false,
                    crate::mir::MirContinuationKind::Normal => has_handler_request,
                    crate::mir::MirContinuationKind::Suspending => has_suspend_request,
                    crate::mir::MirContinuationKind::Resumable => has_resumable_request,
                };
            if !kind_matches_operation {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} kind does not match operation mode",
                    continuation.id.0
                )));
            }
            let Some(resume_destination) = continuation.resume_destination else {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} has no resume destination",
                    continuation.id.0
                )));
            };
            if function.value_types.get(resume_destination.0) != Some(&operation.return_type) {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} resume destination type does not match operation",
                    continuation.id.0
                )));
            }
            let expected_result_ownership = ownership_for_type(operation.return_type, types);
            if function.value_ownership.get(resume_destination.0)
                != Some(&expected_result_ownership)
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} resume destination ownership does not match operation",
                    continuation.id.0
                )));
            }
        }
        if continuation.suspend_block == continuation.resume_block {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} requires a distinct resume block",
                continuation.id.0
            )));
        }
        if !matches!(
            suspend_block.terminator,
            Some(MirTerminator::Goto { target, .. }) if target == continuation.resume_block
        ) {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} must jump from suspend to resume block",
                continuation.id.0
            )));
        }
        // A resume block is entered by the suspension edge only. Allowing a
        // normal CFG edge to reach it would execute the recovery marker
        // without a valid continuation state.
        for predecessor in &function.blocks {
            let reaches_resume = match predecessor.terminator {
                Some(MirTerminator::Goto { target, .. }) => target == continuation.resume_block,
                Some(MirTerminator::Branch {
                    then_block,
                    else_block,
                    ..
                }) => {
                    then_block == continuation.resume_block
                        || else_block == continuation.resume_block
                }
                _ => false,
            };
            if reaches_resume && predecessor.id != continuation.suspend_block {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} resume block has a normal CFG predecessor",
                    continuation.id.0
                )));
            }
        }
        if let Some(operation_id) = continuation.operation {
            let resume_destination = continuation
                .resume_destination
                .expect("verified destination");
            let suspend_count = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .filter(|statement| {
                    matches!(
                        statement,
                        MirStatement::Suspend {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
                .count();
            let request_count = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .filter(|statement| {
                    matches!(
                        statement,
                        MirStatement::ResumableRequest {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
                .count();
            let handler_request_count = function
                .blocks
                .iter()
                .flat_map(|block| &block.statements)
                .filter(|statement| {
                    matches!(
                        statement,
                        MirStatement::HandlerRequest {
                            continuation: id,
                            ..
                        } if *id == continuation.id
                    )
                })
                .count();
            let request_destination = function.blocks.iter().find_map(|block| {
                block
                    .statements
                    .iter()
                    .find_map(|statement| match statement {
                        MirStatement::Suspend {
                            destination,
                            operation,
                            continuation: id,
                            ..
                        }
                        | MirStatement::ResumableRequest {
                            destination,
                            operation,
                            continuation: id,
                            ..
                        }
                        | MirStatement::HandlerRequest {
                            destination,
                            operation,
                            continuation: id,
                            ..
                        } if *id == continuation.id => Some((*destination, *operation)),
                        _ => None,
                    })
            });
            if request_destination != Some((resume_destination, operation_id)) {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} request destination or operation does not match metadata",
                    continuation.id.0
                )));
            }
            match continuation.kind {
                crate::mir::MirContinuationKind::Suspending if suspend_count != 1 => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} must have exactly one suspend statement",
                        continuation.id.0
                    )));
                }
                crate::mir::MirContinuationKind::Resumable if request_count != 1 => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} must have exactly one resumable request",
                        continuation.id.0
                    )));
                }
                crate::mir::MirContinuationKind::Normal if handler_request_count != 1 => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} must have exactly one normal handler request",
                        continuation.id.0
                    )));
                }
                _ => {}
            }
            if suspend_count
                != usize::from(continuation.kind == crate::mir::MirContinuationKind::Suspending)
                || request_count
                    != usize::from(continuation.kind == crate::mir::MirContinuationKind::Resumable)
                || handler_request_count
                    != usize::from(continuation.kind == crate::mir::MirContinuationKind::Normal)
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} contains a request for the wrong protocol",
                    continuation.id.0
                )));
            }
        }
        let resume_count = function
            .blocks
            .iter()
            .flat_map(|block| &block.statements)
            .filter(|statement| {
                matches!(
                    statement,
                    MirStatement::Resume { continuation: id } if *id == continuation.id
                )
            })
            .count();
        if resume_count != 1 {
            return Err(Diagnostic::codegen(format!(
                "MIR function {} continuation c{} must have exactly one resume marker (found {})",
                function.name, continuation.id.0, resume_count
            )));
        }
        let resume_marker_block = function.blocks.iter().find_map(|block| {
            block
                .statements
                .iter()
                .any(|statement| {
                    matches!(
                        statement,
                        MirStatement::Resume { continuation: id } if *id == continuation.id
                    )
                })
                .then_some(block.id)
        });
        if resume_marker_block != Some(continuation.resume_block) {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} resume marker is outside its resume block",
                continuation.id.0
            )));
        }
        let mut spill_seen = HashSet::new();
        for value in &continuation.spill_values {
            if function.value_types.get(value.0).is_none() {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} spill value is out of bounds",
                    continuation.id.0
                )));
            }
            if !spill_seen.insert(*value) {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} lists a spill value more than once",
                    continuation.id.0
                )));
            }
            if function.value_ownership[value.0] == MirOwnership::Borrowed
                && !function.value_types[value.0].is_native_resource()
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} cannot spill a borrowed value",
                    continuation.id.0
                )));
            }
            let Some(defining_block) = function.blocks.iter().find_map(|block| {
                block
                    .statements
                    .iter()
                    .any(|statement| statement_destination(statement) == Some(*value))
                    .then_some(block.id)
            }) else {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} spill value has no definition",
                    continuation.id.0
                )));
            };
            if !dominators
                .get(continuation.suspend_block.0)
                .is_some_and(|set| set.contains(&defining_block))
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} spill value does not dominate suspend",
                    continuation.id.0
                )));
            }
        }
        if continuation.spill_slots.len() != continuation.spill_values.len() {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} has mismatched spill value and slot metadata",
                continuation.id.0
            )));
        }
        for (expected_slot, slot) in continuation.spill_slots.iter().enumerate() {
            if slot.slot != expected_slot
                || continuation.spill_values.get(expected_slot) != Some(&slot.value)
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} has an invalid spill slot order",
                    continuation.id.0
                )));
            }
            if function.value_ownership.get(slot.value.0) != Some(&slot.ownership) {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} spill slot ownership does not match its value",
                    continuation.id.0
                )));
            }
        }
        let mut frame_locals = HashSet::new();
        for (expected_slot, slot) in continuation.frame_slots.iter().enumerate() {
            if slot.slot != expected_slot
                || slot.local.0 >= continuation.locals_before_suspend
                || !frame_locals.insert(slot.local)
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} has an invalid frame slot order",
                    continuation.id.0
                )));
            }
            let Some(local) = function.locals.get(slot.local.0) else {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} frame local is out of bounds",
                    continuation.id.0
                )));
            };
            if local.ty != slot.ty || local.ownership != slot.ownership {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} frame slot metadata does not match local {}",
                    continuation.id.0, slot.local.0
                )));
            }
            if let Some(value) = slot.value {
                let Some(value_type) = function.value_types.get(value.0) else {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} frame value is out of bounds",
                        continuation.id.0
                    )));
                };
                if *value_type != slot.ty
                    || function.value_ownership.get(value.0) != Some(&slot.ownership)
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} frame value metadata does not match local {}",
                        continuation.id.0, slot.local.0
                    )));
                }
                let Some(defining_block) = function.blocks.iter().find_map(|block| {
                    block
                        .statements
                        .iter()
                        .any(|statement| statement_destination(statement) == Some(value))
                        .then_some(block.id)
                }) else {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} frame value has no definition",
                        continuation.id.0
                    )));
                };
                if !dominators
                    .get(continuation.suspend_block.0)
                    .is_some_and(|set| set.contains(&defining_block))
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} frame value does not dominate suspend",
                        continuation.id.0
                    )));
                }
            }
            // Native resources have stable heap handles. The suspended caller
            // retains the owner until the borrowed call returns.
            if slot.ownership == MirOwnership::Borrowed
                && !slot.ty.is_native_resource()
                && !matches!(slot.ty, Type::Dyn(_))
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR continuation c{} cannot carry borrowed local {}; read or copy the value before suspend",
                    continuation.id.0, slot.local.0
                )));
            }
        }
    }
    for block in &function.blocks {
        for statement in &block.statements {
            if let MirStatement::Call {
                function: callee,
                continuation: Some(id),
                ..
            }
            | MirStatement::MethodCall {
                method: callee,
                continuation: Some(id),
                ..
            } = statement
            {
                if !function
                    .continuations
                    .iter()
                    .any(|metadata| metadata.id == *id && metadata.callee == Some(*callee))
                {
                    return Err(Diagnostic::codegen(
                        "MIR call continuation does not match its callee",
                    ));
                }
            }
            let id = match statement {
                MirStatement::Suspend { continuation, .. }
                | MirStatement::TaskWait { continuation, .. }
                | MirStatement::CownAcquire { continuation, .. }
                | MirStatement::CallIndirect {
                    continuation: Some(continuation),
                    ..
                }
                | MirStatement::Resume { continuation }
                | MirStatement::Call {
                    continuation: Some(continuation),
                    ..
                }
                | MirStatement::MethodCall {
                    continuation: Some(continuation),
                    ..
                } => Some(*continuation),
                _ => None,
            };
            if let Some(id) = id {
                if !seen.contains(&id) {
                    return Err(Diagnostic::codegen(
                        "MIR suspension statement has no continuation metadata",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn verify_function_call(
    function: &MirFunction,
    continuation: &MirContinuation,
    callee: Option<MirFunctionId>,
) -> Result<(), Diagnostic> {
    let calls = function
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .statements
                .iter()
                .filter_map(move |statement| match statement {
                    MirStatement::Call {
                        function: target,
                        continuation: Some(id),
                        destination,
                        ..
                    }
                    | MirStatement::MethodCall {
                        method: target,
                        continuation: Some(id),
                        destination,
                        ..
                    } if *id == continuation.id => Some((block.id, Some(*target), *destination)),
                    MirStatement::CallIndirect {
                        continuation: Some(id),
                        destination,
                        may_suspend: true,
                        ..
                    } if *id == continuation.id => Some((block.id, None, *destination)),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    let valid = continuation.kind == MirContinuationKind::Suspending
        && continuation.operation.is_none()
        && continuation.resume_destination.is_some_and(|destination| {
            calls == [(continuation.suspend_block, callee, destination)]
        })
        && matches!(function.blocks[continuation.suspend_block.0].statements.last(),
            Some(MirStatement::Call { continuation: Some(id), .. }
                | MirStatement::MethodCall { continuation: Some(id), .. }
                | MirStatement::CallIndirect { continuation: Some(id), may_suspend: true, .. })
                if *id == continuation.id);
    if !valid {
        return Err(Diagnostic::codegen(format!(
            "MIR function-call continuation c{} has invalid call metadata",
            continuation.id.0
        )));
    }
    Ok(())
}
