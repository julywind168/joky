use std::collections::HashMap;

use cranelift_codegen::ir::InstBuilder;
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::abi::value_arguments;
use super::super::super::environment::{CompiledValue, Environment};
use super::super::statements::create_task_slot;
use super::RuntimeCallContext;

pub(super) fn compile_cown_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &Environment,
) -> Result<bool, CodegenError> {
    let types = context.types;
    let pointer_type = context.pointer_type;
    let value_for = |argument: &MirCallArgument| -> Result<CompiledValue, CodegenError> {
        values
            .get(&argument.value)
            .cloned()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "MIR Cown argument was not compiled".to_owned(),
            })
    };

    match intrinsic {
        RuntimeIntrinsic::CownNew(payload_type) => {
            let [argument] = arguments else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown.new expects one argument".to_owned(),
                });
            };
            let payload_arguments = value_arguments(value_for(argument)?);
            let [payload] = payload_arguments.as_slice() else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown payload must have a pointer representation".to_owned(),
                });
            };
            let drop_ref = match payload_type {
                Type::Class(id) => *environment.class_drop_refs.get(id).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: format!("class {id} has no Cown payload drop glue"),
                    }
                })?,
                _ => context.refs.managed_drop,
            };
            let payload_drop = builder.ins().func_addr(pointer_type, drop_ref);
            let call = builder
                .ins()
                .call(context.refs.cown_new, &[*payload, payload_drop]);
            let pointer = builder.inst_results(call)[0];
            values.insert(
                *destination,
                CompiledValue::Cown {
                    pointer,
                    ty: Type::Cown(types.cown_id(*payload_type).ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "Cown payload type was not interned".to_owned(),
                        }
                    })?),
                },
            );
        }
        RuntimeIntrinsic::CownAcquire(payload_type) => {
            let [argument] = arguments else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown.acquire expects one argument".to_owned(),
                });
            };
            let cown_arguments = value_arguments(value_for(argument)?);
            let [cown] = cown_arguments.as_slice() else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown handle must have a pointer representation".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.cown_acquire, &[*cown]);
            let pointer = builder.inst_results(call)[0];
            let value = match payload_type {
                Type::Class(id) => CompiledValue::Class {
                    pointer,
                    ty: Type::Class(*id),
                },
                Type::MutList(id) => CompiledValue::MutList {
                    pointer,
                    ty: Type::MutList(*id),
                },
                Type::MutMap(id) => CompiledValue::MutMap {
                    pointer,
                    ty: Type::MutMap(*id),
                },
                Type::MutSet(id) => CompiledValue::MutSet {
                    pointer,
                    ty: Type::MutSet(*id),
                },
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "Cown payload must have a unique pointer representation"
                            .to_owned(),
                    })
                }
            };
            values.insert(*destination, value);
        }
        RuntimeIntrinsic::CownAcquireMany => {
            if arguments.is_empty() {
                return Err(CodegenError::RuntimeError {
                    message: "Cown.acquire_many expects handles".into(),
                });
            }
            let (slot, handles) = create_task_slot(builder, pointer_type, arguments.len())?;
            let (_out_slot, outputs) = create_task_slot(builder, pointer_type, arguments.len())?;
            for (index, argument) in arguments.iter().enumerate() {
                let argument_values = value_arguments(value_for(argument)?);
                let [pointer] = argument_values.as_slice() else {
                    return Err(CodegenError::RuntimeError {
                        message: "Cown handle must have a pointer representation".into(),
                    });
                };
                builder
                    .ins()
                    .stack_store(pointer_type, *pointer, slot, (index * 8) as i32);
            }
            let count = builder.ins().iconst(pointer_type, arguments.len() as i64);
            let call = builder
                .ins()
                .call(context.refs.cown_acquire_many, &[handles, count, outputs]);
            let _ = builder.inst_results(call);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::CownPayload(payload_type) => {
            let [argument] = arguments else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown.payload expects one handle".into(),
                });
            };
            let argument_values = value_arguments(value_for(argument)?);
            let [pointer] = argument_values.as_slice() else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown handle must have a pointer representation".into(),
                });
            };
            let call = builder.ins().call(context.refs.cown_payload, &[*pointer]);
            let payload = builder.inst_results(call)[0];
            let value = match payload_type {
                Type::Class(_) => CompiledValue::Class {
                    pointer: payload,
                    ty: *payload_type,
                },
                Type::MutList(_) => CompiledValue::MutList {
                    pointer: payload,
                    ty: *payload_type,
                },
                Type::MutMap(_) => CompiledValue::MutMap {
                    pointer: payload,
                    ty: *payload_type,
                },
                Type::MutSet(_) => CompiledValue::MutSet {
                    pointer: payload,
                    ty: *payload_type,
                },
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "Cown payload must have a unique pointer representation".into(),
                    })
                }
            };
            values.insert(*destination, value);
        }
        RuntimeIntrinsic::CownRelease => {
            let [argument] = arguments else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown release expects one argument".to_owned(),
                });
            };
            let cown_arguments = value_arguments(value_for(argument)?);
            let [cown] = cown_arguments.as_slice() else {
                return Err(CodegenError::RuntimeError {
                    message: "Cown handle must have a pointer representation".to_owned(),
                });
            };
            builder.ins().call(context.refs.cown_release, &[*cown]);
            values.insert(*destination, CompiledValue::Unit);
        }
        _ => return Ok(false),
    }
    Ok(true)
}
