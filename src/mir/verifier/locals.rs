use super::*;

pub(super) fn verify_read_statement(
    function: &MirFunction,
    destination: MirValueId,
    local_id: MirLocalId,
) -> Result<(), Diagnostic> {
    let local = function.locals.get(local_id.0).ok_or_else(|| {
        Diagnostic::codegen(format!(
            "MIR function '{}' reads invalid local slot {}",
            function.name, local_id.0
        ))
    })?;
    // A class handler capture is a borrowed pointer view of the lexical owner.
    // It deliberately uses Read rather than TakeLocal; the handler environment
    // has no drop glue for borrowed slots.
    let class_borrow = matches!(local.ty, Type::Class(_))
        && function.value_ownership.get(destination.0) == Some(&MirOwnership::Borrowed);
    if local.ownership == MirOwnership::Owned && !class_borrow {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' copies owned local '{}' with Read",
            function.name, local.name
        )));
    }
    check_destination_type(function, destination, local.ty)
}

pub(super) fn verify_borrow_local_statement(
    function: &MirFunction,
    types: &TypeTable,
    destination: MirValueId,
    local_id: MirLocalId,
) -> Result<(), Diagnostic> {
    let local = function.locals.get(local_id.0).ok_or_else(|| {
        Diagnostic::codegen(format!(
            "MIR function '{}' borrows an invalid local",
            function.name
        ))
    })?;
    if !types.is_owned(local.ty)
        || !matches!(
            local.ownership,
            MirOwnership::Owned | MirOwnership::Borrowed
        )
    {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' borrows a non-owned local",
            function.name
        )));
    }
    check_destination_type(function, destination, local.ty)?;
    if function.value_ownership[destination.0] != MirOwnership::Borrowed {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' gives a local borrow invalid ownership",
            function.name
        )));
    }
    Ok(())
}

pub(super) fn verify_take_local_statement(
    function: &MirFunction,
    destination: MirValueId,
    local_id: MirLocalId,
) -> Result<(), Diagnostic> {
    let local = function.locals.get(local_id.0).ok_or_else(|| {
        Diagnostic::codegen(format!(
            "MIR function '{}' takes an invalid local",
            function.name
        ))
    })?;
    if local.ownership == MirOwnership::Borrowed {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' takes non-owned local '{}'",
            function.name, local.name
        )));
    }
    check_destination_type(function, destination, local.ty)?;
    if function.value_ownership[destination.0] != local.ownership {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' gives a taken local invalid ownership",
            function.name
        )));
    }
    Ok(())
}
