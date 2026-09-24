//! Runtime intrinsic lowering for MIR statements.

mod batch;
mod bytes;
mod c_memory;
mod callbacks;
mod cowns;
mod equality;
mod helpers;
mod list;
mod map;
mod mut_list;
mod ordering;
mod path;
mod strings;

use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::environment::{CompiledValue, Environment};
use super::statements::RuntimeCallRefs;
use super::values::*;
use helpers::{combine_masks, drop_map_key};
pub(super) use helpers::{create_list_word_slot, map_key_descriptor, store_list_words};

pub(super) struct RuntimeCallContext<'a> {
    pub(super) types: &'a crate::sema::TypeTable,
    pub(super) pointer_type: cranelift_codegen::ir::Type,
    pub(super) refs: RuntimeCallRefs,
}

pub(super) fn compile_runtime_call(
    builder: &mut FunctionBuilder<'_>,
    context: &mut RuntimeCallContext<'_>,
    destination: &MirValueId,
    intrinsic: &RuntimeIntrinsic,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
    _environment: &mut Environment,
) -> Result<(), CodegenError> {
    if let Some(symbol) = intrinsic.runtime_symbol() {
        debug_assert!(
            joky_runtime_abi::symbols::ALL.contains(&symbol),
            "runtime intrinsic symbol {symbol} is missing from the ABI table"
        );
    }
    if let RuntimeIntrinsic::Path(op) = intrinsic {
        return path::compile(builder, context, *destination, *op, arguments, values);
    }
    match intrinsic {
        RuntimeIntrinsic::HasherNew => {
            let call = builder.ins().call(context.refs.hasher_new, &[]);
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::Hasher,
                },
            );
            return Ok(());
        }
        RuntimeIntrinsic::HasherFinish => {
            let CompiledValue::NativeHandle { pointer, .. } = values[&arguments[0].value] else {
                unreachable!()
            };
            let call = builder.ins().call(context.refs.hasher_finish, &[pointer]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
            return Ok(());
        }
        RuntimeIntrinsic::Hash(_) => {
            let CompiledValue::NativeHandle { pointer, .. } = values[&arguments[1].value] else {
                unreachable!()
            };
            crate::codegen::hashing::emit_hash(
                builder,
                values[&arguments[0].value].clone(),
                pointer,
                crate::codegen::hashing::HashRefs {
                    word: context.refs.hasher_word,
                    string: context.refs.hasher_string,
                    bytes: context.refs.hasher_bytes,
                },
            )?;
            drop_map_key(
                builder,
                values[&arguments[0].value].clone(),
                context.refs.managed_drop,
            );
            values.insert(*destination, CompiledValue::Unit);
            return Ok(());
        }
        _ => {}
    }
    if let RuntimeIntrinsic::PartialEqual(_) = intrinsic {
        let value = equality::compile_equal(
            builder,
            context,
            values[&arguments[0].value].clone(),
            values[&arguments[1].value].clone(),
        )?;
        values.insert(*destination, CompiledValue::Boolean { value });
        return Ok(());
    }
    if let RuntimeIntrinsic::PartialCompare(_) | RuntimeIntrinsic::Compare(_) = intrinsic {
        let value = ordering::compile_compare(
            builder,
            context,
            values[&arguments[0].value].clone(),
            values[&arguments[1].value].clone(),
            matches!(intrinsic, RuntimeIntrinsic::PartialCompare(_)),
        )?;
        values.insert(*destination, value);
        return Ok(());
    }
    let types = context.types;
    let pointer_type = context.pointer_type;
    if callbacks::compile_callback_call(
        builder,
        context,
        destination,
        intrinsic,
        arguments,
        values,
        _environment,
    )? {
        return Ok(());
    }
    if batch::compile_batch_call(builder, context, destination, intrinsic, arguments, values)? {
        return Ok(());
    }
    if bytes::compile_bytes_call(builder, context, destination, intrinsic, arguments, values)? {
        return Ok(());
    }
    if strings::compile_string_call(builder, context, destination, intrinsic, arguments, values)? {
        return Ok(());
    }
    if c_memory::compile_c_memory_call(builder, context, destination, intrinsic, arguments, values)?
    {
        return Ok(());
    }
    if cowns::compile_cown_call(
        builder,
        context,
        destination,
        intrinsic,
        arguments,
        values,
        _environment,
    )? {
        return Ok(());
    }
    if list::compile_list_call(builder, context, destination, intrinsic, arguments, values)? {
        return Ok(());
    }
    if mut_list::compile_mut_list_call(builder, context, destination, intrinsic, arguments, values)?
    {
        return Ok(());
    }
    if map::compile_map_call(
        builder,
        context,
        destination,
        intrinsic,
        arguments,
        values,
        _environment,
    )? {
        return Ok(());
    }
    let RuntimeCallRefs {
        panic: panic_ref,
        managed_drop: managed_drop_ref,
        mut_map_new: mut_map_new_ref,
        mut_map_insert: mut_map_insert_ref,
        mut_map_get: mut_map_get_ref,
        mut_map_remove: mut_map_remove_ref,
        mut_map_contains_key: mut_map_contains_key_ref,
        mut_map_length: mut_map_length_ref,
        mut_map_capacity: mut_map_capacity_ref,
        mut_map_is_empty: mut_map_is_empty_ref,
        ..
    } = context.refs;
    match intrinsic {
        RuntimeIntrinsic::CPointerNull(ty) => {
            let pointer = builder.ins().iconst(pointer_type, 0);
            values.insert(
                *destination,
                CompiledValue::NativeHandle { pointer, ty: *ty },
            );
        }
        RuntimeIntrinsic::CPointerIsNull => {
            let Some(CompiledValue::NativeHandle { pointer, ty }) = values.get(&arguments[0].value)
            else {
                return Err(CodegenError::RuntimeError {
                    message: "null check requires a C pointer".into(),
                });
            };
            if !ty.is_c_pointer() {
                return Err(CodegenError::RuntimeError {
                    message: "null check requires a C pointer".into(),
                });
            }
            let value = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                *pointer,
                0,
            );
            values.insert(*destination, CompiledValue::Boolean { value });
        }
        RuntimeIntrinsic::MutMapNew(key, value) => {
            let key_words = super::super::abi::abi_types(*key, pointer_type, types)
                .len()
                .max(1);
            let value_words = super::super::abi::abi_types(*value, pointer_type, types)
                .len()
                .max(1);
            let key_masks = key_words.div_ceil(64);
            let value_masks = value_words.div_ceil(64);
            let key_kind = map_key_descriptor(builder, pointer_type, *key, _environment)?;
            let key_words_value = builder.ins().iconst(pointer_type, key_words as i64);
            let key_masks_value = builder.ins().iconst(pointer_type, key_masks as i64);
            let value_words_value = builder.ins().iconst(pointer_type, value_words as i64);
            let value_masks_value = builder.ins().iconst(pointer_type, value_masks as i64);
            let call = builder.ins().call(
                mut_map_new_ref,
                &[
                    key_words_value,
                    key_masks_value,
                    value_words_value,
                    value_masks_value,
                    key_kind,
                ],
            );
            let map_id = types
                .map_id(*key, *value)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MutMap type was not interned".to_owned(),
                })?;
            values.insert(
                *destination,
                CompiledValue::MutMap {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutMap(map_id),
                },
            );
        }
        RuntimeIntrinsic::MutMapInsert(_key_type, value_type) => {
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
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutMap key was not compiled".to_owned(),
                }
            })?;
            let value = values.get(&arguments[2].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutMap value was not compiled".to_owned(),
                }
            })?;
            let (key_words, key_masks) = list_element_words(builder, key)?;
            let (value_words, value_masks) = list_element_words(builder, value)?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_mask_values = key_masks
                .iter()
                .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                .collect::<Vec<_>>();
            let key_masks_pointer = store_list_words(builder, pointer_type, &key_mask_values)?;
            let value_pointer = store_list_words(builder, pointer_type, &value_words)?;
            let value_mask_values = value_masks
                .iter()
                .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                .collect::<Vec<_>>();
            let value_masks_pointer = store_list_words(builder, pointer_type, &value_mask_values)?;
            let value_count = value_words.len();
            let (slot, output) = create_list_word_slot(builder, pointer_type, value_count)?;
            let key_count_value = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_masks_count_value = builder.ins().iconst(pointer_type, key_masks.len() as i64);
            let value_count_value = builder.ins().iconst(pointer_type, value_count as i64);
            let value_masks_count_value =
                builder.ins().iconst(pointer_type, value_masks.len() as i64);
            let call = builder.ins().call(
                mut_map_insert_ref,
                &[
                    map,
                    key_pointer,
                    key_count_value,
                    key_masks_pointer,
                    key_masks_count_value,
                    value_pointer,
                    value_count_value,
                    value_masks_pointer,
                    value_masks_count_value,
                    output,
                    value_count_value,
                ],
            );
            let success = builder.inst_results(call)[0];
            let is_none = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder.ins().uextend(types::I32, is_none);
            let words = (0..value_count)
                .map(|index| {
                    builder
                        .ins()
                        .stack_load(pointer_type, types::I64, slot, (index * 8) as i32)
                })
                .collect::<Vec<_>>();
            let old = list_value_from_words(builder, &words, *value_type, types)?;
            let option = Type::Option(types.option_id(*value_type).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutMap insert Option was not interned".to_owned(),
                }
            })?);
            values.insert(
                *destination,
                CompiledValue::Enum {
                    tag,
                    fields: vec![old],
                    ty: option,
                },
            );
            let _ = ty;
        }
        RuntimeIntrinsic::MutMapGet(key_type, value_type)
        | RuntimeIntrinsic::MutMapRemove(key_type, value_type) => {
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
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutMap key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let value_count = super::super::abi::abi_types(*value_type, pointer_type, types)
                .len()
                .max(1);
            let (slot, output) = create_list_word_slot(builder, pointer_type, value_count)?;
            let key_kind = map_key_descriptor(builder, pointer_type, *key_type, _environment)?;
            let key_count_value = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let value_count_value = builder.ins().iconst(pointer_type, value_count as i64);
            let call = if matches!(intrinsic, RuntimeIntrinsic::MutMapGet(_, _)) {
                builder.ins().call(
                    mut_map_get_ref,
                    &[
                        map,
                        key_pointer,
                        key_count_value,
                        key_kind,
                        output,
                        value_count_value,
                    ],
                )
            } else {
                builder.ins().call(
                    mut_map_remove_ref,
                    &[
                        map,
                        key_pointer,
                        key_count_value,
                        key_kind,
                        output,
                        value_count_value,
                    ],
                )
            };
            let success = builder.inst_results(call)[0];
            let is_none = builder.ins().icmp_imm_u(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                success,
                0,
            );
            let tag = builder.ins().uextend(types::I32, is_none);
            let words = (0..value_count)
                .map(|index| {
                    builder
                        .ins()
                        .stack_load(pointer_type, types::I64, slot, (index * 8) as i32)
                })
                .collect::<Vec<_>>();
            let value = list_value_from_words(builder, &words, *value_type, types)?;
            let option = Type::Option(types.option_id(*value_type).ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MutMap Option was not interned".to_owned(),
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
        }
        RuntimeIntrinsic::MutMapContainsKey(key_type) => {
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
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutMap key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_count_value = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_kind_value =
                map_key_descriptor(builder, pointer_type, *key_type, _environment)?;
            let call = builder.ins().call(
                mut_map_contains_key_ref,
                &[map, key_pointer, key_count_value, key_kind_value],
            );
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
            drop_map_key(builder, key, managed_drop_ref);
        }
        RuntimeIntrinsic::MutMapLength | RuntimeIntrinsic::MutMapCapacity => {
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
            let reference = if matches!(intrinsic, RuntimeIntrinsic::MutMapLength) {
                mut_map_length_ref
            } else {
                mut_map_capacity_ref
            };
            let call = builder.ins().call(reference, &[map]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::MutMapIsEmpty => {
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
            let call = builder.ins().call(mut_map_is_empty_ref, &[map]);
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::MutSetNew(element) => {
            let key_words = super::super::abi::abi_types(*element, pointer_type, types)
                .len()
                .max(1);
            let key_masks = key_words.div_ceil(64);
            let key_kind = map_key_descriptor(builder, pointer_type, *element, _environment)?;
            let args = [
                builder.ins().iconst(pointer_type, key_words as i64),
                builder.ins().iconst(pointer_type, key_masks as i64),
                builder.ins().iconst(pointer_type, 1),
                builder.ins().iconst(pointer_type, 1),
                key_kind,
            ];
            let call = builder.ins().call(mut_map_new_ref, &args);
            let map_id =
                types
                    .map_id(*element, Type::Bool)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "MutSet type was not interned".to_owned(),
                    })?;
            values.insert(
                *destination,
                CompiledValue::MutSet {
                    pointer: builder.inst_results(call)[0],
                    ty: Type::MutSet(map_id),
                },
            );
        }
        RuntimeIntrinsic::MutSetAdd(element) | RuntimeIntrinsic::MutSetRemove(element) => {
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
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutSet key was not compiled".to_owned(),
                }
            })?;
            let (key_words, key_masks) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_mask_values = key_masks
                .iter()
                .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                .collect::<Vec<_>>();
            let key_masks_pointer = store_list_words(builder, pointer_type, &key_mask_values)?;
            let key_count = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_masks_count = builder.ins().iconst(pointer_type, key_masks.len() as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *element, _environment)?;
            let (slot, output) = create_list_word_slot(builder, pointer_type, 1)?;
            let output_count = builder.ins().iconst(pointer_type, 1);
            let call = if matches!(intrinsic, RuntimeIntrinsic::MutSetAdd(_)) {
                let value = builder.ins().iconst(types::I64, 0);
                let value_pointer = store_list_words(builder, pointer_type, &[value])?;
                let value_mask = builder.ins().iconst(types::I64, 0);
                let value_masks_pointer = store_list_words(builder, pointer_type, &[value_mask])?;
                builder.ins().call(
                    mut_map_insert_ref,
                    &[
                        set,
                        key_pointer,
                        key_count,
                        key_masks_pointer,
                        key_masks_count,
                        value_pointer,
                        output_count,
                        value_masks_pointer,
                        output_count,
                        output,
                        output_count,
                    ],
                )
            } else {
                builder.ins().call(
                    mut_map_remove_ref,
                    &[set, key_pointer, key_count, key_kind, output, output_count],
                )
            };
            let result = builder.inst_results(call)[0];
            let result = if matches!(intrinsic, RuntimeIntrinsic::MutSetAdd(_)) {
                builder
                    .ins()
                    .icmp_imm_u(cranelift_codegen::ir::condcodes::IntCC::Equal, result, 0)
            } else {
                result
            };
            values.insert(*destination, CompiledValue::Boolean { value: result });
            let _ = slot;
            if matches!(intrinsic, RuntimeIntrinsic::MutSetRemove(_)) {
                drop_map_key(builder, key, managed_drop_ref);
            }
        }
        RuntimeIntrinsic::MutSetContains(element) => {
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
            let key = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR MutSet key was not compiled".to_owned(),
                }
            })?;
            let (key_words, _) = list_element_words(builder, key.clone())?;
            let key_pointer = store_list_words(builder, pointer_type, &key_words)?;
            let key_count = builder.ins().iconst(pointer_type, key_words.len() as i64);
            let key_kind = map_key_descriptor(builder, pointer_type, *element, _environment)?;
            let call = builder.ins().call(
                mut_map_contains_key_ref,
                &[set, key_pointer, key_count, key_kind],
            );
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
            drop_map_key(builder, key, managed_drop_ref);
        }
        RuntimeIntrinsic::MutSetLength | RuntimeIntrinsic::MutSetCapacity => {
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
            let reference = if matches!(intrinsic, RuntimeIntrinsic::MutSetLength) {
                mut_map_length_ref
            } else {
                mut_map_capacity_ref
            };
            let call = builder.ins().call(reference, &[set]);
            values.insert(
                *destination,
                CompiledValue::Numeric {
                    value: builder.inst_results(call)[0],
                    ty: Type::U64,
                },
            );
        }
        RuntimeIntrinsic::MutSetIsEmpty => {
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
            let call = builder.ins().call(mut_map_is_empty_ref, &[set]);
            values.insert(
                *destination,
                CompiledValue::Boolean {
                    value: builder.inst_results(call)[0],
                },
            );
        }
        RuntimeIntrinsic::Panic => {
            if arguments.len() != 1 {
                return Err(CodegenError::RuntimeError {
                    message: "panic expects one String argument".to_owned(),
                });
            }
            let CompiledValue::String { pointer, length } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "panic message was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "panic expects a String argument".to_owned(),
                });
            };
            builder.ins().call(panic_ref, &[pointer, length]);
            values.insert(*destination, CompiledValue::Unit);
        }
        _ => unreachable!("byte runtime intrinsic should be handled by compile_bytes_call"),
    }
    Ok(())
}
