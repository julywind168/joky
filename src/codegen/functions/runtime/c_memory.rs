//! C-heap cell intrinsics: allocation, typed loads and stores, and release.

use std::collections::HashMap;

use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData};
use cranelift_frontend::FunctionBuilder;

use crate::diagnostic::CodegenError;
use crate::mir::{MirCallArgument, MirValueId, RuntimeIntrinsic};
use crate::sema::Type;

use super::super::super::environment::CompiledValue;
use super::RuntimeCallContext;

fn cell_value_type(
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
) -> Result<cranelift_codegen::ir::Type, CodegenError> {
    if ty.is_c_pointer() {
        return Ok(pointer_type);
    }
    match ty {
        Type::I8 | Type::U8 => Ok(types::I8),
        Type::I16 | Type::U16 => Ok(types::I16),
        Type::I32 | Type::U32 => Ok(types::I32),
        Type::I64 | Type::U64 => Ok(types::I64),
        Type::F32 => Ok(types::F32),
        Type::F64 => Ok(types::F64),
        _ => Err(CodegenError::RuntimeError {
            message: "C cell content must be a C scalar or pointer".to_owned(),
        }),
    }
}

fn cell_word(value: &CompiledValue) -> Result<cranelift_codegen::ir::Value, CodegenError> {
    match value {
        CompiledValue::Numeric { value, .. } => Ok(*value),
        CompiledValue::NativeHandle { pointer, .. } => Ok(*pointer),
        _ => Err(CodegenError::RuntimeError {
            message: "C cell values must be scalars or C pointers".to_owned(),
        }),
    }
}

fn wrap_cell_value(
    ty: Type,
    word: cranelift_codegen::ir::Value,
) -> Result<CompiledValue, CodegenError> {
    if ty.is_c_pointer() {
        Ok(CompiledValue::NativeHandle { pointer: word, ty })
    } else {
        Ok(CompiledValue::Numeric { value: word, ty })
    }
}

pub(super) fn compile_c_memory_call(
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
        RuntimeIntrinsic::CCellAlloc(content) => {
            let value = values.get(&arguments[0].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR C cell alloc argument was not compiled".to_owned(),
                }
            })?;
            let word = cell_word(&value)?;
            let cell_type = cell_value_type(*content, pointer_type)?;
            let size = builder.ins().iconst(pointer_type, cell_type.bytes() as i64);
            let call = builder.ins().call(context.refs.c_alloc, &[size]);
            let cell = builder.inst_results(call)[0];
            // A null cell cannot hold the value; keep the store off the null
            // address and let the caller observe the failure via is_null().
            let failure_block = builder.create_block();
            let store_block = builder.create_block();
            builder
                .ins()
                .brif(cell, store_block, &[], failure_block, &[]);
            builder.switch_to_block(store_block);
            builder.seal_block(store_block);
            builder.ins().store(MemFlagsData::new(), word, cell, 0);
            builder.ins().jump(failure_block, &[]);
            builder.switch_to_block(failure_block);
            builder.seal_block(failure_block);
            values.insert(
                *destination,
                CompiledValue::NativeHandle {
                    pointer: cell,
                    ty: Type::CMutPtr(types.c_pointer_id(*content).ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "C cell pointer type was not interned".to_owned(),
                        }
                    })?),
                },
            );
        }
        RuntimeIntrinsic::CCellRead(content) => {
            let CompiledValue::NativeHandle { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR C cell read argument was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "C cell read requires a CMutPtr".to_owned(),
                });
            };
            let cell_type = cell_value_type(*content, pointer_type)?;
            let word = builder
                .ins()
                .load(cell_type, MemFlagsData::new(), pointer, 0);
            values.insert(*destination, wrap_cell_value(*content, word)?);
        }
        RuntimeIntrinsic::CCellWrite(_content) => {
            let CompiledValue::NativeHandle { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR C cell write cell was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "C cell write requires a CMutPtr".to_owned(),
                });
            };
            let value = values.get(&arguments[1].value).cloned().ok_or_else(|| {
                CodegenError::RuntimeError {
                    message: "MIR C cell write value was not compiled".to_owned(),
                }
            })?;
            let word = cell_word(&value)?;
            builder.ins().store(MemFlagsData::new(), word, pointer, 0);
            values.insert(*destination, CompiledValue::Unit);
        }
        RuntimeIntrinsic::CCellFree => {
            let CompiledValue::NativeHandle { pointer, .. } = values
                .get(&arguments[0].value)
                .cloned()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "MIR C cell free argument was not compiled".to_owned(),
                })?
            else {
                return Err(CodegenError::RuntimeError {
                    message: "C cell free requires a CMutPtr".to_owned(),
                });
            };
            builder.ins().call(context.refs.c_free, &[pointer]);
            values.insert(*destination, CompiledValue::Unit);
        }
        _ => return Ok(false),
    }
    Ok(true)
}
