use std::collections::HashMap;

use cranelift_codegen::ir::InstBuilder;
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::abi::abi_types;
use super::super::super::environment::CompiledValue;
use super::super::values::{list_element_words, list_value_from_words};
use super::RuntimeCallContext;
use super::{create_list_word_slot, store_list_words};

pub(super) fn compile_list_call(
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
    match intrinsic {
        RuntimeIntrinsic::ListEmpty(element) => {
            let list_id = types
                .list_id(*element)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "List type was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.ins().iconst(pointer_type, 0),
                    ty: Type::List(list_id),
                },
            );
        }
        RuntimeIntrinsic::ListCons(element) => {
            let head = values.get(&arguments[0].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "List head was not compiled".to_owned(),
                }
            })?;
            let tail = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "List tail was not compiled".to_owned(),
                }
            })?;
            let CompiledValue::List { pointer: tail, ty } = tail else {
                return Err(CodegenError::RuntimeError {
                    message: "List tail has invalid type".to_owned(),
                });
            };
            let (words, masks) = list_element_words(builder, head)?;
            let words_pointer = store_list_words(builder, pointer_type, &words)?;
            let mask_values = masks
                .iter()
                .map(|mask| {
                    builder
                        .ins()
                        .iconst(cranelift_codegen::ir::types::I64, *mask as i64)
                })
                .collect::<Vec<_>>();
            let masks_pointer = store_list_words(builder, pointer_type, &mask_values)?;
            let word_count = builder
                .ins()
                .iconst(pointer_type, words.len().try_into().unwrap_or(i64::MAX));
            let mask_count = builder
                .ins()
                .iconst(pointer_type, masks.len().try_into().unwrap_or(i64::MAX));
            let call = builder.ins().call(
                context.refs.list_cons,
                &[words_pointer, word_count, masks_pointer, mask_count, tail],
            );
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty,
                },
            );
            let _ = element;
        }
        RuntimeIntrinsic::ListIsEmpty => {
            let CompiledValue::List { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "List receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "List receiver has invalid type".to_owned(),
                });
            };
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.ins().icmp_imm_u(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        pointer,
                        0,
                    ),
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::ListHead(element) => {
            let CompiledValue::List { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "List receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "List receiver has invalid type".to_owned(),
                });
            };
            if types.is_owned(*element) || matches!(element, Type::Param(_)) {
                return Err(CodegenError::RuntimeError {
                    message: "List element has no runtime representation".to_owned(),
                });
            }
            let word_count = abi_types(*element, pointer_type, types).len().max(1);
            let (slot, output) = create_list_word_slot(builder, pointer_type, word_count)?;
            let count = builder
                .ins()
                .iconst(pointer_type, word_count.try_into().unwrap_or(i64::MAX));
            let call = builder
                .ins()
                .call(context.refs.list_head, &[pointer, output, count]);
            let success = builder.inst_results(call)[0];
            let is_none = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder
                .ins()
                .uextend(cranelift_codegen::ir::types::I32, is_none);
            let words = (0..word_count)
                .map(|index| {
                    builder.ins().stack_load(
                        pointer_type,
                        cranelift_codegen::ir::types::I64,
                        slot,
                        (index * 8) as i32,
                    )
                })
                .collect::<Vec<_>>();
            let value = list_value_from_words(builder, &words, *element, types)?;
            let option = Type::Option(types.option_id(*element).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "List head Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![value],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::ListTail(element) => {
            let CompiledValue::List { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "List receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "List receiver has invalid type".to_owned(),
                });
            };
            let tag = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                pointer,
                0,
            );
            let tag = builder
                .ins()
                .uextend(cranelift_codegen::ir::types::I32, tag);
            let call = builder.ins().call(context.refs.list_tail, &[pointer]);
            let list =
                Type::List(
                    types
                        .list_id(*element)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "List type was not interned".to_owned(),
                        })?,
                );
            let tail = CompiledValue::List {
                pointer: builder.inst_results(call)[0],
                ty: list,
            };
            let option =
                Type::Option(
                    types
                        .option_id(list)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "List tail Option was not interned".to_owned(),
                        })?,
                );
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![tail],
                    ty: option,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::ListLength => {
            let CompiledValue::List { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR List receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR List receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.list_length, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        RuntimeIntrinsic::ListReverse(element) => {
            let CompiledValue::List { pointer, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR List receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR List receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.list_reverse, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty,
                },
            );
            let _ = element;
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        _ => return Ok(false),
    }
    Ok(true)
}
