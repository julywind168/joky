use super::*;
use crate::codegen::types::cranelift_type;
use cranelift_codegen::ir::{StackSlotData, StackSlotKind};

pub(in crate::codegen::functions) fn list_element_words(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
) -> Result<(Vec<cranelift_codegen::ir::Value>, Vec<u64>), CodegenError> {
    let mut words = Vec::new();
    let mut managed = Vec::new();
    append_list_element_words(builder, value, &mut words, &mut managed)?;
    if words.is_empty() {
        words.push(builder.ins().iconst(types::I64, 0));
        managed.push(false);
    }
    let mut masks = vec![0_u64; words.len().div_ceil(64)];
    for (index, is_managed) in managed.into_iter().enumerate() {
        if is_managed {
            masks[index / 64] |= 1_u64 << (index % 64);
        }
    }
    Ok((words, masks))
}

fn append_list_element_words(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    words: &mut Vec<cranelift_codegen::ir::Value>,
    managed: &mut Vec<bool>,
) -> Result<(), CodegenError> {
    match value {
        CompiledValue::Unit => {}
        CompiledValue::Numeric { value, ty } if ty.is_integer() || ty == Type::Duration => {
            let source = cranelift_type(ty).map_err(|_| CodegenError::RuntimeError {
                message: "List integer element has no runtime representation".to_owned(),
            })?;
            let word = if source == types::I64 {
                value
            } else {
                builder.ins().uextend(types::I64, value)
            };
            words.push(word);
            managed.push(false);
        }
        CompiledValue::Numeric {
            value,
            ty: Type::F64,
        } => {
            words.push(builder.ins().bitcast(
                types::I64,
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
            ));
            managed.push(false);
        }
        CompiledValue::Numeric {
            value,
            ty: Type::F32,
        } => {
            let bits = builder.ins().bitcast(
                types::I32,
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
            );
            words.push(builder.ins().uextend(types::I64, bits));
            managed.push(false);
        }
        CompiledValue::Boolean { value } => {
            words.push(builder.ins().uextend(types::I64, value));
            managed.push(false);
        }
        CompiledValue::String { pointer, length } => {
            words.extend([pointer, length]);
            managed.extend([true, false]);
        }
        CompiledValue::Bytes { pointer } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::NativeHandle { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "List elements cannot own native handles".to_owned(),
            })
        }
        CompiledValue::Tuple { elements, .. } => {
            for element in elements {
                append_list_element_words(builder, element, words, managed)?;
            }
        }
        CompiledValue::Struct { fields, .. } => {
            for field in fields {
                append_list_element_words(builder, field, words, managed)?;
            }
        }
        CompiledValue::Enum { tag, fields, .. } => {
            words.push(builder.ins().uextend(types::I64, tag));
            managed.push(false);
            for field in fields {
                append_list_element_words(builder, field, words, managed)?;
            }
        }
        CompiledValue::List { pointer, .. } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::Map { pointer, .. } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::Cown { pointer, .. } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::MutMap { pointer, .. } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::MutSet { pointer, .. } => {
            words.push(pointer);
            managed.push(true);
        }
        CompiledValue::MutBytes { .. }
        | CompiledValue::MutList { .. }
        | CompiledValue::Class { .. }
        | CompiledValue::Function { .. }
        | CompiledValue::Numeric { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "List element has no runtime representation".to_owned(),
            })
        }
    }
    Ok(())
}

pub(in crate::codegen::functions) fn list_value_from_words(
    builder: &mut FunctionBuilder<'_>,
    words: &[cranelift_codegen::ir::Value],
    ty: Type,
    type_table: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let mut cursor = 0;
    let value = decode_list_element_words(builder, words, &mut cursor, ty, type_table)?;
    if cursor != words.len() && !(cursor == 0 && words.len() == 1) {
        return Err(CodegenError::RuntimeError {
            message: "List element payload has trailing words".to_owned(),
        });
    }
    Ok(value)
}

pub(in crate::codegen::functions) fn store_constant_words(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: cranelift_codegen::ir::Type,
    words: &[cranelift_codegen::ir::Value],
) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    let size = words
        .len()
        .checked_mul(std::mem::size_of::<u64>())
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "constant collection representation is too large".to_owned(),
        })?;
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        size.max(8),
        3,
    ));
    let pointer = builder.ins().stack_addr(pointer_type, slot, 0);
    for (index, word) in words.iter().enumerate() {
        builder
            .ins()
            .stack_store(pointer_type, *word, slot, (index * 8) as i32);
    }
    Ok(pointer)
}

fn decode_list_element_words(
    builder: &mut FunctionBuilder<'_>,
    words: &[cranelift_codegen::ir::Value],
    cursor: &mut usize,
    ty: Type,
    type_table: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let mut next_word = || {
        let word = words
            .get(*cursor)
            .copied()
            .ok_or_else(|| CodegenError::RuntimeError {
                message: "List element payload is truncated".to_owned(),
            })?;
        *cursor += 1;
        Ok(word)
    };
    Ok(match ty {
        Type::CPtr(_) | Type::CMutPtr(_) | Type::CArray(_) | Type::CStr => {
            return Err(CodegenError::RuntimeError {
                message: "C ABI values cannot be decoded from collection payloads".to_owned(),
            })
        }
        Type::Unit => CompiledValue::Unit,
        Type::F64 => CompiledValue::Numeric {
            value: builder.ins().bitcast(
                types::F64,
                cranelift_codegen::ir::MemFlagsData::new(),
                next_word()?,
            ),
            ty,
        },
        Type::F32 => {
            let word = next_word()?;
            let bits = builder.ins().ireduce(types::I32, word);
            CompiledValue::Numeric {
                value: builder.ins().bitcast(
                    types::F32,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    bits,
                ),
                ty,
            }
        }
        Type::Bool => CompiledValue::Boolean {
            value: builder.ins().ireduce(types::I8, next_word()?),
        },
        Type::String => CompiledValue::String {
            pointer: next_word()?,
            length: next_word()?,
        },
        Type::Bytes => CompiledValue::Bytes {
            pointer: next_word()?,
        },
        Type::Batch
        | Type::SeqBuilder
        | Type::BytesCursor
        | Type::MapCursor(_)
        | Type::MapKeyCursor(_)
        | Type::MapValueCursor(_)
        | Type::MutListCursor(_)
        | Type::MutMapCursor(_)
        | Type::MutSetCursor(_)
        | Type::Hasher
        | Type::Native(_)
        | Type::CCallback => CompiledValue::NativeHandle {
            pointer: next_word()?,
            ty,
        },
        ty @ (Type::I8 | Type::I16 | Type::I32 | Type::U8 | Type::U16 | Type::U32) => {
            CompiledValue::Numeric {
                value: builder.ins().ireduce(
                    cranelift_type(ty).map_err(|_| CodegenError::RuntimeError {
                        message: "List integer element has no runtime representation".to_owned(),
                    })?,
                    next_word()?,
                ),
                ty,
            }
        }
        Type::I64 | Type::U64 | Type::Duration => CompiledValue::Numeric {
            value: next_word()?,
            ty,
        },
        Type::Tuple(id) => {
            let mut elements = Vec::new();
            for element in type_table.tuple_elements(id) {
                elements.push(decode_list_element_words(
                    builder, words, cursor, *element, type_table,
                )?);
            }
            CompiledValue::Tuple { elements, ty }
        }
        Type::Struct(id) => {
            let mut fields = Vec::new();
            for (_, field) in type_table.struct_fields(id) {
                fields.push(decode_list_element_words(
                    builder, words, cursor, *field, type_table,
                )?);
            }
            CompiledValue::Struct { fields, ty }
        }
        Type::Enum(id) => {
            let tag = builder.ins().ireduce(types::I32, next_word()?);
            let mut fields = Vec::new();
            for variant in type_table.enum_variants(id) {
                for (_, field) in &variant.fields {
                    fields.push(decode_list_element_words(
                        builder, words, cursor, *field, type_table,
                    )?);
                }
            }
            CompiledValue::Enum { tag, fields, ty }
        }
        Type::Option(id) => {
            let tag = builder.ins().ireduce(types::I32, next_word()?);
            let value = decode_list_element_words(
                builder,
                words,
                cursor,
                type_table.option_type(id),
                type_table,
            )?;
            CompiledValue::Enum {
                tag,
                fields: vec![value],
                ty,
            }
        }
        Type::Result(id) => {
            let tag = builder.ins().ireduce(types::I32, next_word()?);
            let (ok, err) = type_table.result_types(id);
            let ok = decode_list_element_words(builder, words, cursor, ok, type_table)?;
            let err = decode_list_element_words(builder, words, cursor, err, type_table)?;
            CompiledValue::Enum {
                tag,
                fields: vec![ok, err],
                ty,
            }
        }
        Type::List(_) => CompiledValue::List {
            pointer: next_word()?,
            ty,
        },
        Type::Map(_) => CompiledValue::Map {
            pointer: next_word()?,
            ty,
        },
        Type::MutList(_) => CompiledValue::MutList {
            pointer: next_word()?,
            ty,
        },
        Type::MutMap(_) => CompiledValue::MutMap {
            pointer: next_word()?,
            ty,
        },
        Type::MutSet(_) => CompiledValue::MutSet {
            pointer: next_word()?,
            ty,
        },
        Type::Cown(_) => CompiledValue::Cown {
            pointer: next_word()?,
            ty,
        },
        Type::MutBytes
        | Type::Class(_)
        | Type::Param(_)
        | Type::SelfType
        | Type::Associated(_)
        | Type::Dyn(_)
        | Type::Function(_) => {
            return Err(CodegenError::RuntimeError {
                message: "List element has no runtime representation".to_owned(),
            })
        }
    })
}
