use std::collections::HashMap;

use crate::diagnostic::CodegenError;
use crate::sema::TypeTable;
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, Signature};
use cranelift_frontend::FunctionBuilder;
use joky_runtime_abi::{TASK_CONTEXT_CAPTURES_OFFSET, TASK_CONTEXT_RESULT_OFFSET};

use super::super::abi::{abi_types, value_from_params};
use super::super::classes::emit_drop_value;
use super::super::cranelift::{CraneliftBackend, ModuleLifecycle};
use super::super::environment::FunctionType;
use super::super::helpers::codegen_error;
use super::statements::payload_word;

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(in crate::codegen) fn compile_task_thunk(
        &mut self,
        thunk_id: cranelift_module::FuncId,
        function_id: cranelift_module::FuncId,
        function_type: &FunctionType,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
        type_table: &TypeTable,
    ) -> Result<(), CodegenError> {
        if function_type.receiver.is_some() {
            return Err(CodegenError::RuntimeError {
                message: "task thunk cannot wrap a method receiver".to_owned(),
            });
        }
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        {
            let target = self
                .module
                .declare_func_in_func(function_id, &mut context.func);
            let status_ref = if function_type.pending_abi {
                let signature = Signature {
                    params: vec![AbiParam::new(types::I8)],
                    returns: vec![],
                    call_conv,
                };
                let id = self
                    .module
                    .declare_function(
                        joky_runtime_abi::symbols::TASK_FUNCTION_STATUS_SYMBOL,
                        cranelift_module::Linkage::Import,
                        &signature,
                    )
                    .map_err(codegen_error)?;
                Some(self.module.declare_func_in_func(id, &mut context.func))
            } else {
                None
            };
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let task_context = builder.block_params(entry)[0];
            let captures = builder.ins().load(
                pointer_type,
                MemFlagsData::new(),
                task_context,
                TASK_CONTEXT_CAPTURES_OFFSET,
            );
            let result = builder.ins().load(
                pointer_type,
                MemFlagsData::new(),
                task_context,
                TASK_CONTEXT_RESULT_OFFSET,
            );
            let mut arguments = Vec::new();
            let mut capture_offset = 0;
            for parameter in &function_type.parameters {
                for ty in abi_types(*parameter, pointer_type, type_table) {
                    arguments.push(builder.ins().load(
                        ty,
                        MemFlagsData::new(),
                        captures,
                        capture_offset,
                    ));
                    capture_offset += 8;
                }
            }
            if function_type.pending_abi {
                arguments.push(builder.ins().iconst(pointer_type, 0));
            }
            let call = builder.ins().call(target, &arguments);
            let mut results = builder.inst_results(call).to_vec();
            if let Some(status_ref) = status_ref {
                let status = results.remove(0);
                builder.ins().call(status_ref, &[status]);
                let ready = builder.create_block();
                let pending = builder.create_block();
                builder.ins().brif(status, pending, &[], ready, &[]);
                builder.switch_to_block(pending);
                builder.ins().return_(&[]);
                builder.seal_block(pending);
                builder.switch_to_block(ready);
                builder.seal_block(ready);
            }
            for (index, value) in results.iter().enumerate() {
                builder
                    .ins()
                    .store(MemFlagsData::new(), *value, result, (index * 8) as i32);
            }
            builder.ins().return_(&[]);
            builder.seal_block(entry);
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(thunk_id, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    /// Define the canonical byte-oriented thunk for a synthetic resumable
    /// handler. The wrapper decodes the request and capture words, calls the
    /// typed MIR function, and writes the flattened result. Managed results
    /// remain on the direct lexical path until their ownership descriptors
    /// are carried by the runtime handle.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::codegen) fn compile_resumable_handler_thunk(
        &mut self,
        thunk_id: cranelift_module::FuncId,
        function_id: cranelift_module::FuncId,
        function_type: &FunctionType,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
        types: &TypeTable,
        operation_parameter_count: usize,
    ) -> Result<(), CodegenError> {
        if function_type.receiver.is_some()
            || function_type.parameters.len() < operation_parameter_count
        {
            return Err(CodegenError::RuntimeError {
                message:
                    "canonical synthetic handler thunk requires operation parameters and a result"
                        .to_owned(),
            });
        }
        let request_types = function_type.parameters[..operation_parameter_count]
            .iter()
            .flat_map(|ty| abi_types(*ty, pointer_type, types))
            .collect::<Vec<_>>();
        let result_types = abi_types(function_type.return_type, pointer_type, types);
        if (operation_parameter_count == 0) != request_types.is_empty() {
            return Err(CodegenError::RuntimeError {
                message: "canonical synthetic handler thunk ABI has no result words".to_owned(),
            });
        }
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let target = self
            .module
            .declare_func_in_func(function_id, &mut context.func);
        let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let call = builder.block_params(entry)[0];
        let request = builder.ins().load(
            pointer_type,
            MemFlagsData::new(),
            call,
            joky_runtime_abi::HANDLER_CALL_REQUEST_OFFSET,
        );
        let mut request_offset = 0;
        let mut arguments = Vec::with_capacity(function_type.parameters.len());
        for request_type in request_types {
            arguments.push(builder.ins().load(
                request_type,
                MemFlagsData::new(),
                request,
                request_offset,
            ));
            request_offset += 8;
        }
        let captures = builder.ins().load(
            pointer_type,
            MemFlagsData::new(),
            call,
            joky_runtime_abi::HANDLER_CALL_CAPTURES_OFFSET,
        );
        let mut capture_offset = 0;
        for ty in function_type
            .parameters
            .iter()
            .skip(operation_parameter_count)
        {
            for capture_type in abi_types(*ty, pointer_type, types) {
                arguments.push(builder.ins().load(
                    capture_type,
                    MemFlagsData::new(),
                    captures,
                    capture_offset,
                ));
                capture_offset += 8;
            }
        }
        let result = builder.ins().call(target, &arguments);
        let result_values = builder.inst_results(result).to_vec();
        if result_values.len() != result_types.len() {
            return Err(CodegenError::RuntimeError {
                message: "canonical synthetic handler result ABI shape mismatch".to_owned(),
            });
        }
        let result_pointer = builder.ins().load(
            pointer_type,
            MemFlagsData::new(),
            call,
            joky_runtime_abi::HANDLER_CALL_RESULT_OFFSET,
        );
        for (index, (result_value, result_type)) in result_values
            .into_iter()
            .zip(result_types.iter())
            .enumerate()
        {
            let word = payload_word(&mut builder, result_value, *result_type);
            builder.ins().store(
                MemFlagsData::new(),
                word,
                result_pointer,
                (index * 8) as i32,
            );
        }
        let result_size = builder.ins().iconst(
            pointer_type,
            i64::try_from(result_types.len().saturating_mul(8)).unwrap_or(i64::MAX),
        );
        builder.ins().store(
            MemFlagsData::new(),
            result_size,
            call,
            joky_runtime_abi::HANDLER_CALL_RESULT_SIZE_OFFSET,
        );
        let success = builder.ins().iconst(types::I8, 1);
        builder.ins().return_(&[success]);
        builder.seal_block(entry);
        builder.finalize(self.module.target_config());
        self.module
            .define_function(thunk_id, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::codegen) fn compile_task_result_drop(
        &mut self,
        drop_id: cranelift_module::FuncId,
        result_type: crate::sema::Type,
        class_drop_ids: &HashMap<usize, cranelift_module::FuncId>,
        managed_drop_id: cranelift_module::FuncId,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
        type_table: &TypeTable,
    ) -> Result<(), CodegenError> {
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        {
            let managed_drop_ref = self
                .module
                .declare_func_in_func(managed_drop_id, &mut context.func);
            let class_drop_refs = class_drop_ids
                .iter()
                .map(|(id, drop)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*drop, &mut context.func),
                    )
                })
                .collect::<HashMap<_, _>>();
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let storage = builder.block_params(entry)[0];
            let values = abi_types(result_type, pointer_type, type_table)
                .into_iter()
                .enumerate()
                .map(|(index, ty)| {
                    builder
                        .ins()
                        .load(ty, MemFlagsData::new(), storage, (index * 8) as i32)
                })
                .collect::<Vec<_>>();
            let value = value_from_params(&values, result_type, type_table).map_err(|error| {
                CodegenError::RuntimeError {
                    message: error.to_string(),
                }
            })?;
            emit_drop_value(
                &mut builder,
                value,
                &class_drop_refs,
                managed_drop_ref,
                type_table,
            )?;
            builder.ins().return_(&[]);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(drop_id, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::codegen) fn compile_task_failure_drop(
        &mut self,
        drop_id: cranelift_module::FuncId,
        parameter_types: &[crate::sema::Type],
        class_drop_ids: &HashMap<usize, cranelift_module::FuncId>,
        managed_drop_id: cranelift_module::FuncId,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
        type_table: &TypeTable,
    ) -> Result<(), CodegenError> {
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: vec![AbiParam::new(pointer_type)],
            returns: vec![],
            call_conv,
        };
        {
            let managed_drop_ref = self
                .module
                .declare_func_in_func(managed_drop_id, &mut context.func);
            let class_drop_refs = class_drop_ids
                .iter()
                .map(|(id, drop)| {
                    (
                        *id,
                        self.module.declare_func_in_func(*drop, &mut context.func),
                    )
                })
                .collect::<HashMap<_, _>>();
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let storage = builder.block_params(entry)[0];
            let mut word_offset = 0_usize;
            for parameter_type in parameter_types {
                let components = abi_types(*parameter_type, pointer_type, type_table);
                let values = components
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        builder.ins().load(
                            *ty,
                            MemFlagsData::new(),
                            storage,
                            ((word_offset + index) * 8) as i32,
                        )
                    })
                    .collect::<Vec<_>>();
                word_offset += components.len();
                if !type_table.needs_drop(*parameter_type) {
                    continue;
                }
                let value =
                    value_from_params(&values, *parameter_type, type_table).map_err(|error| {
                        CodegenError::RuntimeError {
                            message: error.to_string(),
                        }
                    })?;
                emit_drop_value(
                    &mut builder,
                    value,
                    &class_drop_refs,
                    managed_drop_ref,
                    type_table,
                )?;
            }
            builder.ins().return_(&[]);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(drop_id, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(())
    }
}
