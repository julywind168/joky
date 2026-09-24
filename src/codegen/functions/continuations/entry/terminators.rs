use std::collections::HashMap;

use cranelift_codegen::ir::{Block, FuncRef, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;

use crate::codegen::abi::{abi_types, value_arguments};
use crate::codegen::environment::{CompiledValue, Environment};
use crate::codegen::functions::continuation::{
    continuation_frame_offset, continuation_pointer_offsets, continuation_return_source_local,
};
use crate::codegen::functions::values::expect_boolean;
use crate::diagnostic::CodegenError;
use crate::mir::{MirContinuation, MirFunction, MirTerminator, MirValueId};
use crate::sema::TypeTable;

#[expect(
    clippy::too_many_arguments,
    reason = "Continuation terminators combine CFG state, frame ownership and the runtime completion ABI."
)]
pub(super) fn compile_continuation_terminator(
    builder: &mut FunctionBuilder<'_>,
    terminator: &MirTerminator,
    values: &HashMap<MirValueId, CompiledValue>,
    environments: &mut [Option<Environment>],
    blocks: &[Option<Block>],
    environment: &Environment,
    function: &MirFunction,
    continuation: &MirContinuation,
    continuation_handle: Value,
    frame: Value,
    complete_ref: FuncRef,
    result_pointer_ref: FuncRef,
    main_result_ref: FuncRef,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(), CodegenError> {
    match terminator {
        MirTerminator::Goto { target, arguments } => {
            let arguments = arguments
                .iter()
                .map(|value| {
                    values
                        .get(value)
                        .cloned()
                        .ok_or_else(|| CodegenError::RuntimeError {
                            message: "continuation CFG jump value was not compiled".to_owned(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flat_map(value_arguments)
                .map(Into::into)
                .collect::<Vec<_>>();
            if environments[target.0].is_none() {
                environments[target.0] = Some(environment.clone());
            }
            let target = blocks[target.0].ok_or_else(|| CodegenError::RuntimeError {
                message: "continuation CFG jumps outside its resume region".to_owned(),
            })?;
            builder.ins().jump(target, &arguments);
        }
        MirTerminator::Branch {
            condition,
            then_block,
            else_block,
        } => {
            let condition =
                values
                    .get(condition)
                    .cloned()
                    .ok_or_else(|| CodegenError::RuntimeError {
                        message: "continuation CFG branch condition was not compiled".to_owned(),
                    })?;
            let condition = expect_boolean(condition)?;
            if environments[then_block.0].is_none() {
                environments[then_block.0] = Some(environment.clone());
            }
            if environments[else_block.0].is_none() {
                environments[else_block.0] = Some(environment.clone());
            }
            let then_block = blocks[then_block.0].ok_or_else(|| CodegenError::RuntimeError {
                message: "continuation CFG branches outside its resume region".to_owned(),
            })?;
            let else_block = blocks[else_block.0].ok_or_else(|| CodegenError::RuntimeError {
                message: "continuation CFG branches outside its resume region".to_owned(),
            })?;
            builder
                .ins()
                .brif(condition, then_block, &[], else_block, &[]);
        }
        MirTerminator::Return(return_value) => {
            if function.return_type != crate::sema::Type::Unit {
                if let Some(value_id) = *return_value {
                    let value = values.get(&value_id).cloned().ok_or_else(|| {
                        CodegenError::RuntimeError {
                            message: "continuation return value was not compiled".to_owned(),
                        }
                    })?;
                    let is_main_result = function.name == "main"
                        && matches!(function.return_type, crate::sema::Type::Result(_));
                    if is_main_result {
                        super::super::super::values::emit_main_result(
                            builder,
                            &value,
                            main_result_ref,
                        )?;
                    } else {
                        let components = value_arguments(value);
                        let expected = abi_types(function.return_type, pointer_type, types);
                        if components.len() != expected.len() {
                            return Err(CodegenError::RuntimeError {
                                message: "continuation machine result ABI shape mismatch"
                                    .to_owned(),
                            });
                        }
                        let result_call = builder
                            .ins()
                            .call(result_pointer_ref, &[continuation_handle]);
                        let result_pointer = builder.inst_results(result_call)[0];
                        for (index, component) in components.into_iter().enumerate() {
                            builder.ins().store(
                                MemFlagsData::new(),
                                component,
                                result_pointer,
                                (index * 8) as i32,
                            );
                        }
                    }
                    // A returned managed value moves out of its frame slot. Clear the
                    // source pointer so cancellation cleanup cannot release it after
                    // the result buffer has taken over ownership.
                    if types.needs_drop(function.return_type) {
                        if let Some(local) = continuation_return_source_local(
                            &function.blocks[continuation.resume_block.0],
                            value_id,
                        ) {
                            if let Some(offset) = continuation_frame_offset(
                                function,
                                continuation,
                                local,
                                pointer_type,
                                types,
                            ) {
                                for cleanup_offset in continuation_pointer_offsets(
                                    function.return_type,
                                    offset,
                                    types,
                                ) {
                                    let zero = builder.ins().iconst(pointer_type, 0);
                                    builder.ins().store(
                                        MemFlagsData::new(),
                                        zero,
                                        frame,
                                        cleanup_offset as i32,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            builder.ins().call(complete_ref, &[continuation_handle]);
            builder.ins().return_(&[]);
        }
        MirTerminator::Unreachable => {
            builder
                .ins()
                .trap(cranelift_codegen::ir::TrapCode::unwrap_user(1));
        }
    }
    Ok(())
}
