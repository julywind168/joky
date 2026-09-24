use super::*;

/// Verify MIR statements that move, drop or bind owned values.
pub(super) fn verify_ownership_statement(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    statement: &MirStatement,
) -> Result<(), Diagnostic> {
    match statement {
        MirStatement::Dup { destination, value } => {
            check_definition(available, *value, function)?;
            let source_type = check_value_exists(function, *value)?;
            if ownership_for_type(source_type, types) != MirOwnership::Shared {
                return Err(Diagnostic::codegen(format!(
                    "MIR dup requires a shared value in function '{}'",
                    function.name
                )));
            }
            check_destination_type(function, *destination, source_type)?;
            if function.value_ownership[destination.0] != MirOwnership::Shared {
                return Err(Diagnostic::codegen(format!(
                    "MIR dup destination is not shared in function '{}'",
                    function.name
                )));
            }
        }
        MirStatement::Move { destination, value } => {
            check_definition(available, *value, function)?;
            let source_type = check_value_exists(function, *value)?;
            if ownership_for_type(source_type, types) == MirOwnership::Copy {
                return Err(Diagnostic::codegen(format!(
                    "MIR move requires a non-copy value in function '{}'",
                    function.name
                )));
            }
            check_destination_type(function, *destination, source_type)?;
        }
        MirStatement::Drop { destination, value } => {
            check_definition(available, *value, function)?;
            let source_type = check_value_exists(function, *value)?;
            if ownership_for_type(source_type, types) == MirOwnership::Copy {
                return Err(Diagnostic::codegen(format!(
                    "MIR drop requires a non-copy value in function '{}'",
                    function.name
                )));
            }
            check_destination_type(function, *destination, Type::Unit)?;
        }
        MirStatement::Deinit {
            destination,
            value,
            variant,
        } => {
            check_definition(available, *value, function)?;
            let source_type = check_value_exists(function, *value)?;
            if !types.is_owned(source_type)
                || !matches!(
                    source_type,
                    Type::Tuple(_)
                        | Type::Struct(_)
                        | Type::Enum(_)
                        | Type::Option(_)
                        | Type::Result(_)
                )
                || function.value_ownership[value.0] != MirOwnership::Owned
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR deinit requires an owned value aggregate in function '{}'",
                    function.name
                )));
            }
            match (source_type, variant) {
                (Type::Tuple(_) | Type::Struct(_), None) => {}
                (Type::Enum(enum_id), Some(variant))
                    if *variant < types.enum_variants(enum_id).len() => {}
                (Type::Enum(_), Some(variant)) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' deinitializes unknown enum variant {}",
                        function.name, variant
                    )));
                }
                (Type::Enum(_), None) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' deinitializes an enum without a variant",
                        function.name
                    )));
                }
                (Type::Tuple(_) | Type::Struct(_), Some(_)) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' attaches an enum variant to a value aggregate deinit",
                        function.name
                    )));
                }
                (Type::Option(_) | Type::Result(_), Some(variant)) if *variant <= 1 => {}
                (Type::Option(_) | Type::Result(_), Some(variant)) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' deinitializes unknown wrapper variant {}",
                        function.name, variant
                    )));
                }
                (Type::Option(_) | Type::Result(_), None) => {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' deinitializes a wrapper without a variant",
                        function.name
                    )));
                }
                _ => unreachable!("aggregate deinit type checked above"),
            }
            check_destination_type(function, *destination, Type::Unit)?;
        }
        MirStatement::DropLocal { destination, local } => {
            let local = function.locals.get(local.0).ok_or_else(|| {
                Diagnostic::codegen(format!(
                    "MIR function '{}' drops an invalid local",
                    function.name
                ))
            })?;
            if !matches!(local.ownership, MirOwnership::Owned | MirOwnership::Shared) {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' drops non-owned local '{}'",
                    function.name, local.name
                )));
            }
            check_destination_type(function, *destination, Type::Unit)?;
        }
        MirStatement::Bind {
            destination,
            value,
            local,
        } => {
            check_destination_type(function, *destination, Type::Unit)?;
            let local_type = function
                .locals
                .get(local.0)
                .map(|local| local.ty)
                .ok_or_else(|| {
                    Diagnostic::codegen(format!(
                        "MIR function '{}' binds invalid local slot {}",
                        function.name, local.0
                    ))
                })?;
            if let Some(value) = value {
                check_definition(available, *value, function)?;
                check_value_type(function, *value, local_type)?;
                if matches!(
                    function.locals[local.0].ownership,
                    MirOwnership::Owned | MirOwnership::Shared
                ) && function.value_ownership[value.0] != function.locals[local.0].ownership
                {
                    return Err(Diagnostic::codegen(format!(
                        "MIR function '{}' binds a borrowed value into owned local '{}'",
                        function.name, function.locals[local.0].name
                    )));
                }
            }
        }
        _ => {
            return Err(Diagnostic::codegen(
                "non-ownership statement passed to ownership verifier",
            ));
        }
    }
    Ok(())
}
