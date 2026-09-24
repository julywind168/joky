//! MIR value and aggregate compilation helpers.

mod closures;
mod collections;
mod constants;

use std::collections::HashMap;

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, Signature};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirConstant, MirFieldAccess};
use crate::sema::{Type, TypeTable};

use super::super::abi::{abi_types, result_from_params, value_arguments};
use super::super::classes::{
    emit_drop_value, load_field as load_class_field, size as class_size,
    store_field as store_class_field,
};
use super::super::constructors::{compile_mir_constructor_values, zero_compiled_value};
use super::super::environment::{CompiledValue, Environment};

pub(super) use closures::compile_closure_environment;
pub(super) use closures::compile_dynamic_environment;
pub(in crate::codegen) use closures::{define_closure_call_glue, define_closure_drop_glue};
pub(super) use collections::{list_element_words, list_value_from_words, store_constant_words};
pub(super) use constants::compile_mir_constant;

pub(super) fn compile_mir_read(
    _builder: &mut FunctionBuilder<'_>,
    local: crate::mir::MirLocalId,
    _pointer_type: cranelift_codegen::ir::Type,
    environment: &Environment,
    _types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    if let Some(value) = environment.lookup_local(local) {
        return Ok(value);
    }
    Err(CodegenError::RuntimeError {
        message: format!("local %{} was not resolved", local.0),
    })
}

/// Resolve a local while compiling an independent continuation entry.  This
/// deliberately shares only the restored continuation environment; callers do
/// not provide a native function environment as a fallback.
pub(super) fn compile_mir_read_stackless(
    _builder: &mut FunctionBuilder<'_>,
    local: crate::mir::MirLocalId,
    _pointer_type: cranelift_codegen::ir::Type,
    environment: &Environment,
    _types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    if let Some(value) = environment.lookup_local_stackless(local) {
        return Ok(value);
    }
    Err(CodegenError::RuntimeError {
        message: format!(
            "stackless machine entry cannot resolve native-only local l{}",
            local.0
        ),
    })
}

pub(super) fn compile_mir_call(
    builder: &mut FunctionBuilder<'_>,
    function_id: &crate::mir::MirFunctionId,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    environment: &Environment,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    compile_mir_call_abi(
        builder,
        function_id,
        arguments,
        values,
        environment,
        types,
        None,
        None,
    )
    .map(|(_, value)| value)
}

#[expect(
    clippy::too_many_arguments,
    reason = "The shared call ABI explicitly distinguishes arguments, optional receiver and parent continuation."
)]
pub(super) fn compile_mir_call_abi(
    builder: &mut FunctionBuilder<'_>,
    function_id: &crate::mir::MirFunctionId,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    environment: &Environment,
    types: &TypeTable,
    parent: Option<cranelift_codegen::ir::Value>,
    receiver: Option<CompiledValue>,
) -> Result<(Option<cranelift_codegen::ir::Value>, CompiledValue), CodegenError> {
    let function =
        *environment
            .function_refs
            .get(function_id)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!(
                    "function id {} has no codegen implementation",
                    function_id.0
                ),
            })?;
    let function_type =
        environment
            .function_types
            .get(function_id)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "function signature was not resolved".to_owned(),
            })?;
    let mut ordered = vec![None; function_type.parameters.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(CodegenError::RuntimeError {
                message: "argument index out of bounds".to_owned(),
            });
        }
        if ordered[index].is_some() {
            return Err(CodegenError::RuntimeError {
                message: "duplicate call argument".to_owned(),
            });
        }
        ordered[index] = Some(values.get(&argument.value).cloned().ok_or_else(|| {
            CodegenError::RuntimeError {
                message: "MIR call argument was not compiled".to_owned(),
            }
        })?);
    }
    let ordered = ordered
        .into_iter()
        .map(|value| {
            value.ok_or_else(|| CodegenError::RuntimeError {
                message: "missing call argument".to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut call_arguments = receiver
        .into_iter()
        .chain(ordered)
        .flat_map(value_arguments)
        .collect::<Vec<_>>();
    if function_type.pending_abi != parent.is_some() {
        return Err(CodegenError::RuntimeError {
            message: "function call does not match its Pending ABI".to_owned(),
        });
    }
    if let Some(parent) = parent {
        call_arguments.push(parent);
    }
    let call = builder.ins().call(function.reference, &call_arguments);
    let mut results = builder.inst_results(call).to_vec();
    let status = parent.map(|_| results.remove(0));
    let result =
        result_from_params(results, function_type.return_type, types).map_err(|error| {
            CodegenError::RuntimeError {
                message: error.to_string(),
            }
        })?;
    Ok((status, result))
}

pub(super) fn compile_dynamic_upcast(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    target: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let (
        CompiledValue::Function {
            code,
            environment,
            ty: Type::Dyn(source),
        },
        Type::Dyn(target_id),
    ) = (value, target)
    else {
        return Err(CodegenError::RuntimeError {
            message: "invalid dynamic upcast value".into(),
        });
    };
    let slots = types.dynamic_types[source]
        .upcast_slots(&types.dynamic_types[target_id], |id| {
            types.function_type(id)
        })
        .map_err(|message| CodegenError::RuntimeError { message })?;
    // Vtables are private to uniquely owned allocations. Compact their entries,
    // preserving the original payload offset, call glue and drop callback.
    // Load all entries before writing so successive upcasts cannot clobber them.
    let entries = slots
        .iter()
        .map(|slot| {
            builder.ins().load(
                pointer_type,
                cranelift_codegen::ir::MemFlagsData::new(),
                code,
                (*slot * pointer_type.bytes() as usize) as i32,
            )
        })
        .collect::<Vec<_>>();
    for (slot, entry) in entries.into_iter().enumerate() {
        builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            entry,
            code,
            (slot * pointer_type.bytes() as usize) as i32,
        );
    }
    Ok(CompiledValue::Function {
        code,
        environment,
        ty: target,
    })
}

pub(super) fn compile_mir_project(
    builder: &mut FunctionBuilder<'_>,
    base: CompiledValue,
    access: &MirFieldAccess,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    match (base, access) {
        (
            CompiledValue::Function {
                code: vtable,
                environment,
                ty: Type::Dyn(id),
            },
            MirFieldAccess::Index(slot),
        ) => {
            let signature = types.dynamic_types[id].methods[*slot].signature;
            let code = builder.ins().load(
                pointer_type,
                cranelift_codegen::ir::MemFlagsData::new(),
                vtable,
                (*slot * pointer_type.bytes() as usize) as i32,
            );
            Ok(CompiledValue::Function {
                code,
                environment,
                ty: Type::Function(signature),
            })
        }
        (CompiledValue::Tuple { elements, .. }, MirFieldAccess::Index(index)) => elements
            .get(*index)
            .cloned()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "tuple field index is out of bounds".to_owned(),
            }),
        (
            CompiledValue::Struct {
                fields,
                ty: crate::sema::Type::Struct(_id),
            },
            MirFieldAccess::Index(index),
        ) => Ok(fields[*index].clone()),
        (
            CompiledValue::Class {
                pointer,
                ty: crate::sema::Type::Class(id),
            },
            MirFieldAccess::Index(index),
        ) => {
            let field_type = types.class_fields(id)[*index].1;
            load_class_field(
                builder,
                pointer,
                id,
                *index,
                field_type,
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })
        }
        _ => Err(CodegenError::RuntimeError {
            message: "invalid MIR field projection".to_owned(),
        }),
    }
}

pub(super) fn compile_mir_enum_project(
    value: CompiledValue,
    variant: usize,
    field: usize,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    if let CompiledValue::Enum {
        fields,
        ty: crate::sema::Type::Option(_),
        ..
    } = &value
    {
        if variant == 0 && field == 0 {
            return fields
                .first()
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Option payload is missing".to_owned(),
                });
        }
        return Err(CodegenError::RuntimeError {
            message: "Option projection is out of bounds".to_owned(),
        });
    }
    if let CompiledValue::Enum {
        fields,
        ty: crate::sema::Type::Result(_),
        ..
    } = &value
    {
        if field == 0 && variant < 2 {
            return fields
                .get(variant)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Result payload is missing".to_owned(),
                });
        }
        return Err(CodegenError::RuntimeError {
            message: "Result projection is out of bounds".to_owned(),
        });
    }
    let CompiledValue::Enum {
        fields,
        ty: crate::sema::Type::Enum(enum_id),
        ..
    } = value
    else {
        return Err(CodegenError::RuntimeError {
            message: "MIR enum projection reads a non-enum value".to_owned(),
        });
    };
    let variants = types.enum_variants(enum_id);
    let layout = variants
        .get(variant)
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "MIR enum projection variant is out of bounds".to_owned(),
        })?;
    if field >= layout.fields.len() {
        return Err(CodegenError::RuntimeError {
            message: "MIR enum projection field is out of bounds".to_owned(),
        });
    }
    let offset = variants[..variant]
        .iter()
        .map(|candidate| candidate.fields.len())
        .sum::<usize>();
    fields
        .get(offset + field)
        .cloned()
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "MIR enum projection payload is out of bounds".to_owned(),
        })
}

/// Report a `main -> Result(Unit, String)` failure while keeping the machine
/// entry ABI void. The runtime records the failure for the host after all
/// pending work has drained.
pub(super) fn emit_main_result(
    builder: &mut FunctionBuilder<'_>,
    value: &CompiledValue,
    report_ref: cranelift_codegen::ir::FuncRef,
) -> Result<(), CodegenError> {
    let CompiledValue::Enum { tag, fields, .. } = value else {
        return Err(CodegenError::RuntimeError {
            message: "main Result value has invalid codegen shape".to_owned(),
        });
    };
    let Some(CompiledValue::String { pointer, length }) = fields.get(1) else {
        return Err(CodegenError::RuntimeError {
            message: "main Result error payload is not a String".to_owned(),
        });
    };
    builder.ins().call(report_ref, &[*tag, *pointer, *length]);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_mir_construct(
    builder: &mut FunctionBuilder<'_>,
    type_id: crate::mir::MirTypeId,
    fields: &[crate::mir::MirValueId],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    destination_type: crate::sema::Type,
    allocate_ref: cranelift_codegen::ir::FuncRef,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    match destination_type {
        crate::sema::Type::Struct(id) => {
            if type_id != crate::mir::MirTypeId::Struct(id) {
                return Err(CodegenError::RuntimeError {
                    message: "MIR struct type id does not match destination".to_owned(),
                });
            }
            let field_values = compile_mir_constructor_values(fields, values).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            Ok(CompiledValue::Struct {
                fields: field_values,
                ty: destination_type,
            })
        }
        crate::sema::Type::Class(id) => {
            if type_id != crate::mir::MirTypeId::Class(id) {
                return Err(CodegenError::RuntimeError {
                    message: "MIR class type id does not match destination".to_owned(),
                });
            }
            let size = class_size(id, pointer_type, types).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            let kind = builder.ins().iconst(types::I8, 2);
            let size_value = builder.ins().iconst(pointer_type, i64::from(size));
            let align = builder.ins().iconst(pointer_type, 8);
            let pointer = builder.ins().call(allocate_ref, &[kind, size_value, align]);
            let pointer = builder.inst_results(pointer)[0];
            let declared = types.class_fields(id);
            let field_values = compile_mir_constructor_values(fields, values).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            for (index, (_, field_type)) in declared.iter().enumerate() {
                store_class_field(
                    builder,
                    pointer,
                    id,
                    index,
                    field_values[index].clone(),
                    *field_type,
                    pointer_type,
                    types,
                )
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
            }
            Ok(CompiledValue::Class {
                pointer,
                ty: destination_type,
            })
        }
        _ => Err(CodegenError::RuntimeError {
            message: "MIR type id is not a constructible struct/class".to_owned(),
        }),
    }
}

pub(super) fn compile_mir_store(
    builder: &mut FunctionBuilder<'_>,
    receiver: CompiledValue,
    access: &MirFieldAccess,
    value: CompiledValue,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
    environment: &Environment,
) -> Result<(), CodegenError> {
    let (
        CompiledValue::Class {
            pointer,
            ty: crate::sema::Type::Class(id),
        },
        MirFieldAccess::Index(field_index),
    ) = (receiver, access)
    else {
        return Err(CodegenError::RuntimeError {
            message: "invalid MIR class field store".to_owned(),
        });
    };
    let field_type = types.class_fields(id)[*field_index].1;
    if types.needs_drop(field_type) {
        let old_value = load_class_field(
            builder,
            pointer,
            id,
            *field_index,
            field_type,
            pointer_type,
            types,
        )
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
        compile_drop_value(builder, old_value, environment, pointer_type, types)?;
    }
    store_class_field(
        builder,
        pointer,
        id,
        *field_index,
        value,
        field_type,
        pointer_type,
        types,
    )
    .map_err(|error| CodegenError::RuntimeError {
        message: error.to_string(),
    })
}

pub(super) fn compile_drop_value(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    environment: &Environment,
    _pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(), CodegenError> {
    emit_drop_value(
        builder,
        value,
        &environment.class_drop_refs,
        environment
            .managed_drop_ref
            .expect("managed drop reference initialized"),
        types,
    )
}

pub(super) fn compile_mir_indirect_call(
    builder: &mut FunctionBuilder<'_>,
    callee: CompiledValue,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    compile_mir_indirect_call_abi(
        builder,
        callee,
        arguments,
        values,
        pointer_type,
        types,
        None,
    )
    .map(|(_, value)| value)
}

pub(super) fn compile_mir_indirect_call_abi(
    builder: &mut FunctionBuilder<'_>,
    callee: CompiledValue,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
    parent: Option<cranelift_codegen::ir::Value>,
) -> Result<(Option<cranelift_codegen::ir::Value>, CompiledValue), CodegenError> {
    let CompiledValue::Function {
        code,
        ty: Type::Function(signature_id),
        environment: closure_environment,
    } = callee
    else {
        return Err(CodegenError::RuntimeError {
            message: "MIR indirect callee is not a function".to_owned(),
        });
    };
    let function_signature = types.function_type(signature_id);
    let mut ordered = vec![None; function_signature.parameters.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(CodegenError::RuntimeError {
                message: "indirect argument index out of bounds".to_owned(),
            });
        }
        if ordered[index].is_some() {
            return Err(CodegenError::RuntimeError {
                message: "duplicate indirect call argument".to_owned(),
            });
        }
        ordered[index] = Some(values.get(&argument.value).cloned().ok_or_else(|| {
            CodegenError::RuntimeError {
                message: "MIR indirect argument was not compiled".to_owned(),
            }
        })?);
    }
    let ordered = ordered
        .into_iter()
        .map(|value| {
            value.ok_or_else(|| CodegenError::RuntimeError {
                message: "missing indirect call argument".to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut call_args = std::iter::once(closure_environment)
        .chain(ordered.into_iter().flat_map(value_arguments))
        .collect::<Vec<_>>();
    if let Some(parent) = parent {
        call_args.push(parent);
    }
    let mut return_types = abi_types(function_signature.return_type, pointer_type, types)
        .into_iter()
        .map(AbiParam::new)
        .collect::<Vec<_>>();
    if parent.is_some() {
        return_types.insert(0, AbiParam::new(types::I8));
    }
    let mut signature_params = std::iter::once(AbiParam::new(pointer_type))
        .chain(
            function_signature
                .parameters
                .iter()
                .flat_map(|ty| abi_types(*ty, pointer_type, types))
                .map(AbiParam::new),
        )
        .collect::<Vec<_>>();
    if parent.is_some() {
        signature_params.push(AbiParam::new(pointer_type));
    }
    let signature = Signature {
        params: signature_params,
        returns: return_types,
        call_conv: builder.func.signature.call_conv,
    };
    let signature_ref = builder.func.import_signature(signature);
    let call = builder.ins().call_indirect(signature_ref, code, &call_args);
    let mut results = builder.inst_results(call).to_vec();
    let status = parent.map(|_| results.remove(0));
    result_from_params(results, function_signature.return_type, types)
        .map(|value| (status, value))
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_mir_method_call(
    builder: &mut FunctionBuilder<'_>,
    receiver: CompiledValue,
    _receiver_type: crate::sema::Type,
    method_id: crate::mir::MirFunctionId,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    environment: &Environment,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let function =
        *environment
            .function_refs
            .get(&method_id)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!("method id {} has no codegen implementation", method_id.0),
            })?;
    let function_type =
        environment
            .function_types
            .get(&method_id)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "method signature was not resolved".to_owned(),
            })?;
    let mut ordered = vec![None; function_type.parameters.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(CodegenError::RuntimeError {
                message: "argument index out of bounds".to_owned(),
            });
        }
        if ordered[index].is_some() {
            return Err(CodegenError::RuntimeError {
                message: "duplicate call argument".to_owned(),
            });
        }
        ordered[index] = Some(values.get(&argument.value).cloned().ok_or_else(|| {
            CodegenError::RuntimeError {
                message: "MIR method argument was not compiled".to_owned(),
            }
        })?);
    }
    let arguments = ordered
        .into_iter()
        .map(|value| {
            value.ok_or_else(|| CodegenError::RuntimeError {
                message: "missing call argument".to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut call_arguments = value_arguments(receiver);
    call_arguments.extend(arguments.into_iter().flat_map(value_arguments));
    let call = builder.ins().call(function.reference, &call_arguments);
    result_from_params(
        builder.inst_results(call).to_vec(),
        function_type.return_type,
        types,
    )
    .map_err(|error| CodegenError::RuntimeError {
        message: error.to_string(),
    })
}

pub(super) fn compile_mir_enum_construct(
    builder: &mut FunctionBuilder<'_>,
    enum_id: crate::mir::MirTypeId,
    variant_index: usize,
    arguments: &[MirCallArgument],
    values: &HashMap<crate::mir::MirValueId, CompiledValue>,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    if let crate::mir::MirTypeId::Option(option_id) = enum_id {
        if variant_index > 1 || arguments.len() != usize::from(variant_index == 0) {
            return Err(CodegenError::RuntimeError {
                message: "invalid Option constructor arguments".to_owned(),
            });
        }
        let inner = types.option_type(option_id);
        let value = if variant_index == 0 {
            values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Option value was not compiled".to_owned(),
                })?
        } else {
            zero_compiled_value(builder, inner, pointer_type, types).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?
        };
        return Ok(CompiledValue::Enum {
            tag: builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I32, variant_index as i64),
            fields: vec![value],
            ty: crate::sema::Type::Option(option_id),
        });
    }
    if let crate::mir::MirTypeId::Result(result_id) = enum_id {
        if variant_index > 1 || arguments.len() != 1 || arguments[0].parameter != 0 {
            return Err(CodegenError::RuntimeError {
                message: "invalid Result constructor arguments".to_owned(),
            });
        }
        let (ok, err) = types.result_types(result_id);
        let payload =
            values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Result value was not compiled".to_owned(),
                })?;
        let inactive = zero_compiled_value(
            builder,
            if variant_index == 0 { err } else { ok },
            pointer_type,
            types,
        )
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
        let fields = if variant_index == 0 {
            vec![payload, inactive]
        } else {
            vec![inactive, payload]
        };
        return Ok(CompiledValue::Enum {
            tag: builder
                .ins()
                .iconst(cranelift_codegen::ir::types::I32, variant_index as i64),
            fields,
            ty: crate::sema::Type::Result(result_id),
        });
    }
    let crate::mir::MirTypeId::Enum(enum_id) = enum_id else {
        unreachable!("enum_type returns only enum types");
    };
    let variants = types.enum_variants(enum_id);
    if variant_index >= variants.len() {
        return Err(CodegenError::RuntimeError {
            message: "MIR enum variant id is out of bounds".to_owned(),
        });
    }
    let fields = &variants[variant_index].fields;
    let mut ordered = vec![None; fields.len()];
    for argument in arguments {
        let index = argument.parameter;
        if index >= ordered.len() {
            return Err(CodegenError::RuntimeError {
                message: "enum argument index out of bounds".to_owned(),
            });
        }
        if ordered[index].is_some() {
            return Err(CodegenError::RuntimeError {
                message: "duplicate enum constructor argument".to_owned(),
            });
        }
        ordered[index] = Some(values.get(&argument.value).cloned().ok_or_else(|| {
            CodegenError::RuntimeError {
                message: "MIR enum argument was not compiled".to_owned(),
            }
        })?);
    }
    if ordered.iter().any(Option::is_none) {
        return Err(CodegenError::RuntimeError {
            message: "missing enum constructor argument".to_owned(),
        });
    }
    let start = variants[..variant_index]
        .iter()
        .map(|variant| variant.fields.len())
        .sum::<usize>();
    let mut payload = variants
        .iter()
        .flat_map(|variant| variant.fields.iter())
        .map(|(_, ty)| zero_compiled_value(builder, *ty, pointer_type, types))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
    for (offset, value) in ordered.into_iter().enumerate() {
        payload[start + offset] = value.expect("checked enum constructor arguments");
    }
    Ok(CompiledValue::Enum {
        tag: builder
            .ins()
            .iconst(cranelift_codegen::ir::types::I32, variant_index as i64),
        fields: payload,
        ty: crate::sema::Type::Enum(enum_id),
    })
}

pub(super) fn expect_boolean(
    value: CompiledValue,
) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    match value {
        CompiledValue::Boolean { value } => Ok(value),
        _ => Err(CodegenError::RuntimeError {
            message: "MIR branch condition is not Bool".to_owned(),
        }),
    }
}
