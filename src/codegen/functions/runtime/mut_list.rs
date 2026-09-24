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

pub(super) fn compile_mut_list_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
) -> Result<bool, CodegenError> {
    let types = context.types;
    let pointer_type = context.pointer_type;
    match intrinsic {
        RuntimeIntrinsic::MutListNew(element) => {
            if types.is_owned(*element) || matches!(*element, Type::Param(_)) {
                return Err(CodegenError::RuntimeError {
                    message: "MutList element has no supported runtime representation".to_owned(),
                });
            }
            let word_len = abi_types(*element, pointer_type, types).len().max(1);
            let mask_len = word_len.div_ceil(64);
            let word_count = builder.ins().iconst(pointer_type, word_len as i64);
            let mask_count = builder.ins().iconst(pointer_type, mask_len as i64);
            let call = builder
                .ins()
                .call(context.refs.mut_list_new, &[word_count, mask_count]);
            let id = types
                .list_id(*element)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutList type was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::MutList {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutList(id),
                },
            );
        }
        RuntimeIntrinsic::MutListPush(element) => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let value = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutList element was not compiled".to_owned(),
                }
            })?;
            let (words, masks) = list_element_words(builder, value)?;
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
            let words_len = builder.ins().iconst(pointer_type, words.len() as i64);
            let masks_len = builder.ins().iconst(pointer_type, masks.len() as i64);
            let call = builder.ins().call(
                context.refs.mut_list_push,
                &[pointer, words_pointer, words_len, masks_pointer, masks_len],
            );
            let _ = builder.inst_results(call)[0];
            values.insert(*destination, CompiledValue::Unit);
            let _ = element;
        }
        RuntimeIntrinsic::MutListGet(element) | RuntimeIntrinsic::MutListPop(element) => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let word_count = abi_types(*element, pointer_type, types).len().max(1);
            let (slot, output) = create_list_word_slot(builder, pointer_type, word_count)?;
            let success = if matches!(intrinsic, RuntimeIntrinsic::MutListGet(_)) {
                let index = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList index was not compiled".to_owned(),
                    }
                })?;
                let CompiledValue::Numeric { value: index, .. } = index else {
                    return Err(CodegenError::RuntimeError {
                        message: "MutList index is not numeric".to_owned(),
                    });
                };
                let word_count_value = builder.ins().iconst(pointer_type, word_count as i64);
                builder.ins().call(
                    context.refs.mut_list_get,
                    &[pointer, index, output, word_count_value],
                )
            } else {
                let word_count_value = builder.ins().iconst(pointer_type, word_count as i64);
                builder.ins().call(
                    context.refs.mut_list_pop,
                    &[pointer, output, word_count_value],
                )
            };
            let ok = builder.inst_results(success)[0];
            let failed =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, ok, 0);
            let tag = builder
                .ins()
                .uextend(cranelift_codegen::ir::types::I32, failed);
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
                    message: "MutList Option was not interned".to_owned(),
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
        }
        RuntimeIntrinsic::MutListSet(element) => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let CompiledValue::Numeric { value: index, .. } = values
                .get(&arguments[1].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutList index was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList index is not numeric".to_owned(),
                });
            };
            let value = values.get(&arguments[2].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutList element was not compiled".to_owned(),
                }
            })?;
            let (words, masks) = list_element_words(builder, value)?;
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
            let words_len = builder.ins().iconst(pointer_type, words.len() as i64);
            let masks_len = builder.ins().iconst(pointer_type, masks.len() as i64);
            builder.ins().call(
                context.refs.mut_list_set,
                &[
                    pointer,
                    index,
                    words_pointer,
                    words_len,
                    masks_pointer,
                    masks_len,
                ],
            );
            values.insert(*destination, CompiledValue::Unit);
            let _ = element;
        }
        RuntimeIntrinsic::MutListLength | RuntimeIntrinsic::MutListCapacity => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let reference = if matches!(intrinsic, RuntimeIntrinsic::MutListLength) {
                context.refs.mut_list_length
            } else {
                context.refs.mut_list_capacity
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
        RuntimeIntrinsic::MutListIntoIter(element) => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_list_cursor_new, &[pointer]);
            let id = types
                .list_id(*element)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutListCursor type was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutListCursor(id),
                },
            );
        }
        RuntimeIntrinsic::MutListToList(element) => {
            let CompiledValue::MutList { pointer, .. } =
                values.get(&arguments[0].value).cloned().ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutList receiver was not compiled".to_owned(),
                    }
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList receiver has invalid type".to_owned(),
                });
            };
            let call = builder
                .ins()
                .call(context.refs.mut_list_to_list, &[pointer]);
            let id = types
                .list_id(*element)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutList to_list List was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::List(id),
                },
            );
        }
        RuntimeIntrinsic::MutListCursorAdvance(element) => {
            let CompiledValue::NativeHandle {
                pointer: cursor,
                ty: receiver_type,
            } = values.get(&arguments[0].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutList cursor receiver was not compiled".to_owned(),
                }
            })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList cursor receiver has invalid type".to_owned(),
                });
            };
            let word_count = abi_types(*element, pointer_type, types).len().max(1);
            let (slot, output) = create_list_word_slot(builder, pointer_type, word_count)?;
            let word_count_value = builder.ins().iconst(pointer_type, word_count as i64);
            let call = builder.ins().call(
                context.refs.mut_list_cursor_step,
                &[cursor, output, word_count_value],
            );
            let next = builder.inst_results(call)[0];
            let exhausted =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, next, 0);
            let tag = builder
                .ins()
                .uextend(cranelift_codegen::ir::types::I32, exhausted);
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
            let item = list_value_from_words(builder, &words, *element, types)?;
            let pair =
                Type::Tuple(types.tuple_id(&[*element, receiver_type]).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MutListCursor pair was not interned".to_owned(),
                    }
                })?);
            let option =
                Type::Option(
                    types
                        .option_id(pair)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MutListCursor Option was not interned".to_owned(),
                        })?,
                );
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Tuple {
                        elements: vec![
                            item,
                            CompiledValue::NativeHandle {
                                pointer: next,
                                ty: receiver_type,
                            },
                        ],
                        ty: pair,
                    }],
                    ty: option,
                },
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}
