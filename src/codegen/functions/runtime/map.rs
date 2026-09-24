use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::abi::abi_types;
use super::super::super::environment::CompiledValue;
use super::super::values::{list_element_words, list_value_from_words};
use super::RuntimeCallContext;
use super::{
    combine_masks, create_list_word_slot, drop_map_key, map_key_descriptor, store_list_words,
};

pub(super) fn compile_map_call(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
    environment: &super::Environment,
) -> Result<bool, CodegenError> {
    let types = context.types;
    let pointer_type = context.pointer_type;
    let managed_drop_ref = context.refs.managed_drop;
    match intrinsic {
        RuntimeIntrinsic::MapEmpty(key, value) => {
            let map_id = types
                .map_id(*key, *value)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Map type was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::Map {
                    pointer: builder.ins().iconst(pointer_type, 0),
                    ty: Type::Map(map_id),
                },
            );
        }
        RuntimeIntrinsic::MapInsert(key_type, value_type) => {
            let CompiledValue::Map { pointer: map, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR Map key was not compiled".to_owned(),
                }
            })?;
            let value = values.get(&arguments[2].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR Map value was not compiled".to_owned(),
                }
            })?;
            let (key_words, key_masks) = list_element_words(builder, key)?;
            let (value_words, value_masks) = list_element_words(builder, value)?;
            let key_word_count = key_words.len();
            let mut words = key_words;
            words.extend(value_words);
            let masks = combine_masks(&[
                (key_word_count, key_masks),
                (words.len() - key_word_count, value_masks),
            ]);
            let words_pointer = store_list_words(builder, pointer_type, &words)?;
            let mask_values = masks
                .iter()
                .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                .collect::<Vec<_>>();
            let masks_pointer = store_list_words(builder, pointer_type, &mask_values)?;
            let word_count = builder.ins().iconst(pointer_type, words.len() as i64);
            let mask_count = builder.ins().iconst(pointer_type, masks.len() as i64);
            let key_count = builder.ins().iconst(pointer_type, key_word_count as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *key_type, environment)?;
            let call = builder.ins().call(
                context.refs.map_insert,
                &[
                    map,
                    words_pointer,
                    word_count,
                    masks_pointer,
                    mask_count,
                    key_count,
                    key_kind,
                ],
            );
            values.insert(
                *destination,
                CompiledValue::Map {
                    pointer: builder.inst_results(call)[0],
                    ty,
                },
            );
            let _ = value_type;
        }
        RuntimeIntrinsic::MapGet(key_type, value_type) => {
            let CompiledValue::Map { pointer: map, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR Map key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let value_words = abi_types(*value_type, pointer_type, types).len().max(1);
            let (slot, output) = create_list_word_slot(builder, pointer_type, value_words)?;
            let key_count = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *key_type, environment)?;
            let value_count = builder.ins().iconst(pointer_type, value_words as i64);
            let call = builder.ins().call(
                context.refs.map_get,
                &[map, key_pointer, key_count, key_kind, output, value_count],
            );
            let success = builder.inst_results(call)[0];
            let is_none = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder.ins().uextend(types::I32, is_none);
            let words = (0..value_words)
                .map(|index| {
                    builder
                        .ins()
                        .stack_load(pointer_type, types::I64, slot, (index * 8) as i32)
                })
                .collect::<Vec<_>>();
            let value = list_value_from_words(builder, &words, *value_type, types)?;
            let option = Type::Option(types.option_id(*value_type).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "Map get Option type was not interned".to_owned(),
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
            drop_map_key(builder, key, managed_drop_ref);
            builder.ins().call(managed_drop_ref, &[map]);
        }
        RuntimeIntrinsic::MapRemove(key_type, value_type) => {
            let CompiledValue::Map { pointer: map, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR Map key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_count = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *key_type, environment)?;
            let call = builder.ins().call(
                context.refs.map_remove,
                &[map, key_pointer, key_count, key_kind],
            );
            values.insert(
                *destination,
                CompiledValue::Map {
                    pointer: builder.inst_results(call)[0],
                    ty,
                },
            );
            drop_map_key(builder, key, managed_drop_ref);
            let _ = value_type;
        }
        RuntimeIntrinsic::MapEntriesCursorNew(key_type, value_type)
        | RuntimeIntrinsic::MapKeysCursorNew(key_type, value_type)
        | RuntimeIntrinsic::MapValuesCursorNew(key_type, value_type) => {
            let CompiledValue::Map { pointer: map, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let Type::Map(map_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let cursor_type = match intrinsic {
                RuntimeIntrinsic::MapEntriesCursorNew(_, _) => Type::MapCursor(map_id),
                RuntimeIntrinsic::MapKeysCursorNew(_, _) => Type::MapKeyCursor(map_id),
                _ => Type::MapValueCursor(map_id),
            };
            // The runtime retains an independent map reference; release the
            // borrowed receiver reference afterwards.
            let call = builder.ins().call(context.refs.map_cursor_new, &[map]);
            builder.ins().call(managed_drop_ref, &[map]);
            let _ = (key_type, value_type);
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: cursor_type,
                },
            );
        }
        RuntimeIntrinsic::MapCursorAdvance(key_type, value_type) => {
            let CompiledValue::NativeHandle {
                pointer: cursor,
                ty: receiver_type,
            } = values.get(&arguments[0].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR map cursor receiver was not compiled".to_owned(),
                }
            })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR map cursor receiver has invalid type".to_owned(),
                });
            };
            let key_words = abi_types(*key_type, pointer_type, types).len().max(1);
            let value_words = abi_types(*value_type, pointer_type, types).len().max(1);
            let (key_slot, key_output) = create_list_word_slot(builder, pointer_type, key_words)?;
            let (value_slot, value_output) =
                create_list_word_slot(builder, pointer_type, value_words)?;
            // Entry 0, key 1, value 2; the runtime only clones the requested
            // half, so the skipped projection never leaks a reference.
            let (projection, item_type) = match receiver_type {
                Type::MapCursor(_) => (0_i64, None),
                Type::MapKeyCursor(_) => (1_i64, Some(*key_type)),
                Type::MapValueCursor(_) => (2_i64, Some(*value_type)),
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "MIR map cursor advance has an invalid receiver".to_owned(),
                    })
                }
            };
            let projection_value = builder.ins().iconst(pointer_type, projection);
            let key_count = builder.ins().iconst(pointer_type, key_words as i64);
            let value_count = builder.ins().iconst(pointer_type, value_words as i64);
            // The runtime adopts the receiver reference: on exhaustion it is
            // released inside the call, otherwise the successor carries it.
            let call = builder.ins().call(
                context.refs.map_cursor_step,
                &[
                    cursor,
                    projection_value,
                    key_output,
                    key_count,
                    value_output,
                    value_count,
                ],
            );
            let next = builder.inst_results(call)[0];
            let exhausted =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, next, 0);
            let tag = builder.ins().uextend(types::I32, exhausted);
            fn read_words(
                builder: &mut FunctionBuilder<'_>,
                pointer_type: cranelift_codegen::ir::Type,
                slot: cranelift_codegen::ir::StackSlot,
                count: usize,
            ) -> Vec<cranelift_codegen::ir::Value> {
                (0..count)
                    .map(|index| {
                        builder
                            .ins()
                            .stack_load(pointer_type, types::I64, slot, (index * 8) as i32)
                    })
                    .collect()
            }
            let successor = CompiledValue::NativeHandle {
                pointer: next,
                ty: receiver_type,
            };
            let item = match item_type {
                Some(ty) => {
                    let (slot, count) = if projection == 1 {
                        (key_slot, key_words)
                    } else {
                        (value_slot, value_words)
                    };
                    let words = read_words(builder, pointer_type, slot, count);
                    list_value_from_words(builder, &words, ty, types)?
                }
                None => {
                    let key_words_values = read_words(builder, pointer_type, key_slot, key_words);
                    let key = list_value_from_words(builder, &key_words_values, *key_type, types)?;
                    let value_words_values =
                        read_words(builder, pointer_type, value_slot, value_words);
                    let value =
                        list_value_from_words(builder, &value_words_values, *value_type, types)?;
                    let entry = Type::Tuple(types.tuple_id(&[*key_type, *value_type]).ok_or_else(
                        || CodegenError::RuntimeError {
                            message: "MIR map cursor entry was not interned".to_owned(),
                        },
                    )?);
                    CompiledValue::Tuple {
                        elements: vec![key, value],
                        ty: entry,
                    }
                }
            };
            let item_type = match receiver_type {
                Type::MapCursor(_) => {
                    Type::Tuple(types.tuple_id(&[*key_type, *value_type]).ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "MIR map cursor entry was not interned".to_owned(),
                        }
                    })?)
                }
                Type::MapKeyCursor(_) => *key_type,
                _ => *value_type,
            };
            let pair =
                Type::Tuple(types.tuple_id(&[item_type, receiver_type]).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MIR map cursor result was not interned".to_owned(),
                    }
                })?);
            let option =
                Type::Option(
                    types
                        .option_id(pair)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR map cursor Option was not interned".to_owned(),
                        })?,
                );
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Tuple {
                        elements: vec![item, successor],
                        ty: pair,
                    }],
                    ty: option,
                },
            );
        }
        RuntimeIntrinsic::MapContainsKey(key_type) => {
            let CompiledValue::Map { pointer: map, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR Map key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_count = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *key_type, environment)?;
            let call = builder.ins().call(
                context.refs.map_contains_key,
                &[map, key_pointer, key_count, key_kind],
            );
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
            drop_map_key(builder, key, managed_drop_ref);
            builder.ins().call(managed_drop_ref, &[map]);
        }
        RuntimeIntrinsic::MapLength => {
            let CompiledValue::Map { pointer: map, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.map_length, &[map]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
            builder.ins().call(managed_drop_ref, &[map]);
        }
        RuntimeIntrinsic::MapIsEmpty => {
            let CompiledValue::Map { pointer: map, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR Map receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR Map receiver has invalid type".to_owned(),
                });
            };
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.ins().icmp_imm_u(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        map,
                        0,
                    ),
                },
            );
            builder.ins().call(managed_drop_ref, &[map]);
        }
        RuntimeIntrinsic::MutMapIntoIter(key, value) => {
            let CompiledValue::MutMap { pointer: map, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR MutMap receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutMap receiver has invalid type".to_owned(),
                });
            };
            let Type::MutMap(map_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutMap receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.mut_map_cursor_new, &[map]);
            let _ = (key, value);
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutMapCursor(map_id),
                },
            );
        }
        RuntimeIntrinsic::MutSetIntoIter(element) => {
            let CompiledValue::MutSet { pointer: set, ty } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR MutSet receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutSet receiver has invalid type".to_owned(),
                });
            };
            let Type::MutSet(map_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutSet receiver has invalid type".to_owned(),
                });
            };
            let call = builder.ins().call(context.refs.mut_map_cursor_new, &[set]);
            let _ = element;
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutSetCursor(map_id),
                },
            );
        }
        RuntimeIntrinsic::MutMapToList(key, value) => {
            let CompiledValue::MutMap { pointer: map, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR MutMap receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutMap receiver has invalid type".to_owned(),
                });
            };
            let keys_only = builder.ins().iconst(types::I8, 0);
            let call = builder
                .ins()
                .call(context.refs.mut_map_to_list, &[map, keys_only]);
            let entry = Type::Tuple(types.tuple_id(&[*key, *value]).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutMap to_list entry was not interned".to_owned(),
                }
            })?);
            let list_id = types
                .list_id(entry)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutMap to_list List was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::List(list_id),
                },
            );
        }
        RuntimeIntrinsic::MutSetToList(element) => {
            let CompiledValue::MutSet { pointer: set, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR MutSet receiver was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR MutSet receiver has invalid type".to_owned(),
                });
            };
            let keys_only = builder.ins().iconst(types::I8, 1);
            let call = builder
                .ins()
                .call(context.refs.mut_map_to_list, &[set, keys_only]);
            let list_id = types
                .list_id(*element)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutSet to_list List was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::List {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::List(list_id),
                },
            );
        }
        RuntimeIntrinsic::MutMapCursorAdvance(key_type, _)
        | RuntimeIntrinsic::MutSetCursorAdvance(key_type) => {
            let CompiledValue::NativeHandle {
                pointer: cursor,
                ty: receiver_type,
            } = values.get(&arguments[0].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR mut map cursor receiver was not compiled".to_owned(),
                }
            })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "MIR mut map cursor receiver has invalid type".to_owned(),
                });
            };
            let value_type = match intrinsic {
                RuntimeIntrinsic::MutMapCursorAdvance(_, value) => *value,
                _ => Type::Bool,
            };
            let key_words = abi_types(*key_type, pointer_type, types).len().max(1);
            let value_words = abi_types(value_type, pointer_type, types).len().max(1);
            let (key_slot, key_output) = create_list_word_slot(builder, pointer_type, key_words)?;
            let (value_slot, value_output) =
                create_list_word_slot(builder, pointer_type, value_words)?;
            let projection = if matches!(intrinsic, RuntimeIntrinsic::MutSetCursorAdvance(_)) {
                1_i64
            } else {
                0_i64
            };
            let projection_value = builder.ins().iconst(pointer_type, projection);
            let key_count = builder.ins().iconst(pointer_type, key_words as i64);
            let value_count = builder.ins().iconst(pointer_type, value_words as i64);
            let call = builder.ins().call(
                context.refs.mut_map_cursor_step,
                &[
                    cursor,
                    projection_value,
                    key_output,
                    key_count,
                    value_output,
                    value_count,
                ],
            );
            let next = builder.inst_results(call)[0];
            let exhausted =
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, next, 0);
            let tag = builder.ins().uextend(types::I32, exhausted);
            fn read_words(
                builder: &mut FunctionBuilder<'_>,
                pointer_type: cranelift_codegen::ir::Type,
                slot: cranelift_codegen::ir::StackSlot,
                count: usize,
            ) -> Vec<cranelift_codegen::ir::Value> {
                (0..count)
                    .map(|index| {
                        builder
                            .ins()
                            .stack_load(pointer_type, types::I64, slot, (index * 8) as i32)
                    })
                    .collect()
            }
            let successor = CompiledValue::NativeHandle {
                pointer: next,
                ty: receiver_type,
            };
            let item = if projection == 1 {
                let words = read_words(builder, pointer_type, key_slot, key_words);
                list_value_from_words(builder, &words, *key_type, types)?
            } else {
                let key_words_values = read_words(builder, pointer_type, key_slot, key_words);
                let key = list_value_from_words(builder, &key_words_values, *key_type, types)?;
                let value_words_values = read_words(builder, pointer_type, value_slot, value_words);
                let value = list_value_from_words(builder, &value_words_values, value_type, types)?;
                let entry =
                    Type::Tuple(types.tuple_id(&[*key_type, value_type]).ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "MIR mut map cursor entry was not interned".to_owned(),
                        }
                    })?);
                CompiledValue::Tuple {
                    elements: vec![key, value],
                    ty: entry,
                }
            };
            let item_type = if projection == 1 {
                *key_type
            } else {
                Type::Tuple(types.tuple_id(&[*key_type, value_type]).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MIR mut map cursor entry was not interned".to_owned(),
                    }
                })?)
            };
            let pair =
                Type::Tuple(types.tuple_id(&[item_type, receiver_type]).ok_or_else(|| {
                    CodegenError::RuntimeError {
                        message: "MIR mut map cursor result was not interned".to_owned(),
                    }
                })?);
            let option =
                Type::Option(
                    types
                        .option_id(pair)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "MIR mut map cursor Option was not interned".to_owned(),
                        })?,
                );
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![CompiledValue::Tuple {
                        elements: vec![item, successor],
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
