//! Shape constraints for continuation blocks, ensuring the verifier stays
//! synchronized with the capability analysis and machine entry codegen.
//!
//! Each capability admitted in phase 1a must have a corresponding shape
//! constraint here to prevent codegen from assuming invariants the verifier
//! does not enforce.

use super::*;

/// Verify shape constraints for suspending continuation blocks.
///
/// These constraints ensure that:
/// - Suspend statements are the last statement in their block (machine entry
///   stops processing at Suspend and returns immediately)
/// - TaskAbort blocks end in Return with the abort destination (fixed abort
///   shape for machine entry compilation)
/// - Resume blocks are reachable from the resume entry
/// - All resume-tail terminators are supported by machine entry
pub(super) fn verify_suspending_continuation_shapes(
    function: &MirFunction,
    continuation: &MirContinuation,
    types: &TypeTable,
) -> Result<(), Diagnostic> {
    if !continuation.kind.is_suspending() {
        return Ok(());
    }

    if !crate::mir::continuation_capability::supports_machine_return(function.return_type, types) {
        return Err(Diagnostic::codegen(format!(
            "MIR continuation c{} return type cannot be represented by machine-entry ABI; use a supported return type or keep the operation non-suspending",
            continuation.id.0
        )));
    }

    // Check that resume blocks are reachable
    let reachable = crate::mir::continuation_capability::continuation_resume_blocks(
        function,
        continuation.resume_block,
    );
    if reachable.is_empty() {
        return Err(Diagnostic::codegen(format!(
            "MIR continuation c{} has no reachable blocks from its resume entry",
            continuation.id.0
        )));
    }

    // A machine entry must be able to close every reachable resume block.  A
    // malformed terminator is a MIR shape error and must be reported during
    // verification rather than turning into a runtime synchronous fallback.
    for block_id in reachable.iter() {
        let block = &function.blocks[block_id.0];
        for statement in &block.statements {
            if let MirStatement::Resume {
                continuation: nested,
            } = statement
            {
                if function.continuations.iter().any(|item| {
                    item.id == *nested
                        && item.kind.is_suspending()
                        && item.id != continuation.id
                        && item.callee.is_none()
                        && item.operation.is_some()
                }) {
                    return Err(Diagnostic::codegen(format!(
                        "MIR continuation c{} crosses suspending continuation c{} in resume block b{}",
                        continuation.id.0, nested.0, block.id.0
                    )));
                }
            }
        }
        let supported = match block.terminator {
            Some(MirTerminator::Goto { .. })
            | Some(MirTerminator::Branch { .. })
            | Some(MirTerminator::Unreachable) => true,
            Some(MirTerminator::Return(None)) => function.return_type == Type::Unit,
            Some(MirTerminator::Return(Some(value))) => {
                function.value_types.get(value.0) == Some(&function.return_type)
            }
            None => false,
        };
        if !supported {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} resume block b{} has an unsupported terminator",
                continuation.id.0, block.id.0
            )));
        }
    }

    // Find the Suspend statement for this continuation
    let suspend_block = &function.blocks[continuation.suspend_block.0];
    let suspend_statement_index = suspend_block.statements.iter().position(|statement| {
        matches!(
            statement,
            MirStatement::Suspend { continuation: id, .. } | MirStatement::TaskWait { continuation: id, .. } | MirStatement::CownAcquire { continuation: id, .. } if *id == continuation.id
        )
    });

    if let Some(index) = suspend_statement_index {
        // Suspend must be the last statement before the terminator.
        // The machine entry compiler breaks out of the statement loop at Suspend
        // and returns immediately, effectively dropping any subsequent statements.
        if index != suspend_block.statements.len() - 1 {
            return Err(Diagnostic::codegen(format!(
                "MIR continuation c{} Suspend statement must be the last statement in its block",
                continuation.id.0
            )));
        }
    }

    Ok(())
}

/// Verify shape constraints for blocks containing TaskAbort statements.
///
/// The capability analysis admits TaskAbort in resume tails under the
/// assumption of a fixed shape: TaskAbort followed by cleanup (drops),
/// then Return. This ensures the machine entry can compile the abort
/// path correctly and publish the Aborted state.
pub(super) fn verify_abort_block_shapes(function: &MirFunction) -> Result<(), Diagnostic> {
    for block in &function.blocks {
        let abort_index = block
            .statements
            .iter()
            .position(|statement| matches!(statement, MirStatement::TaskAbort { .. }));

        if let Some(index) = abort_index {
            let abort_statement = &block.statements[index];
            let MirStatement::TaskAbort { destination, .. } = abort_statement else {
                unreachable!();
            };

            // After TaskAbort, only cleanup statements (Drop, DropLocal, Deinit)
            // are permitted before the terminator.
            for statement in &block.statements[index + 1..] {
                if !matches!(
                    statement,
                    MirStatement::Drop { .. }
                        | MirStatement::DropLocal { .. }
                        | MirStatement::Deinit { .. }
                ) {
                    return Err(Diagnostic::codegen(format!(
                        "MIR block b{} contains non-cleanup statement after TaskAbort",
                        block.id.0
                    )));
                }
            }

            // The block must terminate with Return using the abort destination
            match block.terminator {
                Some(MirTerminator::Return(Some(value))) if value == *destination => {}
                Some(MirTerminator::Return(None)) if function.return_type == Type::Unit => {
                    // Unit abort blocks may Return(None) since the destination is Unit
                }
                _ => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR block b{} with TaskAbort must terminate with Return(abort_destination)",
                        block.id.0
                    )));
                }
            }
        }
    }

    Ok(())
}
