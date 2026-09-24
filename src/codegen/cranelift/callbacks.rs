//! Typed C entries and Joky invocation adapters for retained callbacks.

use super::*;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(super) fn define_retained_callback(
        &mut self,
        id: usize,
        table: &crate::sema::TypeTable,
    ) -> Result<cranelift_module::FuncId, CodegenError> {
        let callback = table.function_type(id);
        let pointer = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();
        let result_type = if callback.return_type == crate::sema::Type::Unit {
            None
        } else {
            Some(super::foreign::c_parameter(callback.return_type, pointer)?)
        };
        let mut c_params = vec![AbiParam::new(pointer)];
        c_params.extend(
            callback
                .parameters
                .iter()
                .map(|ty| super::foreign::c_parameter(*ty, pointer))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let entry_signature = Signature {
            params: c_params,
            returns: result_type.into_iter().collect(),
            call_conv,
        };
        let invoke_signature = Signature {
            params: vec![AbiParam::new(pointer); 5],
            returns: vec![AbiParam::new(types::I8)],
            call_conv,
        };
        let factory_signature = Signature {
            params: vec![
                AbiParam::new(pointer),
                AbiParam::new(pointer),
                AbiParam::new(types::I64),
            ],
            returns: vec![AbiParam::new(pointer)],
            call_conv,
        };
        let suffix = self.next_function;
        self.next_function += 1;
        let entry = self
            .module
            .declare_function(
                &format!("joky_callback_entry_{suffix}"),
                Linkage::Local,
                &entry_signature,
            )
            .map_err(codegen_error)?;
        let invoke = self
            .module
            .declare_function(
                &format!("joky_callback_invoke_{suffix}"),
                Linkage::Local,
                &invoke_signature,
            )
            .map_err(codegen_error)?;
        let factory = self
            .module
            .declare_function(
                &format!("joky_callback_new_{suffix}"),
                Linkage::Local,
                &factory_signature,
            )
            .map_err(codegen_error)?;
        let dispatch_signature = Signature {
            params: vec![AbiParam::new(pointer); 3],
            returns: vec![],
            call_conv,
        };
        let dispatch = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::CALLBACK_INVOKE_SYMBOL,
                Linkage::Import,
                &dispatch_signature,
            )
            .map_err(codegen_error)?;
        let new_signature = Signature {
            params: vec![
                AbiParam::new(pointer),
                AbiParam::new(pointer),
                AbiParam::new(pointer),
                AbiParam::new(pointer),
                AbiParam::new(types::I64),
                AbiParam::new(pointer),
            ],
            returns: vec![AbiParam::new(pointer)],
            call_conv,
        };
        let new = self
            .module
            .declare_function(
                joky_runtime_abi::symbols::CALLBACK_NEW_SYMBOL,
                Linkage::Import,
                &new_signature,
            )
            .map_err(codegen_error)?;

        let mut context = self.module.make_context();
        context.func.signature = entry_signature;
        let dispatch_ref = self
            .module
            .declare_func_in_func(dispatch, &mut context.func);
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            let params = builder.block_params(block).to_vec();
            let size =
                u32::try_from(callback.parameters.len().max(1) * 8).map_err(codegen_error)?;
            let args_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                3,
            ));
            let args = builder.ins().stack_addr(pointer, args_slot, 0);
            for (index, value) in params[1..].iter().enumerate() {
                builder
                    .ins()
                    .store(MemFlagsData::new(), *value, args, (index * 8) as i32);
            }
            let result_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let result = builder.ins().stack_addr(pointer, result_slot, 0);
            builder.ins().call(dispatch_ref, &[params[0], args, result]);
            let returns = result_type
                .map(|ty| {
                    builder
                        .ins()
                        .load(ty.value_type, MemFlagsData::new(), result, 0)
                })
                .into_iter()
                .collect::<Vec<_>>();
            builder.ins().return_(&returns);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(entry, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);

        context.func.signature = invoke_signature;
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            let params = builder.block_params(block).to_vec();
            let mut signature = Signature {
                params: vec![AbiParam::new(pointer)],
                returns: result_type
                    .map(|p| AbiParam::new(p.value_type))
                    .into_iter()
                    .collect(),
                call_conv,
            };
            let mut arguments = vec![params[1]];
            for (index, ty) in callback.parameters.iter().enumerate() {
                let ty = super::foreign::c_parameter(*ty, pointer)?.value_type;
                signature.params.push(AbiParam::new(ty));
                arguments.push(builder.ins().load(
                    ty,
                    MemFlagsData::new(),
                    params[2],
                    (index * 8) as i32,
                ));
            }
            if callback.suspends {
                signature.params.push(AbiParam::new(pointer));
                signature.returns.insert(0, AbiParam::new(types::I8));
                arguments.push(params[4]);
            }
            let signature = builder.import_signature(signature);
            let call = builder
                .ins()
                .call_indirect(signature, params[0], &arguments);
            let results = builder.inst_results(call).to_vec();
            let status = if callback.suspends {
                results[0]
            } else {
                builder.ins().iconst(types::I8, 0)
            };
            // Pending return words are placeholders and must not be observed.
            let ready = builder.create_block();
            let exit = builder.create_block();
            builder.ins().brif(status, exit, &[], ready, &[]);
            builder.switch_to_block(ready);
            if result_type.is_some() {
                builder.ins().store(
                    MemFlagsData::new(),
                    results[usize::from(callback.suspends)],
                    params[3],
                    0,
                );
            }
            builder.ins().jump(exit, &[]);
            builder.switch_to_block(exit);
            builder.ins().return_(&[status]);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(invoke, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);

        context.func.signature = factory_signature;
        let entry_ref = self.module.declare_func_in_func(entry, &mut context.func);
        let invoke_ref = self.module.declare_func_in_func(invoke, &mut context.func);
        let new_ref = self.module.declare_func_in_func(new, &mut context.func);
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            let params = builder.block_params(block).to_vec();
            let entry = builder.ins().func_addr(pointer, entry_ref);
            let invoke = builder.ins().func_addr(pointer, invoke_ref);
            let size = builder
                .ins()
                .iconst(pointer, if result_type.is_some() { 8 } else { 0 });
            let call = builder.ins().call(
                new_ref,
                &[params[0], params[1], entry, invoke, params[2], size],
            );
            let value = builder.inst_results(call)[0];
            builder.ins().return_(&[value]);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(factory, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(factory)
    }
}
