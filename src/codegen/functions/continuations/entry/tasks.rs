use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;

use crate::codegen::abi::{abi_types, value_from_params};
use crate::codegen::environment::CompiledValue;
use crate::diagnostic::CodegenError;
use crate::sema::{Type, TypeTable};

/// Load a borrowed task result or failure payload. MIR Claim owns the later
/// transfer, so reading must leave the source's drop callback intact.
pub(super) fn load_task_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: Value,
    offset: usize,
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let components = abi_types(ty, pointer_type, types)
        .into_iter()
        .enumerate()
        .map(|(index, ty)| {
            builder.ins().load(
                ty,
                MemFlagsData::new(),
                pointer,
                (offset + index * 8) as i32,
            )
        })
        .collect::<Vec<_>>();
    value_from_params(&components, ty, types).map_err(|error| CodegenError::RuntimeError {
        message: error.to_string(),
    })
}
