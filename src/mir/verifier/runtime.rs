//! Validation of runtime intrinsic MIR statements.

use super::*;

pub(super) fn verify_runtime_call(
    function: &MirFunction,
    types: &TypeTable,
    available: &IndexSet<MirValueId>,
    destination: MirValueId,
    arguments: &[MirCallArgument],
    intrinsic: &RuntimeIntrinsic,
) -> Result<(), Diagnostic> {
    let argument_types = arguments
        .iter()
        .map(|argument| {
            check_definition(available, argument.value, function)?;
            check_value_exists(function, argument.value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let spec = intrinsic.spec(types, &argument_types)?;
    debug_assert_eq!(spec.symbol, intrinsic.runtime_symbol());
    if spec.variadic.is_none() && argument_types.len() != spec.parameters.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR {} requires {} argument(s) in function '{}'",
            spec.label,
            spec.parameters.len(),
            function.name
        )));
    }
    if spec.variadic.is_some() && argument_types.len() < spec.parameters.len() {
        return Err(Diagnostic::codegen(format!(
            "MIR {} is missing required arguments in function '{}'",
            spec.label, function.name
        )));
    }
    for (index, (argument, expected)) in arguments.iter().zip(&spec.parameters).enumerate() {
        if argument.parameter != index {
            return Err(Diagnostic::codegen(format!(
                "MIR {} has an out-of-order argument in function '{}'",
                spec.label, function.name
            )));
        }
        let actual = argument_types[index];
        if !expected.matches(actual, types) {
            return Err(Diagnostic::codegen(format!(
                "MIR {} argument type mismatch in function '{}'",
                spec.label, function.name
            )));
        }
        if let Some(ownership) = spec
            .arg_ownership
            .get(index)
            .copied()
            .unwrap_or(crate::mir::IntrinsicArgOwnership::Any)
            .expected(actual, types)
        {
            if function.value_ownership[argument.value.0] != ownership {
                return Err(Diagnostic::codegen(format!(
                    "MIR {} argument ownership mismatch in function '{}'",
                    spec.label, function.name
                )));
            }
        }
    }
    if let Some(variadic) = spec.variadic {
        for (offset, argument) in arguments.iter().skip(spec.parameters.len()).enumerate() {
            let index = spec.parameters.len() + offset;
            if argument.parameter != index {
                return Err(Diagnostic::codegen(format!(
                    "MIR {} has an out-of-order argument in function '{}'",
                    spec.label, function.name
                )));
            }
            if !variadic.matches(argument_types[index], types) {
                return Err(Diagnostic::codegen(format!(
                    "MIR {} argument type mismatch in function '{}'",
                    spec.label, function.name
                )));
            }
        }
    }
    check_destination_type(function, destination, spec.result)
}
