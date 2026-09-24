use super::*;

fn operation_signature(
    types: &TypeTable,
    operation: crate::sema::EffectOperationId,
) -> Result<MirSignature, Diagnostic> {
    let info = types
        .effects()
        .operation_info(operation)
        .ok_or_else(|| Diagnostic::codegen("unknown MIR effect operation"))?;
    Ok(MirSignature {
        foreign: false,
        receiver_ownership: None,
        parameter_ownership: info
            .parameters
            .iter()
            .zip(&info.parameter_borrows)
            .map(|(ty, borrowed)| {
                if *borrowed {
                    MirOwnership::Borrowed
                } else {
                    ownership_for_type(*ty, types)
                }
            })
            .collect(),
        parameter_types: info.parameters.clone(),
        return_type: info.return_type,
    })
}

fn verify_request_protocol(
    function: &MirFunction,
    types: &TypeTable,
    operation: crate::sema::EffectOperationId,
    continuation: Option<(MirContinuationId, MirContinuationKind)>,
    destination: MirValueId,
    block_id: Option<MirBlockId>,
    label: &str,
) -> Result<MirSignature, Diagnostic> {
    let operation_info = types
        .effects()
        .operation_info(operation)
        .ok_or_else(|| Diagnostic::codegen("unknown MIR effect operation"))?;
    // Every request statement binds a continuation, so the protocol match is
    // always decided by `MirContinuationKind::matches_operation`.
    let Some((_, expected_kind)) = continuation else {
        unreachable!("request protocol verification requires a continuation");
    };
    if !expected_kind.matches_operation(operation_info.mode, operation_info.suspends) {
        return Err(Diagnostic::codegen(format!(
            "MIR {label} operation does not match its control protocol"
        )));
    }

    if let Some((continuation_id, expected_kind)) = continuation {
        let metadata = function
            .continuations
            .iter()
            .find(|metadata| metadata.id == continuation_id)
            .ok_or_else(|| {
                Diagnostic::codegen(format!("MIR {label} references an unknown continuation"))
            })?;
        if metadata.kind != expected_kind
            || metadata.operation != Some(operation)
            || metadata.resume_destination != Some(destination)
        {
            return Err(Diagnostic::codegen(format!(
                "MIR {label} does not match continuation metadata"
            )));
        }
        if let Some(block_id) = block_id {
            if metadata.suspend_block != block_id {
                return Err(Diagnostic::codegen(format!(
                    "MIR {label} does not match continuation metadata"
                )));
            }
        }
    }

    operation_signature(types, operation)
}

pub(super) fn verify_handler_request(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    operation: crate::sema::EffectOperationId,
    continuation: MirContinuationId,
    arguments: &[MirCallArgument],
) -> Result<(), Diagnostic> {
    let signature = verify_request_protocol(
        function,
        types,
        operation,
        Some((continuation, MirContinuationKind::Normal)),
        destination,
        None,
        "HandlerRequest",
    )?;
    let operation_info = types
        .effects()
        .operation_info(operation)
        .expect("validated operation");
    validate_call_arguments(
        function,
        "normal handler operation",
        arguments,
        &signature,
        available,
        types,
    )?;
    verify_effect_argument_ownership(
        function,
        arguments,
        &operation_info.parameters,
        &signature.parameter_ownership,
        types,
        "normal handler operation",
    )?;
    check_destination_type(function, destination, signature.return_type)
}

pub(super) fn verify_resumable_request(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    operation: crate::sema::EffectOperationId,
    continuation: MirContinuationId,
    arguments: &[MirCallArgument],
) -> Result<(), Diagnostic> {
    let signature = verify_request_protocol(
        function,
        types,
        operation,
        Some((continuation, MirContinuationKind::Resumable)),
        destination,
        None,
        "ResumableRequest",
    )?;
    let operation_info = types
        .effects()
        .operation_info(operation)
        .expect("validated operation");
    validate_call_arguments(
        function,
        "resumable effect operation",
        arguments,
        &signature,
        available,
        types,
    )?;
    verify_effect_argument_ownership(
        function,
        arguments,
        &operation_info.parameters,
        &signature.parameter_ownership,
        types,
        "resumable effect operation",
    )?;
    check_destination_type(function, destination, signature.return_type)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Suspend verification needs the statement's operands plus block-local availability and type context."
)]
pub(super) fn verify_suspend_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    block_id: MirBlockId,
    destination: MirValueId,
    operation: crate::sema::EffectOperationId,
    continuation: MirContinuationId,
    arguments: &[MirCallArgument],
) -> Result<(), Diagnostic> {
    let signature = verify_request_protocol(
        function,
        types,
        operation,
        Some((continuation, MirContinuationKind::Suspending)),
        destination,
        Some(block_id),
        "Suspend",
    )?;
    let operation_info = types
        .effects()
        .operation_info(operation)
        .expect("validated operation");
    validate_call_arguments(
        function,
        "suspending effect operation",
        arguments,
        &signature,
        available,
        types,
    )?;
    verify_effect_argument_ownership(
        function,
        arguments,
        &operation_info.parameters,
        &signature.parameter_ownership,
        types,
        "suspending effect operation",
    )?;
    check_destination_type(function, destination, signature.return_type)
}

pub(super) fn verify_resume_statement(
    function: &MirFunction,
    block_id: MirBlockId,
    continuation: MirContinuationId,
) -> Result<(), Diagnostic> {
    let metadata = function
        .continuations
        .iter()
        .find(|item| item.id == continuation)
        .ok_or_else(|| Diagnostic::codegen("MIR Resume references an unknown continuation"))?;
    if metadata.resume_block != block_id {
        return Err(Diagnostic::codegen(
            "MIR Resume is not in its continuation resume block",
        ));
    }
    Ok(())
}
