use std::collections::HashMap;

use crate::diagnostic::CodegenError;
use crate::mir::{MirConstant, MirFunction, MirFunctionId, MirStatement};

use super::super::environment::FunctionType;
use super::super::helpers::method_key;

pub(in crate::codegen) fn collect_mir_constant_strings<'a>(
    constant: &'a MirConstant,
    output: &mut Vec<&'a str>,
) {
    match constant {
        MirConstant::String(value) => output.push(value.as_str()),
        MirConstant::Option(Some(value)) | MirConstant::Result { value, .. } => {
            collect_mir_constant_strings(value, output)
        }
        MirConstant::Tuple(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_strings(value, output)),
        MirConstant::Struct(values) => values
            .iter()
            .for_each(|(_, value)| collect_mir_constant_strings(value, output)),
        MirConstant::Class(values) => values
            .iter()
            .for_each(|(_, value)| collect_mir_constant_strings(value, output)),
        MirConstant::List(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_strings(value, output)),
        MirConstant::Map(entries) => entries.iter().for_each(|(key, value)| {
            collect_mir_constant_strings(key, output);
            collect_mir_constant_strings(value, output);
        }),
        MirConstant::MutList(values) | MirConstant::MutSet(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_strings(value, output)),
        MirConstant::MutMap(entries) => entries.iter().for_each(|(key, value)| {
            collect_mir_constant_strings(key, output);
            collect_mir_constant_strings(value, output);
        }),
        MirConstant::Enum { fields, .. } => fields
            .iter()
            .for_each(|value| collect_mir_constant_strings(value, output)),
        MirConstant::Option(None)
        | MirConstant::Integer(_)
        | MirConstant::EffectOperation(_)
        | MirConstant::Float(_)
        | MirConstant::Boolean(_) => {}
    }
}

pub(super) fn collect_mir_constant_owned_strings(constant: &MirConstant, output: &mut Vec<String>) {
    match constant {
        MirConstant::String(value) => output.push(value.clone()),
        MirConstant::Option(Some(value)) | MirConstant::Result { value, .. } => {
            collect_mir_constant_owned_strings(value, output)
        }
        MirConstant::Tuple(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_owned_strings(value, output)),
        MirConstant::Struct(values) => values
            .iter()
            .for_each(|(_, value)| collect_mir_constant_owned_strings(value, output)),
        MirConstant::Class(values) => values
            .iter()
            .for_each(|(_, value)| collect_mir_constant_owned_strings(value, output)),
        MirConstant::List(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_owned_strings(value, output)),
        MirConstant::Map(entries) => entries.iter().for_each(|(key, value)| {
            collect_mir_constant_owned_strings(key, output);
            collect_mir_constant_owned_strings(value, output);
        }),
        MirConstant::MutList(values) | MirConstant::MutSet(values) => values
            .iter()
            .for_each(|value| collect_mir_constant_owned_strings(value, output)),
        MirConstant::MutMap(entries) => entries.iter().for_each(|(key, value)| {
            collect_mir_constant_owned_strings(key, output);
            collect_mir_constant_owned_strings(value, output);
        }),
        MirConstant::Enum { fields, .. } => fields
            .iter()
            .for_each(|value| collect_mir_constant_owned_strings(value, output)),
        MirConstant::Option(None)
        | MirConstant::Integer(_)
        | MirConstant::EffectOperation(_)
        | MirConstant::Float(_)
        | MirConstant::Boolean(_) => {}
    }
}

pub(super) fn collect_tagged_continuation_cleanup_types(
    ty: crate::sema::Type,
    types: &crate::sema::TypeTable,
    collected: &mut std::collections::HashSet<crate::sema::Type>,
) {
    match ty {
        crate::sema::Type::Tuple(id) => {
            for element in types.tuple_elements(id) {
                collect_tagged_continuation_cleanup_types(*element, types, collected);
            }
        }
        crate::sema::Type::Struct(id) => {
            for (_, field) in types.struct_fields(id) {
                collect_tagged_continuation_cleanup_types(*field, types, collected);
            }
        }
        crate::sema::Type::Enum(_)
        | crate::sema::Type::Option(_)
        | crate::sema::Type::Result(_)
            if types.needs_drop(ty) =>
        {
            collected.insert(ty);
        }
        _ => {}
    }
}
pub(super) fn mir_function_symbol(function: &MirFunction) -> String {
    function
        .receiver
        .map(|receiver| method_key(receiver, &function.name))
        .unwrap_or_else(|| function.name.clone())
}

pub(super) fn mir_function_type(function: &MirFunction) -> FunctionType {
    FunctionType {
        parameters: function
            .parameters
            .iter()
            .map(|parameter| parameter.ty)
            .collect(),
        return_type: function.return_type,
        receiver: function.receiver,
        is_suspending: function.is_suspending,
        pending_abi: false,
        foreign: function.foreign.is_some(),
    }
}

pub(super) fn is_main_mir(function: &MirFunction) -> bool {
    function.receiver.is_none() && function.name == "main"
}

pub(super) fn closure_capture_types(
    functions: &[MirFunction],
) -> Result<HashMap<MirFunctionId, Vec<crate::sema::Type>>, CodegenError> {
    let mut captures = HashMap::new();
    for function in functions {
        for block in &function.blocks {
            for statement in &block.statements {
                if let MirStatement::DynamicValue { methods, value, .. } = statement {
                    for method in methods {
                        captures.insert(*method, vec![function.value_types[value.0]]);
                    }
                    continue;
                }
                let MirStatement::FunctionValue {
                    function: target,
                    captures: values,
                    ..
                } = statement
                else {
                    continue;
                };
                let types = values
                    .iter()
                    .map(|value| function.value_types[value.0])
                    .collect::<Vec<_>>();
                if let Some(previous) = captures.insert(*target, types.clone()) {
                    if previous != types {
                        return Err(CodegenError::RuntimeError {
                            message: "closure target has inconsistent capture types".to_owned(),
                        });
                    }
                }
            }
        }
    }
    Ok(captures)
}
