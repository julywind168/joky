use super::*;

pub(super) fn verify_terminator(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    terminator: &MirTerminator,
) -> Result<(), Diagnostic> {
    match terminator {
        MirTerminator::Branch { condition, .. } => {
            check_definition(available, *condition, function)?;
            check_value_type(function, *condition, Type::Bool)?;
        }
        MirTerminator::Goto { arguments, .. } => {
            for argument in arguments {
                check_definition(available, *argument, function)?;
            }
        }
        MirTerminator::Return(Some(value)) => {
            check_definition(available, *value, function)?;
            check_value_type(function, *value, function.return_type)?;
            if ownership_for_type(function.return_type, types) == MirOwnership::Owned
                && function.value_ownership[value.0] != MirOwnership::Owned
            {
                return Err(Diagnostic::codegen(format!(
                    "MIR function '{}' returns a borrowed owned value",
                    function.name
                )));
            }
        }
        MirTerminator::Return(None) if function.return_type == Type::Unit => {}
        MirTerminator::Return(None) => {
            return Err(Diagnostic::codegen(format!(
                "MIR function '{}' returns Unit from a non-Unit function",
                function.name
            )));
        }
        MirTerminator::Unreachable => {}
    }
    Ok(())
}
