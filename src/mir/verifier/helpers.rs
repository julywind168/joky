//! Shared CFG, definition, type, and ownership helpers for MIR verification.

use super::*;
use crate::sema::type_name;

pub(super) fn statement_destination(statement: &MirStatement) -> Option<MirValueId> {
    match statement {
        MirStatement::Unit { destination }
        | MirStatement::Const { destination, .. }
        | MirStatement::Read { destination, .. }
        | MirStatement::BorrowLocal { destination, .. }
        | MirStatement::TakeLocal { destination, .. }
        | MirStatement::Unary { destination, .. }
        | MirStatement::Binary { destination, .. }
        | MirStatement::Numeric { destination, .. }
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
        | MirStatement::ResumableRequest { destination, .. }
        | MirStatement::HandlerRequest { destination, .. }
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
        MirStatement::Resume { .. } => None,
        MirStatement::HandlerEnter { .. }
        | MirStatement::HandlerExit
        | MirStatement::TaskFailureClaim { .. }
        | MirStatement::ScopeEnter { .. }
        | MirStatement::ScopeExit { .. }
        | MirStatement::TaskCreate { .. }
        | MirStatement::TaskClaimResult { .. }
        | MirStatement::TaskCancel { .. }
        | MirStatement::RaceStart { .. } => None,
    }
}

/// The resolved signature of a compiled function or method, used to validate
/// `Call` and `MethodCall` statements against their declarations.
#[derive(Clone)]
pub(crate) struct MirSignature {
    pub(crate) receiver_ownership: Option<MirOwnership>,
    pub(crate) parameter_types: Vec<Type>,
    pub(crate) parameter_ownership: Vec<MirOwnership>,
    pub(crate) return_type: Type,
    /// Foreign callees accept owned closure values for callback parameters:
    /// codegen passes the trampoline address and releases the environment.
    pub(crate) foreign: bool,
}

/// Compute post-order traversal of the CFG reachable from `entry`.
pub(super) fn rpo_sequence(
    block: MirBlockId,
    function: &MirFunction,
    visited: &mut HashSet<MirBlockId>,
    order: &mut Vec<MirBlockId>,
) {
    if !visited.insert(block) {
        return;
    }
    if let Some(terminator) = &function.blocks[block.0].terminator {
        for target in terminator_targets(terminator) {
            rpo_sequence(target, function, visited, order);
        }
    }
    order.push(block);
}

/// Compute dominator sets by iterating predecessor intersections. The result
/// is indexed by `MirBlockId.0`.
pub(super) fn compute_dominators(
    function: &MirFunction,
    reverse_postorder: &[MirBlockId],
) -> Vec<IndexSet<MirBlockId>> {
    let n = function.blocks.len();
    let all_blocks = IndexSet::<MirBlockId>::full(n);

    // Dominators of entry = {entry}; all others start as all blocks.
    let mut doms = vec![all_blocks.clone(); n];
    doms[function.entry.0] = {
        let mut s = IndexSet::empty(n);
        s.insert(function.entry);
        s
    };

    // Collect predecessors (ignoring edge arguments).
    let mut preds = vec![Vec::new(); n];
    for block in &function.blocks {
        if let Some(terminator) = &block.terminator {
            for target in terminator_targets(terminator) {
                if target.0 < n {
                    preds[target.0].push(block.id);
                }
            }
        }
    }

    // Iterate until fixpoint.
    let mut changed = true;
    while changed {
        changed = false;
        for &block_id in reverse_postorder {
            if block_id == function.entry {
                continue;
            }
            let mut new_doms = if let Some(first) = preds[block_id.0].first() {
                doms[first.0].clone()
            } else {
                IndexSet::empty(n)
            };
            for pred in &preds[block_id.0][1..] {
                new_doms.intersect_with(&doms[pred.0]);
            }
            new_doms.insert(block_id);
            if new_doms != doms[block_id.0] {
                doms[block_id.0] = new_doms;
                changed = true;
            }
        }
    }
    doms
}

pub(super) fn local_use_after_move(
    function: &MirFunction,
    local: MirLocalId,
    operation: &str,
) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' attempts to {operation} local '{}' after move",
        function.name, function.locals[local.0].name
    ))
}

pub(crate) fn statement_operands(statement: &MirStatement) -> Vec<MirValueId> {
    match statement {
        MirStatement::Unary { operand, .. }
        | MirStatement::EnumTag { value: operand, .. }
        | MirStatement::EnumProject { value: operand, .. }
        | MirStatement::Project { base: operand, .. }
        | MirStatement::Dup { value: operand, .. }
        | MirStatement::Move { value: operand, .. }
        | MirStatement::Drop { value: operand, .. }
        | MirStatement::Deinit { value: operand, .. } => vec![*operand],
        MirStatement::Binary { left, right, .. } => vec![*left, *right],
        MirStatement::Numeric { arguments, .. } => arguments.clone(),
        MirStatement::Call { arguments, .. }
        | MirStatement::EnumConstruct { arguments, .. }
        | MirStatement::RuntimeCall { arguments, .. }
        | MirStatement::CownAcquire { arguments, .. }
        | MirStatement::HandlerRequest { arguments, .. } => {
            arguments.iter().map(|argument| argument.value).collect()
        }
        MirStatement::ResumableRequest { arguments, .. } => {
            arguments.iter().map(|argument| argument.value).collect()
        }
        MirStatement::Suspend { arguments, .. } => {
            arguments.iter().map(|argument| argument.value).collect()
        }
        MirStatement::TaskCreate { arguments, .. } => {
            arguments.iter().map(|argument| argument.value).collect()
        }
        MirStatement::Tuple { elements, .. }
        | MirStatement::Construct {
            fields: elements, ..
        } => elements.clone(),
        MirStatement::Store {
            receiver, value, ..
        } => vec![*receiver, *value],
        MirStatement::MethodCall {
            receiver,
            arguments,
            ..
        } => std::iter::once(*receiver)
            .chain(arguments.iter().map(|argument| argument.value))
            .collect(),
        MirStatement::CallIndirect {
            callee, arguments, ..
        } => std::iter::once(*callee)
            .chain(arguments.iter().map(|argument| argument.value))
            .collect(),
        MirStatement::FunctionValue { captures, .. } => captures.clone(),
        MirStatement::DynamicValue { value, .. } | MirStatement::DynamicUpcast { value, .. } => {
            vec![*value]
        }
        MirStatement::Bind { value, .. } => value.iter().copied().collect(),
        MirStatement::Phi { incoming, .. } => incoming.iter().map(|(_, value)| *value).collect(),
        MirStatement::Unit { .. }
        | MirStatement::Const { .. }
        | MirStatement::TaskPoll { .. }
        | MirStatement::TaskCancelled { .. }
        | MirStatement::TaskFailureOperation { .. }
        | MirStatement::TaskFailurePayload { .. }
        | MirStatement::TaskFailureClaim { .. }
        | MirStatement::TaskFailureRethrow { .. }
        | MirStatement::Read { .. }
        | MirStatement::BorrowLocal { .. }
        | MirStatement::TakeLocal { .. }
        | MirStatement::DropLocal { .. }
        | MirStatement::HandlerEnter { .. }
        | MirStatement::HandlerExit
        | MirStatement::Resume { .. }
        | MirStatement::ScopeEnter { .. }
        | MirStatement::ScopeExit { .. }
        | MirStatement::TaskWait { .. }
        | MirStatement::TaskJoin { .. }
        | MirStatement::TaskClaimResult { .. }
        | MirStatement::TaskCancel { .. }
        | MirStatement::RaceStart { .. }
        | MirStatement::RaceSelect { .. } => Vec::new(),
        MirStatement::TaskAbort { arguments, .. } => arguments.clone(),
    }
}

pub(super) fn consumed_statement_values(
    function: &MirFunction,
    statement: &MirStatement,
) -> Vec<MirValueId> {
    let consumed = |value: &MirValueId| {
        matches!(
            function.value_ownership[value.0],
            MirOwnership::Owned | MirOwnership::Shared
        )
    };
    match statement {
        MirStatement::Move { value, .. }
        | MirStatement::Drop { value, .. }
        | MirStatement::Deinit { value, .. } => vec![*value],
        MirStatement::Bind {
            local,
            value: Some(value),
            ..
        } if matches!(
            function.locals[local.0].ownership,
            MirOwnership::Owned | MirOwnership::Shared
        ) =>
        {
            vec![*value]
        }
        MirStatement::Call { arguments, .. }
        | MirStatement::EnumConstruct { arguments, .. }
        | MirStatement::ResumableRequest { arguments, .. }
        | MirStatement::HandlerRequest { arguments, .. }
        | MirStatement::Suspend { arguments, .. }
        | MirStatement::CownAcquire { arguments, .. }
        | MirStatement::TaskCreate { arguments, .. } => arguments
            .iter()
            .map(|argument| argument.value)
            .filter(consumed)
            .collect(),
        MirStatement::TaskAbort { arguments, .. } => {
            arguments.iter().copied().filter(consumed).collect()
        }
        MirStatement::RuntimeCall {
            intrinsic,
            arguments,
            ..
        } if !matches!(intrinsic, RuntimeIntrinsic::CownPayload(_)) => arguments
            .iter()
            .map(|argument| argument.value)
            .filter(consumed)
            .collect(),
        MirStatement::MethodCall {
            receiver,
            arguments,
            ..
        } => std::iter::once(*receiver)
            .chain(arguments.iter().map(|argument| argument.value))
            .filter(consumed)
            .collect(),
        MirStatement::FunctionValue { captures, .. } => {
            captures.iter().copied().filter(consumed).collect()
        }
        MirStatement::DynamicValue { value, .. } | MirStatement::DynamicUpcast { value, .. } => {
            std::iter::once(*value).filter(consumed).collect()
        }
        MirStatement::Tuple { elements, .. }
        | MirStatement::Construct {
            fields: elements, ..
        } => elements.iter().copied().filter(consumed).collect(),
        MirStatement::Store { value, .. } if consumed(value) => vec![*value],
        _ => Vec::new(),
    }
}

pub(super) fn ownership_use_error(
    function: &MirFunction,
    value: MirValueId,
    context: &str,
) -> Diagnostic {
    Diagnostic::codegen(format!(
        "MIR function '{}' uses value %{} after move or drop in {}",
        function.name, value.0, context
    ))
}

pub(super) fn reachable_blocks(function: &MirFunction) -> Result<HashSet<MirBlockId>, Diagnostic> {
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::from([function.entry]);
    while let Some(block_id) = queue.pop_front() {
        if block_id.0 >= function.blocks.len() || !reachable.insert(block_id) {
            if block_id.0 >= function.blocks.len() {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' jumps to an invalid block",
                    function.name
                )));
            }
            continue;
        }
        let block = &function.blocks[block_id.0];
        let Some(terminator) = &block.terminator else {
            continue;
        };
        queue.extend(terminator_targets(terminator));
    }
    Ok(reachable)
}

pub(super) fn terminator_targets(terminator: &MirTerminator) -> Vec<MirBlockId> {
    match terminator {
        MirTerminator::Goto { target, .. } => vec![*target],
        MirTerminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        MirTerminator::Return(_) => Vec::new(),
        MirTerminator::Unreachable => Vec::new(),
    }
}

pub(super) fn terminator_edges(terminator: &MirTerminator) -> Vec<(MirBlockId, Vec<MirValueId>)> {
    match terminator {
        MirTerminator::Goto { target, arguments } => vec![(*target, arguments.clone())],
        MirTerminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![(*then_block, Vec::new()), (*else_block, Vec::new())],
        MirTerminator::Return(_) => Vec::new(),
        MirTerminator::Unreachable => Vec::new(),
    }
}

pub(super) fn check_destination_type(
    function: &MirFunction,
    value: MirValueId,
    expected: Type,
) -> Result<(), Diagnostic> {
    check_value_type(function, value, expected)
}

pub(super) fn check_value_exists(
    function: &MirFunction,
    value: MirValueId,
) -> Result<Type, Diagnostic> {
    function.value_types.get(value.0).copied().ok_or_else(|| {
        Diagnostic::codegen(format!(
            "MIR function '{}' references value %{} without a type",
            function.name, value.0
        ))
    })
}

pub(super) fn check_value_type(
    function: &MirFunction,
    value: MirValueId,
    expected: Type,
) -> Result<(), Diagnostic> {
    let actual = check_value_exists(function, value)?;
    if actual != expected {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has type mismatch for %{}: expected {}, found {}",
            function.name,
            value.0,
            type_name(expected),
            type_name(actual)
        )));
    }
    Ok(())
}

pub(super) fn check_definition(
    definitions: &IndexSet<MirValueId>,
    value: MirValueId,
    function: &MirFunction,
) -> Result<(), Diagnostic> {
    if definitions.contains(&value) {
        Ok(())
    } else {
        Err(Diagnostic::codegen(format!(
            "MIR function '{}' uses an undefined value",
            function.name
        )))
    }
}

/// Validate a call's arguments against a callee signature: labels/positional
/// ordering, duplicate or excess arguments, missing arguments, and the type of
/// each argument.
pub(super) fn validate_call_arguments(
    function: &MirFunction,
    callee: &str,
    arguments: &[MirCallArgument],
    signature: &MirSignature,
    available: &IndexSet<MirValueId>,
    types: &TypeTable,
) -> Result<(), Diagnostic> {
    let mut ordered = vec![None; signature.parameter_types.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' passes an out-of-range argument to '{callee}'",
                function.name
            )));
        }
        if ordered[index].is_some() {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' passes a duplicate argument to '{callee}'",
                function.name
            )));
        }
        ordered[index] = Some(argument);
    }
    for (index, (argument, expected)) in ordered
        .into_iter()
        .zip(&signature.parameter_types)
        .enumerate()
    {
        let argument = argument.ok_or_else(|| {
            Diagnostic::codegen(format!(
                "MIR function '{}' passes too few arguments to '{callee}'",
                function.name
            ))
        })?;
        check_definition(available, argument.value, function)?;
        check_value_type(function, argument.value, *expected)?;
        let expected_ownership = signature
            .parameter_ownership
            .get(index)
            .copied()
            .unwrap_or_else(|| ownership_for_type(*expected, types));
        let owned_into_foreign_copy = signature.foreign
            && matches!(expected_ownership, MirOwnership::Copy)
            && matches!(expected, Type::Function(_))
            && function.value_ownership[argument.value.0] == MirOwnership::Owned;
        if function.value_ownership[argument.value.0] != expected_ownership
            && !owned_into_foreign_copy
        {
            return Err(Diagnostic::codegen(format!("MIR function '{}' passes a borrowed or incompatible value to {callee} parameter {index} requiring {expected_ownership:?}", function.name)));
        }
    }
    Ok(())
}

pub(super) fn check_owned_argument(
    function: &MirFunction,
    value: MirValueId,
    expected: Type,
    types: &TypeTable,
    context: &str,
) -> Result<(), Diagnostic> {
    if ownership_for_type(expected, types) == MirOwnership::Owned
        && function.value_ownership[value.0] != MirOwnership::Owned
    {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' passes a borrowed value to owned {context}",
            function.name
        )));
    }
    Ok(())
}

/// Effect requests cross a byte-oriented runtime boundary. Their ownership
/// mode must therefore be exact: accepting an owned value as a shared
/// parameter (or vice versa) would make the request descriptor disagree with
/// the MIR value and can cause a use-after-drop or leak.
pub(super) fn verify_effect_argument_ownership(
    function: &MirFunction,
    arguments: &[MirCallArgument],
    parameter_types: &[Type],
    parameter_ownership: &[MirOwnership],
    types: &TypeTable,
    context: &str,
) -> Result<(), Diagnostic> {
    for argument in arguments {
        let Some(expected) = parameter_types.get(argument.parameter).copied() else {
            continue;
        };
        let expected_ownership = parameter_ownership
            .get(argument.parameter)
            .copied()
            .unwrap_or_else(|| ownership_for_type(expected, types));
        let actual_ownership = function.value_ownership[argument.value.0];
        if actual_ownership != expected_ownership {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' passes {:?} value %{} to {} parameter {} requiring {:?}",
                function.name,
                actual_ownership,
                argument.value.0,
                context,
                argument.parameter,
                expected_ownership
            )));
        }
    }
    Ok(())
}
