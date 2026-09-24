use super::*;
use crate::codegen::abi::{abi_types, value_arguments, value_from_params};
use cranelift_codegen::ir::{StackSlotData, StackSlotKind};

pub(super) fn compile(
    builder: &mut FunctionBuilder<'_>,
    context: &RuntimeCallContext<'_>,
    destination: MirValueId,
    op: u8,
    arguments: &[MirCallArgument],
    values: &mut HashMap<MirValueId, CompiledValue>,
) -> Result<(), CodegenError> {
    let pointer_type = context.pointer_type;
    let ty = RuntimeIntrinsic::Path(op)
        .spec(context.types, &[])
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?
        .result;
    let slot =
        builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 32, 3));
    let result =
        builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 40, 3));
    let mut index = 0;
    let mut owned = Vec::new();
    for argument in arguments {
        let value = values[&argument.value].clone();
        owned.push(match &value {
            CompiledValue::String { pointer, .. } | CompiledValue::List { pointer, .. } => *pointer,
            _ => unreachable!("path input"),
        });
        for word in value_arguments(value) {
            builder
                .ins()
                .stack_store(pointer_type, word, slot, index * 8);
            index += 1;
        }
    }
    let input = builder.ins().stack_addr(pointer_type, slot, 0);
    let output = builder.ins().stack_addr(pointer_type, result, 0);
    let operation = builder.ins().iconst(types::I8, i64::from(op));
    builder
        .ins()
        .call(context.refs.path_call, &[operation, input, output]);
    let components = abi_types(ty, pointer_type, context.types)
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            builder
                .ins()
                .stack_load(pointer_type, ty, result, (index * 8) as i32)
        })
        .collect::<Vec<_>>();
    let value = value_from_params(&components, ty, context.types).map_err(|error| {
        CodegenError::RuntimeError {
            message: error.to_string(),
        }
    })?;
    values.insert(destination, value);
    for pointer in owned {
        builder.ins().call(context.refs.managed_drop, &[pointer]);
    }
    Ok(())
}
