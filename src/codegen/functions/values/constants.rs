use super::*;
use crate::codegen::types::cranelift_type;
use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::functions) fn compile_mir_constant(
    builder: &mut FunctionBuilder<'_>,
    constant: &MirConstant,
    ty: crate::sema::Type,
    pointer_type: cranelift_codegen::ir::Type,
    type_table: &TypeTable,
    strings: &mut dyn Iterator<Item = (super::super::context::StringValue, usize)>,
    string_from_ref: cranelift_codegen::ir::FuncRef,
    list_cons_ref: cranelift_codegen::ir::FuncRef,
    map_insert_ref: cranelift_codegen::ir::FuncRef,
    allocate_ref: cranelift_codegen::ir::FuncRef,
    environment: &Environment,
) -> Result<CompiledValue, CodegenError> {
    match constant {
        MirConstant::EffectOperation(operation) => {
            let encoded = ((operation.effect.0 as u64) << 32) | operation.operation as u64;
            Ok(CompiledValue::Numeric {
                value: builder.ins().iconst(types::I64, encoded as i64),
                ty,
            })
        }
        MirConstant::Integer(value) => {
            let numeric_type = cranelift_type(ty).map_err(|error| CodegenError::RuntimeError {
                message: format!("invalid MIR integer constant type: {error:?}"),
            })?;
            Ok(CompiledValue::Numeric {
                value: builder.ins().iconst(numeric_type, *value as i64),
                ty,
            })
        }
        MirConstant::Float(value) => {
            let value = match ty {
                crate::sema::Type::F32 => builder.ins().f32const(Ieee32::with_float(*value as f32)),
                crate::sema::Type::F64 => builder.ins().f64const(Ieee64::with_float(*value)),
                _ => {
                    return Err(CodegenError::RuntimeError {
                        message: "float MIR constant has a non-float type".to_owned(),
                    })
                }
            };
            Ok(CompiledValue::Numeric { value, ty })
        }
        MirConstant::String(_) => {
            let (value, length) = strings.next().ok_or_else(|| CodegenError::RuntimeError {
                message: "MIR String constant has no retained literal".to_owned(),
            })?;
            let data = match value {
                super::super::context::StringValue::Pointer(pointer) => {
                    builder.ins().iconst(pointer_type, pointer as usize as i64)
                }
                super::super::context::StringValue::Global(global) => {
                    builder.ins().symbol_value(pointer_type, global)
                }
            };
            let length_value = builder.ins().iconst(pointer_type, length as i64);
            let call = builder.ins().call(string_from_ref, &[data, length_value]);
            let pointer = builder.inst_results(call)[0];
            Ok(CompiledValue::String {
                pointer,
                length: length_value,
            })
        }
        MirConstant::Boolean(value) => Ok(CompiledValue::Boolean {
            value: builder.ins().iconst(types::I8, i64::from(*value)),
        }),
        MirConstant::Option(value) => {
            let crate::sema::Type::Option(option_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Option MIR constant has a non-Option type".to_owned(),
                });
            };
            let inner_type = type_table.option_type(option_id);
            let (tag, payload) = match value.as_deref() {
                Some(value) => (
                    0,
                    compile_mir_constant(
                        builder,
                        value,
                        inner_type,
                        pointer_type,
                        type_table,
                        strings,
                        string_from_ref,
                        list_cons_ref,
                        map_insert_ref,
                        allocate_ref,
                        environment,
                    )?,
                ),
                None => (
                    1,
                    zero_compiled_value(builder, inner_type, pointer_type, type_table).map_err(
                        |error| CodegenError::RuntimeError {
                            message: error.to_string(),
                        },
                    )?,
                ),
            };
            Ok(CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, tag as i64),
                fields: vec![payload],
                ty,
            })
        }
        MirConstant::Result { is_ok, value } => {
            let crate::sema::Type::Result(result_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Result MIR constant has a non-Result type".to_owned(),
                });
            };
            let (ok_type, err_type) = type_table.result_types(result_id);
            let payload_type = if *is_ok { ok_type } else { err_type };
            let inactive_type = if *is_ok { err_type } else { ok_type };
            let payload = compile_mir_constant(
                builder,
                value,
                payload_type,
                pointer_type,
                type_table,
                strings,
                string_from_ref,
                list_cons_ref,
                map_insert_ref,
                allocate_ref,
                environment,
            )?;
            let inactive = zero_compiled_value(builder, inactive_type, pointer_type, type_table)
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
            let fields = if *is_ok {
                vec![payload, inactive]
            } else {
                vec![inactive, payload]
            };
            Ok(CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, i64::from(!*is_ok)),
                fields,
                ty,
            })
        }
        MirConstant::Tuple(values) => {
            let crate::sema::Type::Tuple(tuple_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Tuple MIR constant has a non-Tuple type".to_owned(),
                });
            };
            let element_types = type_table.tuple_elements(tuple_id);
            if values.len() != element_types.len() {
                return Err(CodegenError::RuntimeError {
                    message: "Tuple MIR constant element count mismatch".to_owned(),
                });
            }
            let elements = values
                .iter()
                .zip(element_types.iter().copied())
                .map(|(value, element_type)| {
                    compile_mir_constant(
                        builder,
                        value,
                        element_type,
                        pointer_type,
                        type_table,
                        strings,
                        string_from_ref,
                        list_cons_ref,
                        map_insert_ref,
                        allocate_ref,
                        environment,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledValue::Tuple { elements, ty })
        }
        MirConstant::Struct(values) => {
            let crate::sema::Type::Struct(struct_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Struct MIR constant has a non-Struct type".to_owned(),
                });
            };
            let fields = type_table
                .struct_fields(struct_id)
                .iter()
                .map(|(name, field_type)| {
                    let value = values
                        .iter()
                        .find(|(field_name, _)| field_name == name)
                        .map(|(_, value)| value)
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: format!("Struct MIR constant is missing field '{name}'"),
                        })?;
                    compile_mir_constant(
                        builder,
                        value,
                        *field_type,
                        pointer_type,
                        type_table,
                        strings,
                        string_from_ref,
                        list_cons_ref,
                        map_insert_ref,
                        allocate_ref,
                        environment,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            if values.len() != fields.len() {
                return Err(CodegenError::RuntimeError {
                    message: "Struct MIR constant field count mismatch".to_owned(),
                });
            }
            Ok(CompiledValue::Struct { fields, ty })
        }
        MirConstant::Class(values) => {
            let crate::sema::Type::Class(class_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Class MIR constant has a non-Class type".to_owned(),
                });
            };
            let declared = type_table.class_fields(class_id);
            if values.len() != declared.len() {
                return Err(CodegenError::RuntimeError {
                    message: "Class MIR constant field count mismatch".to_owned(),
                });
            }
            let size = class_size(class_id, pointer_type, type_table).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            let kind = builder.ins().iconst(types::I8, 2);
            let size_value = builder.ins().iconst(pointer_type, i64::from(size));
            let align = builder.ins().iconst(pointer_type, 8);
            let call = builder.ins().call(allocate_ref, &[kind, size_value, align]);
            let pointer = builder.inst_results(call)[0];
            for (name, field_type) in declared {
                let constant = values
                    .iter()
                    .find(|(field_name, _)| field_name == name)
                    .map(|(_, value)| value)
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: format!("Class MIR constant is missing field '{name}'"),
                    })?;
                let field = compile_mir_constant(
                    builder,
                    constant,
                    *field_type,
                    pointer_type,
                    type_table,
                    strings,
                    string_from_ref,
                    list_cons_ref,
                    map_insert_ref,
                    allocate_ref,
                    environment,
                )?;
                let index = declared
                    .iter()
                    .position(|(field_name, _)| field_name == name)
                    .expect("class field exists");
                store_class_field(
                    builder,
                    pointer,
                    class_id,
                    index,
                    field,
                    *field_type,
                    pointer_type,
                    type_table,
                )
                .map_err(|error| CodegenError::RuntimeError {
                    message: error.to_string(),
                })?;
            }
            Ok(CompiledValue::Class { pointer, ty })
        }
        MirConstant::List(values) => {
            let crate::sema::Type::List(list_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "List MIR constant has a non-List type".to_owned(),
                });
            };
            let element_type = type_table.list_type(list_id);
            let mut list = builder.ins().iconst(pointer_type, 0);
            for value in values.iter().rev() {
                let value = compile_mir_constant(
                    builder,
                    value,
                    element_type,
                    pointer_type,
                    type_table,
                    strings,
                    string_from_ref,
                    list_cons_ref,
                    map_insert_ref,
                    allocate_ref,
                    environment,
                )?;
                let (words, masks) = list_element_words(builder, value)?;
                let words_pointer = store_constant_words(builder, pointer_type, &words)?;
                let mask_values = masks
                    .iter()
                    .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                    .collect::<Vec<_>>();
                let masks_pointer = store_constant_words(builder, pointer_type, &mask_values)?;
                let word_count = builder.ins().iconst(pointer_type, words.len() as i64);
                let mask_count = builder.ins().iconst(pointer_type, masks.len() as i64);
                let call = builder.ins().call(
                    list_cons_ref,
                    &[words_pointer, word_count, masks_pointer, mask_count, list],
                );
                list = builder.inst_results(call)[0];
            }
            Ok(CompiledValue::List { pointer: list, ty })
        }
        MirConstant::Map(entries) => {
            let crate::sema::Type::Map(map_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Map MIR constant has a non-Map type".to_owned(),
                });
            };
            let info = type_table.map_info(map_id);
            let mut map = builder.ins().iconst(pointer_type, 0);
            for (key, value) in entries {
                let key = compile_mir_constant(
                    builder,
                    key,
                    info.key,
                    pointer_type,
                    type_table,
                    strings,
                    string_from_ref,
                    list_cons_ref,
                    map_insert_ref,
                    allocate_ref,
                    environment,
                )?;
                let value = compile_mir_constant(
                    builder,
                    value,
                    info.value,
                    pointer_type,
                    type_table,
                    strings,
                    string_from_ref,
                    list_cons_ref,
                    map_insert_ref,
                    allocate_ref,
                    environment,
                )?;
                let (key_words, key_masks) = list_element_words(builder, key)?;
                let (value_words, value_masks) = list_element_words(builder, value)?;
                let key_count = key_words.len();
                let mut words = key_words;
                words.extend(value_words);
                let mut masks = vec![0_u64; words.len().div_ceil(64)];
                for (word, mask) in key_masks.into_iter().enumerate() {
                    for bit in 0..64 {
                        if mask & (1_u64 << bit) != 0 {
                            let index = word * 64 + bit;
                            if index < words.len() {
                                masks[index / 64] |= 1_u64 << (index % 64);
                            }
                        }
                    }
                }
                for (word, mask) in value_masks.into_iter().enumerate() {
                    for bit in 0..64 {
                        if mask & (1_u64 << bit) != 0 {
                            let index = key_count + word * 64 + bit;
                            if index < words.len() {
                                masks[index / 64] |= 1_u64 << (index % 64);
                            }
                        }
                    }
                }
                let words_pointer = store_constant_words(builder, pointer_type, &words)?;
                let mask_values = masks
                    .iter()
                    .map(|mask| builder.ins().iconst(types::I64, *mask as i64))
                    .collect::<Vec<_>>();
                let masks_pointer = store_constant_words(builder, pointer_type, &mask_values)?;
                let word_count = builder.ins().iconst(pointer_type, words.len() as i64);
                let mask_count = builder.ins().iconst(pointer_type, masks.len() as i64);
                let key_word_count = builder.ins().iconst(pointer_type, key_count as i64);
                let key_kind = super::super::runtime::map_key_descriptor(
                    builder,
                    pointer_type,
                    info.key,
                    environment,
                )?;
                let call = builder.ins().call(
                    map_insert_ref,
                    &[
                        map,
                        words_pointer,
                        word_count,
                        masks_pointer,
                        mask_count,
                        key_word_count,
                        key_kind,
                    ],
                );
                map = builder.inst_results(call)[0];
            }
            Ok(CompiledValue::Map { pointer: map, ty })
        }
        MirConstant::Enum { variant, fields } => {
            let crate::sema::Type::Enum(enum_id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "Enum MIR constant has a non-Enum type".to_owned(),
                });
            };
            let variants = type_table.enum_variants(enum_id);
            let layout = variants
                .get(*variant)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "Enum MIR constant variant is out of bounds".to_owned(),
                })?;
            if fields.len() != layout.fields.len() {
                return Err(CodegenError::RuntimeError {
                    message: "Enum MIR constant field count mismatch".to_owned(),
                });
            }
            let mut field_values = Vec::new();
            for (index, candidate) in variants.iter().enumerate() {
                for (_, field_type) in &candidate.fields {
                    let value = if index == *variant {
                        let field_index = field_values.len()
                            - variants[..index]
                                .iter()
                                .map(|item| item.fields.len())
                                .sum::<usize>();
                        compile_mir_constant(
                            builder,
                            &fields[field_index],
                            *field_type,
                            pointer_type,
                            type_table,
                            strings,
                            string_from_ref,
                            list_cons_ref,
                            map_insert_ref,
                            allocate_ref,
                            environment,
                        )?
                    } else {
                        zero_compiled_value(builder, *field_type, pointer_type, type_table)
                            .map_err(|error| CodegenError::RuntimeError {
                                message: error.to_string(),
                            })?
                    };
                    field_values.push(value);
                }
            }
            Ok(CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, *variant as i64),
                fields: field_values,
                ty,
            })
        }
        MirConstant::MutList(_) | MirConstant::MutMap(_) | MirConstant::MutSet(_) => {
            Err(CodegenError::RuntimeError {
                message: "mutable collection constants require handler payload lowering".to_owned(),
            })
        }
    }
}
