use super::*;

/// Validate function-level metadata before block and statement verification.
pub(super) fn verify_function_shape(
    function: &MirFunction,
    types: &TypeTable,
) -> Result<(), Diagnostic> {
    if function
        .parameters
        .iter()
        .map(|p| p.ty)
        .chain(std::iter::once(function.return_type))
        .chain(function.value_types.iter().copied())
        .any(|ty| types.has_inline_c_array(ty))
    {
        return Err(Diagnostic::codegen(
            "CArray is a native layout type; use CPtr or CMutPtr for Joky values",
        ));
    }
    if function.blocks.is_empty() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has no basic blocks",
            function.name
        )));
    }
    if function.value_types.len() != function.value_ownership.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR function '{}' has mismatched value type and ownership metadata",
            function.name
        )));
    }
    for parameter in &function.parameters {
        let expected = function
            .locals
            .get(parameter.local.0)
            .map(|local| local.ownership)
            .filter(|ownership| matches!(ownership, MirOwnership::Borrowed))
            .unwrap_or_else(|| {
                if function.foreign.is_some() && matches!(parameter.ty, Type::Function(_)) {
                    // A callback parameter is a single trampoline-address word.
                    MirOwnership::Copy
                } else {
                    ownership_for_type(parameter.ty, types)
                }
            });
        if parameter.ownership != expected {
            return Err(Diagnostic::codegen(format!(
                "MIR parameter '{}' has invalid ownership metadata",
                parameter.name
            )));
        }
    }
    for local in &function.locals {
        let expected = if local.ownership == MirOwnership::Borrowed {
            // Lease bindings are borrowed views of an owned Cown payload and
            // intentionally do not participate in local drop tracking.
            MirOwnership::Borrowed
        } else if function.parameters.iter().any(|parameter| {
            parameter.local == local.id && parameter.ownership == MirOwnership::Borrowed
        }) {
            MirOwnership::Borrowed
        } else if function
            .parameters
            .iter()
            .any(|parameter| parameter.local == local.id)
            && function.foreign.is_some()
            && matches!(local.ty, Type::Function(_))
        {
            MirOwnership::Copy
        } else {
            ownership_for_type(local.ty, types)
        };
        if local.ownership != expected {
            return Err(Diagnostic::codegen(format!(
                "MIR local '{}' has invalid ownership metadata",
                local.name
            )));
        }
    }
    for (index, block) in function.blocks.iter().enumerate() {
        if block.id != MirBlockId(index) {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' has a block with an invalid id",
                function.name
            )));
        }
    }
    Ok(())
}
