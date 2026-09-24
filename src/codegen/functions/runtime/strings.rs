use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::abi::{append_result_params, value_arguments, value_from_params};
use super::super::super::constructors::zero_compiled_value;
use super::super::super::environment::CompiledValue;
use super::RuntimeCallContext;

pub(super) fn compile_string_call(
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
        RuntimeIntrinsic::DebugPath(ty) => {
            let CompiledValue::Bytes { pointer: parent } = string_value(&arguments[0])? else {
                unreachable!("checked Debug path")
            };
            let object = if matches!(ty, Type::Class(_)) {
                let CompiledValue::Class { pointer, .. } = string_value(&arguments[1])? else {
                    unreachable!("checked Debug class")
                };
                pointer
            } else {
                builder.ins().iconst(pointer_type, 0)
            };
            let call = builder
                .ins()
                .call(context.refs.debug_path, &[parent, object]);
            let pointer = builder.inst_results(call)[0];
            builder.ins().call(managed_drop_ref, &[parent]);
            values.insert(*destination, CompiledValue::Bytes { pointer });
        }
        RuntimeIntrinsic::DebugPathStatus => {
            let CompiledValue::Bytes { pointer } = string_value(&arguments[0])? else {
                unreachable!("checked Debug path")
            };
            let call = builder
                .ins()
                .call(context.refs.debug_path_status, &[pointer]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value,
                    ty: Type::I32,
                },
            );
        }
        RuntimeIntrinsic::DebugNativeId(_) => {
            let CompiledValue::NativeHandle { pointer, .. } = string_value(&arguments[0])? else {
                unreachable!("checked native Debug receiver")
            };
            let call = builder.ins().call(context.refs.debug_native_id, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::DebugBytes | RuntimeIntrinsic::DebugDuration => {
            let value = string_value(&arguments[0])?;
            let (function, argument, release) = match (intrinsic, value) {
                (RuntimeIntrinsic::DebugBytes, CompiledValue::Bytes { pointer }) => {
                    (context.refs.debug_bytes, pointer, true)
                }
                (
                    RuntimeIntrinsic::DebugDuration,
                    CompiledValue::Numeric {
                        value,
                        ty: Type::Duration,
                    },
                ) => (context.refs.debug_duration, value, false),
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "invalid Debug value".into(),
                    })
                }
            };
            let call = builder.ins().call(function, &[argument]);
            let pointer = builder.inst_results(call)[0];
            let call = builder.ins().call(string_len_ref, &[pointer]);
            let length = builder.inst_results(call)[0];
            if release {
                builder.ins().call(managed_drop_ref, &[argument]);
            }
            values.insert(*destination, CompiledValue::String { pointer, length });
        }
        RuntimeIntrinsic::DebugString => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "Debug requires a String".into(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.debug_string, &[pointer, length]);
            let result = builder.inst_results(call)[0];
            let len = builder.ins().call(string_len_ref, &[result]);
            let length = builder.inst_results(len)[0];
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::String {
                    pointer: result,
                    length,
                },
            );
        }
        RuntimeIntrinsic::Echo => {
            let CompiledValue::String {
                pointer: location,
                length: location_length,
            } = string_value(&arguments[0])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "echo requires a location String".into(),
                });
            };
            let CompiledValue::String { pointer, length } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "echo requires a debug String".into(),
                });
            };
            builder.ins().call(
                context.refs.echo,
                &[location, location_length, pointer, length],
            );
            builder.ins().call(managed_drop_ref, &[location]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::Show(ty) => {
            if arguments.len() != 1 {
                return Err(CodegenError::RuntimeError {
                    message: "Show expects one argument".to_owned(),
                });
            }
            let value = string_value(&arguments[0])?;
            let pointer = match (ty, value) {
                // MIR transfers the argument's reference to this intrinsic.
                // String.show is identity: forward that reference to the result.
                (Type::String, CompiledValue::String { pointer, .. }) => pointer,
                (ty, CompiledValue::Numeric { value, ty: actual })
                    if *ty == actual && ty.is_signed_integer() =>
                {
                    let value = if actual == Type::I64 {
                        value
                    } else {
                        builder.ins().sextend(types::I64, value)
                    };
                    let call = builder.ins().call(context.refs.show_i64, &[value]);
                    builder.inst_results(call)[0]
                }
                (ty, CompiledValue::Numeric { value, ty: actual })
                    if *ty == actual && ty.is_integer() =>
                {
                    let value = if actual == Type::U64 {
                        value
                    } else {
                        builder.ins().uextend(types::I64, value)
                    };
                    let call = builder.ins().call(context.refs.show_u64, &[value]);
                    builder.inst_results(call)[0]
                }
                (ty, CompiledValue::Numeric { value, ty: actual })
                    if *ty == actual && ty.is_float() =>
                {
                    let value = if actual == Type::F64 {
                        value
                    } else {
                        builder.ins().fpromote(types::F64, value)
                    };
                    let call = builder.ins().call(context.refs.show_f64, &[value]);
                    builder.inst_results(call)[0]
                }
                (Type::Bool, CompiledValue::Boolean { value }) => {
                    let call = builder.ins().call(context.refs.show_bool, &[value]);
                    builder.inst_results(call)[0]
                }
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "Show argument has invalid type".to_owned(),
                    })
                }
            };
            let length_call = builder.ins().call(string_len_ref, &[pointer]);
            let length = builder.inst_results(length_call)[0];
            values.insert(*destination, CompiledValue::String { pointer, length });
        }
        RuntimeIntrinsic::Print | RuntimeIntrinsic::Println => {
            if arguments.len() != 1 {
                return Err(CodegenError::RuntimeError {
                    message: "print expects one argument".to_owned(),
                });
            }
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "print argument is not a String".to_owned(),
                });
            };
            let output_ref = if matches!(intrinsic, RuntimeIntrinsic::Print) {
                context.refs.print
            } else {
                context.refs.println
            };
            builder.ins().call(output_ref, &[pointer, length]);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::CStrAsString => {
            if arguments.len() != 1 {
                return Err(CodegenError::RuntimeError {
                    message: "CStr to_string expects one argument".to_owned(),
                });
            }
            let CompiledValue::NativeHandle { pointer, .. } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "to_string receiver is not a CStr".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.string_from_cstr, &[pointer]);
            let result = builder.inst_results(call)[0];
            let length_call = builder.ins().call(string_len_ref, &[result]);
            let failed =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, result, 0);
            let tag = builder.ins().uextend(types::I32, failed);
            let option = Type::Option(types.option_id(Type::String).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "CStr to_string Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::String {
                        pointer: result,
                        length: builder.inst_results(length_call)[0],
                    }],
                    ty: option,
                },
            );
        }
        RuntimeIntrinsic::StringConcat => {
            if arguments.len() != 2 {
                return Err(CodegenError::RuntimeError {
                    message: "String concat expects two arguments".to_owned(),
                });
            }
            let CompiledValue::String {
                pointer: left,
                length: left_length,
            } = string_value(&arguments[0])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String concat left operand is not a String".to_owned(),
                });
            };
            let CompiledValue::String {
                pointer: right,
                length: right_length,
            } = string_value(&arguments[1])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String concat right operand is not a String".to_owned(),
                });
            };
            let call = builder.ins().call(
                context.refs.string_concat,
                &[left, left_length, right, right_length],
            );
            let pointer = builder.inst_results(call)[0];
            let length_call = builder.ins().call(string_len_ref, &[pointer]);
            let length = builder.inst_results(length_call)[0];
            builder.ins().call(managed_drop_ref, &[left]);
            builder.ins().call(managed_drop_ref, &[right]);
            values.insert(*destination, CompiledValue::String { pointer, length });
        }
        RuntimeIntrinsic::StringEqual | RuntimeIntrinsic::StringNotEqual => {
            if arguments.len() != 2 {
                return Err(CodegenError::RuntimeError {
                    message: "String equality expects two arguments".to_owned(),
                });
            }
            let CompiledValue::String {
                pointer: left,
                length: left_length,
            } = string_value(&arguments[0])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String equality left operand is not a String".to_owned(),
                });
            };
            let CompiledValue::String {
                pointer: right,
                length: right_length,
            } = string_value(&arguments[1])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String equality right operand is not a String".to_owned(),
                });
            };
            let call = builder.ins().call(
                context.refs.string_eq,
                &[left, left_length, right, right_length],
            );
            let value = builder.inst_results(call)[0];
            let value = if matches!(intrinsic, RuntimeIntrinsic::StringNotEqual) {
                builder.ins().bxor_imm_u(value, 1)
            } else {
                value
            };
            builder.ins().call(managed_drop_ref, &[left]);
            builder.ins().call(managed_drop_ref, &[right]);
            values.insert(*destination, CompiledValue::Boolean { value });
        }
        RuntimeIntrinsic::StringIsEmpty => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "is_empty receiver is not a String".to_owned(),
                });
            };
            let value =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, length, 0);
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(*destination, CompiledValue::Boolean { value });
        }
        RuntimeIntrinsic::StringByteCount => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "byte_count receiver is not a String".to_owned(),
                });
            };
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: length,
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::StringScalarCount | RuntimeIntrinsic::StringGraphemeCount => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String count receiver is not a String".to_owned(),
                });
            };
            let count_ref = match intrinsic {
                RuntimeIntrinsic::StringScalarCount => context.refs.string_scalar_count,
                RuntimeIntrinsic::StringGraphemeCount => context.refs.string_grapheme_count,
                _ => unreachable!(),
            };
            let call = builder.ins().call(count_ref, &[pointer, length]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value,
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::StringIsAscii => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "is_ascii receiver is not a String".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.string_is_ascii, &[pointer, length]);
            let value = builder.inst_results(call)[0];
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(*destination, CompiledValue::Boolean { value });
        }
        RuntimeIntrinsic::StringTrim
        | RuntimeIntrinsic::StringToUpper
        | RuntimeIntrinsic::StringToLower => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String transform receiver is not a String".to_owned(),
                });
            };
            let transform_ref = match intrinsic {
                RuntimeIntrinsic::StringTrim => context.refs.string_trim,
                RuntimeIntrinsic::StringToUpper => context.refs.string_to_upper,
                RuntimeIntrinsic::StringToLower => context.refs.string_to_lower,
                _ => unreachable!(),
            };
            let call = builder.ins().call(transform_ref, &[pointer, length]);
            let result_pointer = builder.inst_results(call)[0];
            let result_length_call = builder.ins().call(string_len_ref, &[result_pointer]);
            let result_length = builder.inst_results(result_length_call)[0];
            builder.ins().call(managed_drop_ref, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::String {
                    pointer: result_pointer,
                    length: result_length,
                },
            );
        }
        RuntimeIntrinsic::StringAsCString => {
            if arguments.len() != 1 {
                return Err(CodegenError::RuntimeError {
                    message: "String as_cstr expects one argument".to_owned(),
                });
            }
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "as_cstr receiver is not a String".to_owned(),
                });
            };
            // The argument is a scope-leased local read without a reference
            // bump; dropping it here would free the payload while C still
            // borrows it.
            if cfg!(debug_assertions) {
                builder
                    .ins()
                    .call(context.refs.string_c_string_check, &[pointer, length]);
            }
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer,
                    ty: Type::CStr,
                },
            );
        }
        RuntimeIntrinsic::StringStartsWith
        | RuntimeIntrinsic::StringEndsWith
        | RuntimeIntrinsic::StringContains => {
            if arguments.len() != 2 {
                return Err(CodegenError::RuntimeError {
                    message: "String search expects two arguments".to_owned(),
                });
            }
            let CompiledValue::String {
                pointer: value,
                length: value_length,
            } = string_value(&arguments[0])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String search receiver is not a String".to_owned(),
                });
            };
            let CompiledValue::String {
                pointer: needle,
                length: needle_length,
            } = string_value(&arguments[1])?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "String search argument is not a String".to_owned(),
                });
            };
            let search_ref = match intrinsic {
                RuntimeIntrinsic::StringStartsWith => context.refs.string_starts_with,
                RuntimeIntrinsic::StringEndsWith => context.refs.string_ends_with,
                RuntimeIntrinsic::StringContains => context.refs.string_contains,
                _ => unreachable!(),
            };
            let call = builder
                .ins()
                .call(search_ref, &[value, value_length, needle, needle_length]);
            let result = builder.inst_results(call)[0];
            builder.ins().call(managed_drop_ref, &[value]);
            builder.ins().call(managed_drop_ref, &[needle]);
            values.insert(*destination, CompiledValue::Boolean { value: result });
        }
        RuntimeIntrinsic::StringParse(target) => {
            let CompiledValue::String { pointer, length } = string_value(&arguments[0])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String parse receiver is not a String".to_owned(),
                });
            };
            let (parser, value_type, size) = match target {
                Type::I8 => (context.refs.parse_i8, types::I8, 1),
                Type::I16 => (context.refs.parse_i16, types::I16, 2),
                Type::I32 => (context.refs.parse_i32, types::I32, 4),
                Type::I64 => (context.refs.parse_i64, types::I64, 8),
                Type::U8 => (context.refs.parse_u8, types::I8, 1),
                Type::U16 => (context.refs.parse_u16, types::I16, 2),
                Type::U32 => (context.refs.parse_u32, types::I32, 4),
                Type::U64 => (context.refs.parse_u64, types::I64, 8),
                Type::F32 => (context.refs.parse_f32, types::F32, 4),
                Type::F64 => (context.refs.parse_f64, types::F64, 8),
                Type::Bool => (context.refs.parse_bool, types::I8, 1),
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "unsupported String parse target".to_owned(),
                    })
                }
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                0,
            ));
            let output = builder.ins().stack_addr(pointer_type, slot, 0);
            let call = builder.ins().call(parser, &[pointer, length, output]);
            let success = builder.inst_results(call)[0];
            let result_type =
                Type::Result(types.result_id(*target, Type::String).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "String parse result type was not interned".to_owned(),
                    }
                })?);
            let success_block = builder.create_block();
            let error_block = builder.create_block();
            let merge_block = builder.create_block();
            append_result_params(builder, merge_block, result_type, pointer_type, types);
            builder
                .ins()
                .brif(success, success_block, &[], error_block, &[]);

            builder.switch_to_block(success_block);
            builder.seal_block(success_block);
            let parsed = builder.ins().stack_load(pointer_type, value_type, slot, 0);
            let parsed = if *target == Type::Bool {
                CompiledValue::Boolean { value: parsed }
            } else {
                CompiledValue::Numeric {
                    value: parsed,
                    ty: *target,
                }
            };
            builder.ins().call(managed_drop_ref, &[pointer]);
            let success_value = CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, 0),
                fields: vec![
                    parsed,
                    zero_compiled_value(builder, Type::String, pointer_type, types).map_err(
                        |error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        },
                    )?,
                ],
                ty: result_type,
            };
            let success_arguments = value_arguments(success_value)
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>();
            builder.ins().jump(merge_block, &success_arguments);

            builder.switch_to_block(error_block);
            builder.seal_block(error_block);
            let error_value = CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, 1),
                fields: vec![
                    zero_compiled_value(builder, *target, pointer_type, types).map_err(
                        |error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        },
                    )?,
                    CompiledValue::String { pointer, length },
                ],
                ty: result_type,
            };
            let error_arguments = value_arguments(error_value)
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>();
            builder.ins().jump(merge_block, &error_arguments);

            builder.switch_to_block(merge_block);
            builder.seal_block(merge_block);
            let parsed_result =
                value_from_params(builder.block_params(merge_block), result_type, types).map_err(
                    |error| CodegenError::RuntimeError {
                        message: error.to_string(),
                    },
                )?;
            values.insert(*destination, parsed_result);
        }
        RuntimeIntrinsic::StringSplit => {
            let (left, left_length) = string_pair(&string_value, &arguments[0], "split receiver")?;
            let (right, right_length) =
                string_pair(&string_value, &arguments[1], "split separator")?;
            let call = builder.ins().call(
                context.refs.string_split,
                &[left, left_length, right, right_length],
            );
            let list_id =
                types
                    .list_id(Type::String)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "String split List was not interned".to_owned(),
                    })?;
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::List(list_id),
                },
            );
            builder.ins().call(managed_drop_ref, &[left]);
            builder.ins().call(managed_drop_ref, &[right]);
        }
        RuntimeIntrinsic::StringReplace => {
            let (receiver, receiver_length) =
                string_pair(&string_value, &arguments[0], "replace receiver")?;
            let (from, from_length) = string_pair(&string_value, &arguments[1], "replace from")?;
            let (to, to_length) = string_pair(&string_value, &arguments[2], "replace to")?;
            let call = builder.ins().call(
                context.refs.string_replace,
                &[receiver, receiver_length, from, from_length, to, to_length],
            );
            let result = builder.inst_results(call)[0];
            let length_call = builder.ins().call(string_len_ref, &[result]);
            values.insert(
                *destination,
                CompiledValue::String {
                    pointer: result,
                    length: builder.inst_results(length_call)[0],
                },
            );
            builder.ins().call(managed_drop_ref, &[receiver]);
            builder.ins().call(managed_drop_ref, &[from]);
            builder.ins().call(managed_drop_ref, &[to]);
        }
        RuntimeIntrinsic::StringGetByte => {
            let (pointer, length) = string_pair(&string_value, &arguments[0], "get_byte receiver")?;
            let CompiledValue::Numeric { value: index, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String get_byte index is not numeric".to_owned(),
                });
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                1,
                0,
            ));
            let output = builder.ins().stack_addr(pointer_type, slot, 0);
            let call = builder.ins().call(
                context.refs.string_get_byte,
                &[pointer, length, index, output],
            );
            let success = builder.inst_results(call)[0];
            let value = builder.ins().stack_load(pointer_type, types::I8, slot, 0);
            let failed = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let option = Type::Option(types.option_id(Type::U8).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "String get_byte Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag: builder.ins().uextend(types::I32, failed),
                    fields: vec![CompiledValue::Numeric {
                        value,
                        ty: Type::U8,
                    }],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::StringSlice => {
            let (pointer, length) = string_pair(&string_value, &arguments[0], "slice receiver")?;
            let CompiledValue::Numeric { value: start, .. } = string_value(&arguments[1])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String slice start is not numeric".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: end, .. } = string_value(&arguments[2])? else {
                return Err(CodegenError::RuntimeError {
                    message: "String slice end is not numeric".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.string_slice, &[pointer, length, start, end]);
            let result = builder.inst_results(call)[0];
            let length_call = builder.ins().call(string_len_ref, &[result]);
            let failed =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, result, 0);
            let option = Type::Option(types.option_id(Type::String).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "String slice Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag: builder.ins().uextend(types::I32, failed),
                    fields: vec![CompiledValue::String {
                        pointer: result,
                        length: builder.inst_results(length_call)[0],
                    }],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn string_pair(
    string_value: &dyn Fn(&MirCallArgument) -> Result<CompiledValue, CodegenError>,
    argument: &MirCallArgument,
    label: &str,
) -> Result<(cranelift_codegen::ir::Value, cranelift_codegen::ir::Value), CodegenError> {
    let CompiledValue::String { pointer, length } = string_value(argument)? else {
        return Err(CodegenError::RuntimeError {
            message: format!("String {label} is not a String"),
        });
    };
    Ok((pointer, length))
}
