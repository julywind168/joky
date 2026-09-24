use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::environment::CompiledValue;
use super::RuntimeCallContext;

pub(super) fn compile_bytes_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
) -> Result<bool, CodegenError> {
    let types = context.types;
    let pointer_type = context.pointer_type;
    let managed_drop_ref = context.refs.managed_drop;
    let string_len_ref = context.refs.string_len;
    let string_value = |value: &MirCallArgument| -> Result<CompiledValue, CodegenError> {
        values
            .get(&value.value)
            .cloned()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "MIR String argument was not compiled".to_owned(),
            })
    };

    match intrinsic {
        RuntimeIntrinsic::BytesNew => {
            let call = builder.ins().call(context.refs.bytes_new, &[]);
            values.insert(
                *destination,
                CompiledValue::Bytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::BytesFromString => {
            let argument = string_value(&arguments[0])?;
            let CompiledValue::String { pointer, length } = argument else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes.from_string expects String".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.bytes_from_string, &[pointer, length]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Bytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::BytesLength => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.bytes_length, &[pointer]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::BytesIsEmpty => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.bytes_is_empty, &[pointer]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::BytesCursorNew => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            // The runtime retains an independent handle reference; release the
            // borrowed receiver reference afterwards.
            let call = builder
                .ins()
                .call(context.refs.bytes_cursor_new, &[pointer]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::BytesCursor,
                },
            );
        }
        RuntimeIntrinsic::BytesCursorAdvance => {
            let CompiledValue::NativeHandle { pointer, .. } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes cursor receiver has invalid type".to_owned(),
                });
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                pointer_type.bytes(),
                3,
            ));
            let address = builder.ins().stack_addr(pointer_type, slot, 0);
            // The runtime adopts the receiver reference: on exhaustion it is
            // released inside the call, otherwise the successor carries it.
            let call = builder
                .ins()
                .call(context.refs.bytes_cursor_step, &[pointer, address]);
            let next = builder.inst_results(call)[0];
            let exhausted =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, next, 0);
            let tag = builder
                .ins()
                .uextend(cranelift_codegen::ir::types::I32, exhausted);
            let byte = builder.ins().stack_load(pointer_type, types::I8, slot, 0);
            let pair = Type::Tuple(types.tuple_id(&[Type::U8, Type::BytesCursor]).ok_or_else(
                || CodegenError::RuntimeError {
                    message: "Bytes cursor pair was not interned".to_owned(),
                },
            )?);
            let option =
                Type::Option(
                    types
                        .option_id(pair)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "Bytes cursor Option was not interned".to_owned(),
                        })?,
                );
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Tuple {
                        elements: vec![
                            CompiledValue::Numeric {
                                value: byte,
                                ty: Type::U8,
                            },
                            CompiledValue::NativeHandle {
                                pointer: next,
                                ty: Type::BytesCursor,
                            },
                        ],
                        ty: pair,
                    }],
                    ty: option,
                },
            );
        }
        RuntimeIntrinsic::BytesGet => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: index, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes index is not numeric".to_owned(),
                });
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                1,
                0,
            ));
            let output = builder.ins().stack_addr(pointer_type, slot, 0);
            let zero = builder.ins().iconst(types::I8, 0);
            builder.ins().stack_store(pointer_type, zero, slot, 0);
            let call = builder
                .ins()
                .call(context.refs.bytes_get, &[pointer, index, output]);
            let success = builder.inst_results(call)[0];
            let value = builder.ins().stack_load(pointer_type, types::I8, slot, 0);
            let failed = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder.ins().uextend(types::I32, failed);
            let option = Type::Option(types.option_id(Type::U8).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "Bytes.get Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Numeric {
                        value,
                        ty: Type::U8,
                    }],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::BytesSlice => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: start, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes slice start is not numeric".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: length, .. } = string_value(&arguments[2])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes slice length is not numeric".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.bytes_slice, &[pointer, start, length]);
            let result = builder.inst_results(call)[0];
            let failed =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, result, 0);
            let tag = builder.ins().uextend(types::I32, failed);
            let option = Type::Option(types.option_id(Type::Bytes).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "Bytes.slice Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Bytes { pointer: result }],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::BytesConcat => {
            let CompiledValue::Bytes { pointer: left } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes left operand has invalid type".to_owned(),
                });
            };
            let CompiledValue::Bytes { pointer: right } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes right operand has invalid type".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.bytes_concat, &[left, right]);
            values.insert(
                *destination,
                CompiledValue::Bytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
            builder.ins().call(managed_drop_ref, &[left]);
            builder.ins().call(managed_drop_ref, &[right]);
        }
        RuntimeIntrinsic::BytesToString => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Bytes receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.bytes_to_string, &[pointer]);
            let result = builder.inst_results(call)[0];
            let length = builder.ins().call(string_len_ref, &[result]);
            let failed =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, result, 0);
            let tag = builder.ins().uextend(types::I32, failed);
            let option = Type::Option(types.option_id(Type::String).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "Bytes.to_string Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::String {
                        pointer: result,
                        length: builder.inst_results(length)[0],
                    }],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::MutBytesNew => {
            let call = builder.ins().call(context.refs.mut_bytes_new, &[]);
            values.insert(
                *destination,
                CompiledValue::MutBytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::MutBytesWithCapacity => {
            let CompiledValue::Numeric {
                value: capacity, ..
            } = string_value(&arguments[0])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.with_capacity expects UInt64".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_with_capacity, &[capacity]);
            values.insert(
                *destination,
                CompiledValue::MutBytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::MutBytesFromBytes => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.from_bytes expects Bytes".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_from_bytes, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::MutBytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::MutBytesLength | RuntimeIntrinsic::MutBytesCapacity => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let reference = if matches!(intrinsic, RuntimeIntrinsic::MutBytesLength) {
                context.refs.mut_bytes_length
            } else {
                context.refs.mut_bytes_capacity
            };
            let call = builder.ins().call(reference, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::MutBytesPush => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric { value, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.push expects UInt8".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_push, &[pointer, value]);
            let _ = builder.inst_results(call);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::MutBytesGet | RuntimeIntrinsic::MutBytesPop => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let (call, slot) = if matches!(intrinsic, RuntimeIntrinsic::MutBytesGet) {
                let CompiledValue::Numeric { value: index, .. } = string_value(&arguments[1])?
                else {
                    return Err(CodegenError::RuntimeError {
                        message: "MutBytes index is not numeric".to_owned(),
                    });
                };
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    1,
                    0,
                ));
                let output = builder.ins().stack_addr(pointer_type, slot, 0);
                let zero = builder.ins().iconst(types::I8, 0);
                builder.ins().stack_store(pointer_type, zero, slot, 0);
                (
                    builder
                        .ins()
                        .call(context.refs.mut_bytes_get, &[pointer, index, output]),
                    slot,
                )
            } else {
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    1,
                    0,
                ));
                let output = builder.ins().stack_addr(pointer_type, slot, 0);
                let zero = builder.ins().iconst(types::I8, 0);
                builder.ins().stack_store(pointer_type, zero, slot, 0);
                (
                    builder
                        .ins()
                        .call(context.refs.mut_bytes_pop, &[pointer, output]),
                    slot,
                )
            };
            let success = builder.inst_results(call)[0];
            let value = builder.ins().stack_load(pointer_type, types::I8, slot, 0);
            let failed = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder.ins().uextend(types::I32, failed);
            let option = Type::Option(types.option_id(Type::U8).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutBytes Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Numeric {
                        value,
                        ty: Type::U8,
                    }],
                    ty: option,
                },
            );
        }
        RuntimeIntrinsic::MutBytesSet => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: index, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes index is not numeric".to_owned(),
                });
            };
            let CompiledValue::Numeric { value, .. } = string_value(&arguments[2])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.set expects UInt8".to_owned(),
                });
            };
            builder
                .ins()
                .call(context.refs.mut_bytes_set, &[pointer, index, value]);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::MutBytesClear => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.mut_bytes_clear, &[pointer]);
            let _ = builder.inst_results(call);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::MutBytesReserve => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric {
                value: additional, ..
            } = string_value(&arguments[1])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.reserve expects UInt64".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_reserve, &[pointer, additional]);
            let _ = builder.inst_results(call);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::MutBytesExtend => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Bytes { pointer: bytes } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes.extend expects Bytes".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_extend, &[pointer, bytes]);
            let _ = builder.inst_results(call);
            builder.ins().call(managed_drop_ref, &[bytes]);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::MutBytesToBytes => {
            let CompiledValue::MutBytes { pointer } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "MutBytes receiver has invalid type".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_bytes_to_bytes, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Bytes {
                    pointer: builder.inst_results(call)[0],
                },
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}
