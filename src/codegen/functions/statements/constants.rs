use cranelift_codegen::ir::{types, InstBuilder};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::MirConstant;
use crate::sema::{Type, TypeTable};

use super::super::super::abi::abi_types;
use super::super::super::environment::{CompiledValue, Environment};
use super::super::runtime::{create_list_word_slot, map_key_descriptor, store_list_words};
use super::super::values::{compile_mir_constant, list_element_words};
use super::RuntimeCallRefs;

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_payload_constant(
    builder: &mut FunctionBuilder<'_>,
    constant: &MirConstant,
    ty: Type,
    types_table: &TypeTable,
    pointer_type: cranelift_codegen::ir::Type,
    calls: RuntimeCallRefs,
    environment: &Environment,
    strings: &mut dyn Iterator<Item = (super::super::context::StringValue, usize)>,
) -> Result<CompiledValue, CodegenError> {
    match constant {
        MirConstant::MutList(values) => {
            let Type::MutList(id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MutList payload type mismatch".into(),
                });
            };
            let element = types_table.list_type(id);
            let words = abi_types(element, pointer_type, types_table).len();
            let masks = words.div_ceil(64);
            let args = [
                builder.ins().iconst(pointer_type, words as i64),
                builder.ins().iconst(pointer_type, masks as i64),
            ];
            let call = builder.ins().call(calls.mut_list_new, &args);
            let pointer = builder.inst_results(call)[0];
            for value in values {
                let value = compile_payload_constant(
                    builder,
                    value,
                    element,
                    types_table,
                    pointer_type,
                    calls,
                    environment,
                    strings,
                )?;
                let (words, masks) = list_element_words(builder, value)?;
                let mask_values = masks
                    .iter()
                    .map(|m| builder.ins().iconst(types::I64, *m as i64))
                    .collect::<Vec<_>>();
                let wp = store_list_words(builder, pointer_type, &words)?;
                let mp = store_list_words(builder, pointer_type, &mask_values)?;
                let word_count = builder.ins().iconst(pointer_type, words.len() as i64);
                let mask_count = builder.ins().iconst(pointer_type, masks.len() as i64);
                let call = builder.ins().call(
                    calls.mut_list_push,
                    &[pointer, wp, word_count, mp, mask_count],
                );
                let _ = builder.inst_results(call);
            }
            Ok(CompiledValue::MutList { pointer, ty })
        }
        MirConstant::MutMap(entries) => {
            let Type::MutMap(id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MutMap payload type mismatch".into(),
                });
            };
            let info = types_table.map_info(id);
            let kw = abi_types(info.key, pointer_type, types_table).len().max(1);
            let vw = abi_types(info.value, pointer_type, types_table)
                .len()
                .max(1);
            let args = [
                builder.ins().iconst(pointer_type, kw as i64),
                builder.ins().iconst(pointer_type, kw.div_ceil(64) as i64),
                builder.ins().iconst(pointer_type, vw as i64),
                builder.ins().iconst(pointer_type, vw.div_ceil(64) as i64),
                map_key_descriptor(builder, pointer_type, info.key, environment)?,
            ];
            let call = builder.ins().call(calls.mut_map_new, &args);
            let map = builder.inst_results(call)[0];
            for (key, value) in entries {
                let key = compile_payload_constant(
                    builder,
                    key,
                    info.key,
                    types_table,
                    pointer_type,
                    calls,
                    environment,
                    strings,
                )?;
                let value = compile_payload_constant(
                    builder,
                    value,
                    info.value,
                    types_table,
                    pointer_type,
                    calls,
                    environment,
                    strings,
                )?;
                let (kwv, km) = list_element_words(builder, key)?;
                let (vwv, vm) = list_element_words(builder, value)?;
                let key_masks = km
                    .iter()
                    .map(|m| builder.ins().iconst(types::I64, *m as i64))
                    .collect::<Vec<_>>();
                let value_masks = vm
                    .iter()
                    .map(|m| builder.ins().iconst(types::I64, *m as i64))
                    .collect::<Vec<_>>();
                let kp = store_list_words(builder, pointer_type, &kwv)?;
                let kmp = store_list_words(builder, pointer_type, &key_masks)?;
                let vp = store_list_words(builder, pointer_type, &vwv)?;
                let vmp = store_list_words(builder, pointer_type, &value_masks)?;
                let (_, out) = create_list_word_slot(builder, pointer_type, vwv.len())?;
                let args = [
                    map,
                    kp,
                    builder.ins().iconst(pointer_type, kwv.len() as i64),
                    kmp,
                    builder.ins().iconst(pointer_type, km.len() as i64),
                    vp,
                    builder.ins().iconst(pointer_type, vwv.len() as i64),
                    vmp,
                    builder.ins().iconst(pointer_type, vm.len() as i64),
                    out,
                    builder.ins().iconst(pointer_type, vwv.len() as i64),
                ];
                let _ = builder.ins().call(calls.mut_map_insert, &args);
            }
            Ok(CompiledValue::MutMap { pointer: map, ty })
        }
        MirConstant::MutSet(values) => {
            let Type::MutSet(id) = ty else {
                return Err(CodegenError::RuntimeError {
                    message: "MutSet payload type mismatch".into(),
                });
            };
            let info = types_table.map_info(id);
            let kw = abi_types(info.key, pointer_type, types_table).len().max(1);
            let args = [
                builder.ins().iconst(pointer_type, kw as i64),
                builder.ins().iconst(pointer_type, kw.div_ceil(64) as i64),
                builder.ins().iconst(pointer_type, 1),
                builder.ins().iconst(pointer_type, 1),
                map_key_descriptor(builder, pointer_type, info.key, environment)?,
            ];
            let call = builder.ins().call(calls.mut_map_new, &args);
            let set = builder.inst_results(call)[0];
            for value in values {
                let value = compile_payload_constant(
                    builder,
                    value,
                    info.key,
                    types_table,
                    pointer_type,
                    calls,
                    environment,
                    strings,
                )?;
                let (kwv, km) = list_element_words(builder, value)?;
                let key_masks = km
                    .iter()
                    .map(|m| builder.ins().iconst(types::I64, *m as i64))
                    .collect::<Vec<_>>();
                let kp = store_list_words(builder, pointer_type, &kwv)?;
                let kmp = store_list_words(builder, pointer_type, &key_masks)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let vp = store_list_words(builder, pointer_type, &[zero])?;
                let vmp = store_list_words(builder, pointer_type, &[zero])?;
                let (_, out) = create_list_word_slot(builder, pointer_type, 1)?;
                let args = [
                    set,
                    kp,
                    builder.ins().iconst(pointer_type, kwv.len() as i64),
                    kmp,
                    builder.ins().iconst(pointer_type, km.len() as i64),
                    vp,
                    builder.ins().iconst(pointer_type, 1),
                    vmp,
                    builder.ins().iconst(pointer_type, 1),
                    out,
                    builder.ins().iconst(pointer_type, 1),
                ];
                let _ = builder.ins().call(calls.mut_map_insert, &args);
            }
            Ok(CompiledValue::MutSet { pointer: set, ty })
        }
        _ => compile_mir_constant(
            builder,
            constant,
            ty,
            pointer_type,
            types_table,
            strings,
            calls.string_from,
            calls.bytes_from_data,
            calls.list_cons,
            calls.map_insert,
            calls.allocate,
            environment,
        ),
    }
}
