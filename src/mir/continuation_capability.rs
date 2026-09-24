//! Static machine-entry capability analysis for suspending continuations.
//!
//! This module is the single source of truth for whether a `Suspending`
//! continuation can run on an independent machine entry. Codegen must consume
//! [`machine_entry_blocker`] instead of deriving the answer itself, and
//! diagnostics and dumps report from the same query so every synchronous
//! fallback has a statically queryable reason.

use std::collections::HashSet;

use super::{
    MirBlockId, MirContinuation, MirContinuationKind, MirFunction, MirStatement, MirTerminator,
};
use crate::sema::{Type, TypeTable};

/// Why a suspending continuation cannot run on an independent machine entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineEntryBlocker {
    /// The function return type has no machine-entry ABI representation.
    ReturnType,
    /// No block is reachable from the resume entry.
    UnreachableResume,
    /// A resume-tail statement is outside the machine-entry whitelist.
    UnsupportedStatement(&'static str),
    /// A resume-tail terminator cannot close a stackless entry.
    UnsupportedTerminator,
}

impl MachineEntryBlocker {
    pub(crate) fn reason(self) -> String {
        match self {
            Self::ReturnType => "return type has no machine-entry ABI".to_owned(),
            Self::UnreachableResume => "no block is reachable from the resume entry".to_owned(),
            Self::UnsupportedStatement(name) => {
                format!("resume tail statement '{name}' is outside the machine entry whitelist")
            }
            Self::UnsupportedTerminator => {
                "resume tail terminator cannot close a stackless entry".to_owned()
            }
        }
    }
}

fn statement_label(statement: &MirStatement) -> &'static str {
    match statement {
        MirStatement::ResumableRequest { .. } => "resumable_request",
        MirStatement::Store { .. } => "store",
        MirStatement::Resume { .. } => "resume marker of another continuation",
        _ => "unsupported statement",
    }
}

/// Returns the first reason the continuation cannot run on an independent
/// machine entry, or `None` when the resume tail is fully supported and the
/// continuation may detach from the original native stack.
pub(crate) fn machine_entry_blocker(
    function: &MirFunction,
    continuation: &MirContinuation,
    types: &TypeTable,
) -> Option<MachineEntryBlocker> {
    if !supports_machine_return(function.return_type, types) {
        return Some(MachineEntryBlocker::ReturnType);
    }
    let reachable = continuation_resume_blocks(function, continuation.resume_block);
    if reachable.is_empty() {
        return Some(MachineEntryBlocker::UnreachableResume);
    }
    for block_id in reachable {
        let block = &function.blocks[block_id.0];
        for statement in &block.statements {
            match statement {
                // Resume markers identify the continuation edge that produced
                // the block. A standalone machine entry may traverse
                // subsequent resume markers when a resumed tail performs
                // another suspend.
                MirStatement::Resume { continuation: id } if *id == continuation.id => {}
                // Direct calls to suspending functions are sound: the callee
                // uses the Pending ABI, so a suspend inside the callee hands
                // the caller's re-armed activation to the scheduler instead of
                // occupying this worker's native frame.
                MirStatement::Call { .. }
                | MirStatement::MethodCall { .. } => {}
                // Indirect calls use the same nested Pending ABI as direct
                // calls. Their continuation is compiled by the machine-entry
                // call helper, so a suspending closure is safe here too.
                MirStatement::CallIndirect { .. } => {}
                MirStatement::TaskPoll { .. }
                | MirStatement::TaskCancelled { .. }
                | MirStatement::Suspend { .. } | MirStatement::TaskWait { .. } | MirStatement::CownAcquire { .. }
                | MirStatement::Unit { .. }
                | MirStatement::Const { .. }
                | MirStatement::Read { .. }
                | MirStatement::BorrowLocal { .. }
                | MirStatement::TakeLocal { .. }
                | MirStatement::Unary { .. }
                | MirStatement::Binary { .. }
                | MirStatement::Numeric { .. }
                | MirStatement::Project { .. }
                | MirStatement::EnumTag { .. }
                | MirStatement::EnumProject { .. }
                | MirStatement::FunctionValue { .. }
                | MirStatement::DynamicValue { .. }
                | MirStatement::DynamicUpcast { .. }
                | MirStatement::Tuple { .. }
                | MirStatement::EnumConstruct { .. }
                | MirStatement::Construct { .. }
                | MirStatement::Dup { .. }
                | MirStatement::Move { .. }
                | MirStatement::Drop { .. }
                | MirStatement::Deinit { .. }
                | MirStatement::DropLocal { .. }
                | MirStatement::Bind { .. }
                | MirStatement::Phi { .. }
                | MirStatement::HandlerEnter { .. }
                | MirStatement::HandlerExit
                // Normal handler requests dispatch synchronously through the
                // pinned handler frame chain, so a resumed tail may issue
                // further Normal requests.
                | MirStatement::HandlerRequest { .. }
                // Detached branch tasks are spawned through heap-backed
                // storage when a continuation resumes off the original
                // native stack.
                | MirStatement::TaskCreate { .. }
                | MirStatement::ScopeEnter { .. }
                | MirStatement::ScopeExit { .. }
                | MirStatement::TaskJoin { .. }
                | MirStatement::TaskClaimResult { .. }
                | MirStatement::TaskCancel { .. }
                | MirStatement::RaceStart { .. }
                | MirStatement::RaceSelect { .. }
                | MirStatement::TaskFailureOperation { .. }
                | MirStatement::TaskFailurePayload { .. }
                | MirStatement::TaskFailureClaim { .. }
                | MirStatement::TaskFailureRethrow { .. }
                // Abort statements follow the fixed abort block shape
                // (TaskAbort, cleanup, Return), which the machine entry
                // compiles directly; the completion callback then publishes
                // the Aborted task state from the recorded abort operation.
                | MirStatement::TaskAbort { .. }
                | MirStatement::RuntimeCall { .. } => {}
                // ResumableRequest uses the same synchronous handler frame dispatch
                // as HandlerRequest, differing only in continuation kind and heap
                // storage allocation. The machine entry compiles it identically.
                | MirStatement::ResumableRequest { .. } => {}
                MirStatement::Store { .. } => {}
                // Resume markers of Normal continuations are no-ops: the
                // synchronous request already materialized their result.
                // Resumable continuations also complete synchronously through
                // the handler frame, so their Resume markers are similarly safe.
                // Only Suspending continuations may not be crossed inside a
                // foreign machine region.
                MirStatement::Resume { continuation: id } => {
                    let metadata = function
                        .continuations
                        .iter()
                        .find(|item| item.id == *id);
                    if metadata.is_some_and(|item| {
                        item.kind == MirContinuationKind::Suspending
                            && item.callee.is_none()
                            && item.operation.is_some()
                    }) {
                        return Some(MachineEntryBlocker::UnsupportedStatement(statement_label(
                            statement,
                        )));
                    }
                }
            }
        }
        let terminator_supported = match block.terminator {
            Some(MirTerminator::Goto { .. })
            | Some(MirTerminator::Branch { .. })
            | Some(MirTerminator::Unreachable) => true,
            Some(MirTerminator::Return(None)) => function.return_type == Type::Unit,
            Some(MirTerminator::Return(Some(value))) => {
                function.value_types[value.0] == function.return_type
            }
            None => false,
        };
        if !terminator_supported {
            return Some(MachineEntryBlocker::UnsupportedTerminator);
        }
    }
    None
}

pub(crate) fn continuation_resume_blocks(
    function: &MirFunction,
    start: MirBlockId,
) -> Vec<MirBlockId> {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    let mut blocks = Vec::new();
    while let Some(block) = pending.pop() {
        if !visited.insert(block) || block.0 >= function.blocks.len() {
            continue;
        }
        blocks.push(block);
        // A machine entry re-arms the continuation at the next Suspend and
        // returns immediately. Its following resume block belongs to a
        // separate machine entry, so including it here would incorrectly
        // reject a valid multi-suspend tail due to the next Resume marker.
        if function.blocks[block.0].statements.iter().any(|statement| {
            matches!(
                statement,
                MirStatement::Suspend { .. } | MirStatement::TaskWait { .. }
            )
        }) {
            continue;
        }
        match function.blocks[block.0].terminator.as_ref() {
            Some(MirTerminator::Goto { target, .. }) => pending.push(*target),
            Some(MirTerminator::Branch {
                then_block,
                else_block,
                ..
            }) => {
                pending.push(*else_block);
                pending.push(*then_block);
            }
            Some(MirTerminator::Return(_)) | Some(MirTerminator::Unreachable) | None => {}
        }
    }
    blocks
}

pub(crate) fn supports_machine_return(ty: Type, types: &TypeTable) -> bool {
    match ty {
        Type::Unit
        | Type::CPtr(_)
        | Type::CMutPtr(_)
        | Type::CArray(_)
        | Type::CStr
        | Type::String
        | Type::Class(_)
        | Type::Native(_)
        | Type::CCallback
        | Type::Bool
        | Type::Duration
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::F32
        | Type::F64 => true,
        Type::Tuple(id) => types
            .tuple_elements(id)
            .iter()
            .all(|element| supports_machine_return(*element, types)),
        Type::Struct(id) => types
            .struct_fields(id)
            .iter()
            .all(|(_, field)| supports_machine_return(*field, types)),
        Type::Enum(id) => types.enum_variants(id).iter().all(|variant| {
            variant
                .fields
                .iter()
                .all(|(_, field)| supports_machine_return(*field, types))
        }),
        Type::Option(id) => supports_machine_return(types.option_type(id), types),
        Type::Result(id) => {
            let (ok, err) = types.result_types(id);
            supports_machine_return(ok, types) && supports_machine_return(err, types)
        }
        // Bytes is represented by a single managed pointer in the flattened
        // ABI, just like the other managed leaf types above.  It can therefore
        // be transferred through continuation result storage without reading
        // the suspended native frame.
        Type::Batch | Type::SeqBuilder | Type::Bytes | Type::BytesCursor | Type::Hasher => true,
        Type::MapCursor(_) | Type::MapKeyCursor(_) | Type::MapValueCursor(_) => true,
        Type::MutListCursor(_) | Type::MutMapCursor(_) | Type::MutSetCursor(_) => true,
        Type::MutBytes => true,
        // List, Map, and Set are also represented as single managed pointers
        // in the flattened ABI, matching other managed types. They can be
        // transferred through continuation result storage.
        Type::List(_) | Type::MutList(_) => true,
        Type::Map(_) | Type::MutMap(_) | Type::MutSet(_) => true,
        // Function is represented as a code pointer plus environment pointer.
        // Both components can be transferred through result storage.
        Type::Function(_) | Type::Dyn(_) | Type::Cown(_) => true,
        Type::Param(_) | Type::SelfType | Type::Associated(_) => false,
    }
}

impl super::MirProgram {
    /// One line per suspending continuation: whether it can run on an
    /// independent machine entry, or the static reason it falls back.
    pub(crate) fn dump_continuation_capabilities(&self) -> String {
        let mut lines = Vec::new();
        for function in &self.functions {
            for continuation in &function.continuations {
                if !continuation.kind.is_suspending() || continuation.callee.is_some() {
                    continue;
                }
                let status = match machine_entry_blocker(function, continuation, &self.types) {
                    None => "machine-entry capable".to_owned(),
                    Some(blocker) => format!("verifier-required rejection ({})", blocker.reason()),
                };
                lines.push(format!(
                    "function '{}' continuation c{}: {status}",
                    function.name, continuation.id.0
                ));
            }
        }
        lines.join("\n")
    }
}
