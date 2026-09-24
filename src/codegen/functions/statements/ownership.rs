use cranelift_codegen::ir::{FuncRef, InstBuilder};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;

use super::super::super::environment::CompiledValue;

pub(crate) fn duplicate_shared_value(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    dup_ref: FuncRef,
) -> Result<CompiledValue, CodegenError> {
    Ok(match value {
        CompiledValue::String { pointer, length } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::String {
                pointer: builder.inst_results(call)[0],
                length,
            }
        }
        CompiledValue::Bytes { pointer } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::Bytes {
                pointer: builder.inst_results(call)[0],
            }
        }
        CompiledValue::NativeHandle {
            pointer,
            ty:
                ty @ (crate::sema::Type::Batch
                | crate::sema::Type::SeqBuilder
                | crate::sema::Type::BytesCursor),
        } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::NativeHandle {
                pointer: builder.inst_results(call)[0],
                ty,
            }
        }
        value @ CompiledValue::NativeHandle { ty, .. } if ty.is_c_pointer() => value,
        CompiledValue::NativeHandle { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a uniquely owned native handle".to_owned(),
            })
        }
        CompiledValue::MutBytes { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy uniquely owned MutBytes".to_owned(),
            })
        }
        CompiledValue::List { pointer, ty } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::List {
                pointer: builder.inst_results(call)[0],
                ty,
            }
        }
        CompiledValue::Map { pointer, ty } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::Map {
                pointer: builder.inst_results(call)[0],
                ty,
            }
        }
        CompiledValue::MutList { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a uniquely owned MutList".to_owned(),
            })
        }
        CompiledValue::MutMap { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a uniquely owned MutMap".to_owned(),
            })
        }
        CompiledValue::MutSet { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a uniquely owned MutSet".to_owned(),
            })
        }
        CompiledValue::Tuple { elements, ty } => CompiledValue::Tuple {
            elements: elements
                .into_iter()
                .map(|element| duplicate_shared_value(builder, element, dup_ref))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        CompiledValue::Struct { fields, ty } => CompiledValue::Struct {
            fields: fields
                .into_iter()
                .map(|field| duplicate_shared_value(builder, field, dup_ref))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        CompiledValue::Enum { tag, fields, ty } => CompiledValue::Enum {
            tag,
            fields: fields
                .into_iter()
                .map(|field| duplicate_shared_value(builder, field, dup_ref))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        CompiledValue::Numeric { value, ty } => CompiledValue::Numeric { value, ty },
        CompiledValue::Boolean { value } => CompiledValue::Boolean { value },
        CompiledValue::Function { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a function value".to_owned(),
            })
        }
        CompiledValue::Unit => CompiledValue::Unit,
        CompiledValue::Class { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "MIR dup cannot copy a uniquely owned class".to_owned(),
            })
        }
        CompiledValue::Cown { pointer, ty } => {
            let call = builder.ins().call(dup_ref, &[pointer]);
            CompiledValue::Cown {
                pointer: builder.inst_results(call)[0],
                ty,
            }
        }
    })
}
