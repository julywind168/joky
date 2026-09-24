use cranelift_codegen::ir::{InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::sema::Type;

use super::super::super::environment::CompiledValue;

pub(crate) fn map_key_descriptor(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: cranelift_codegen::ir::Type,
    key: Type,
    environment: &super::super::super::environment::Environment,
) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    let (hash, equal) =
        environment
            .map_key_refs
            .get(&key)
            .ok_or_else(|| CodegenError::RuntimeError {
                message: format!("missing map key adapters for {key:?}"),
            })?;
    let hash = builder.ins().func_addr(pointer_type, *hash);
    let equal = builder.ins().func_addr(pointer_type, *equal);
    store_list_words(builder, pointer_type, &[hash, equal])
}

pub(crate) fn create_list_word_slot(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: cranelift_codegen::ir::Type,
    word_count: usize,
) -> Result<
    (
        cranelift_codegen::ir::StackSlot,
        cranelift_codegen::ir::Value,
    ),
    CodegenError,
> {
    let size = word_count
        .checked_mul(size_of::<u64>())
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| CodegenError::RuntimeError {
            message: "List element representation is too large".to_owned(),
        })?;
    let slot =
        builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
    let pointer = builder.ins().stack_addr(pointer_type, slot, 0);
    Ok((slot, pointer))
}

pub(crate) fn store_list_words(
    builder: &mut FunctionBuilder<'_>,
    pointer_type: cranelift_codegen::ir::Type,
    words: &[cranelift_codegen::ir::Value],
) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    let (slot, pointer) = create_list_word_slot(builder, pointer_type, words.len())?;
    for (index, word) in words.iter().enumerate() {
        builder
            .ins()
            .stack_store(pointer_type, *word, slot, (index * 8) as i32);
    }
    Ok(pointer)
}

pub(super) fn combine_masks(parts: &[(usize, Vec<u64>)]) -> Vec<u64> {
    let words = parts.iter().map(|(count, _)| *count).sum::<usize>();
    let mut combined = vec![0_u64; words.div_ceil(64)];
    let mut offset = 0;
    for (count, masks) in parts {
        for index in 0..*count {
            if masks[index / 64] & (1_u64 << (index % 64)) != 0 {
                let target = offset + index;
                combined[target / 64] |= 1_u64 << (target % 64);
            }
        }
        offset += count;
    }
    combined
}

pub(super) fn drop_map_key(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    managed_drop_ref: cranelift_codegen::ir::FuncRef,
) {
    match value {
        CompiledValue::String { pointer, .. } | CompiledValue::Bytes { pointer } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::Struct { fields, .. }
        | CompiledValue::Tuple {
            elements: fields, ..
        } => {
            for field in fields {
                drop_map_key(builder, field, managed_drop_ref);
            }
        }
        _ => {}
    }
}
