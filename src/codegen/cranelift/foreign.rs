//! Synchronous adapters from the Joky scalar ABI to the host C ABI.

use super::*;
use crate::mir::MirFunction;
use crate::sema::{Type, TypeTable};
use cranelift_codegen::ir::InstBuilder;
use cranelift_frontend::FunctionBuilder;
use cranelift_module::FuncId;

pub(in crate::codegen) fn c_parameter(
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
) -> Result<AbiParam, CodegenError> {
    let scalar = match ty {
        Type::CPtr(_) | Type::CMutPtr(_) | Type::CStr | Type::Function(_) => {
            return Ok(AbiParam::new(pointer_type))
        }
        Type::I8 | Type::U8 => types::I8,
        Type::I16 | Type::U16 => types::I16,
        Type::I32 | Type::U32 => types::I32,
        Type::I64 | Type::U64 => types::I64,
        Type::F32 => types::F32,
        Type::F64 => types::F64,
        _ => return Err(codegen_error("unsupported C FFI scalar type")),
    };
    let parameter = AbiParam::new(scalar);
    // The host C ABI can require extension of narrow integer arguments/results,
    // notably on Apple AArch64. Do not reuse Joky's unextended scalar signature.
    Ok(match ty {
        Type::I8 | Type::I16 | Type::I32 => parameter.sext(),
        Type::U8 | Type::U16 | Type::U32 => parameter.uext(),
        _ => parameter,
    })
}

/// The Joky-side ABI of an extern declaration. Function-typed parameters
/// flatten to a single trampoline-address word; every other type keeps the
/// ordinary Joky layout.
fn extern_joky_abi_types(
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Vec<cranelift_codegen::ir::Type> {
    match ty {
        Type::Function(_) => vec![pointer_type],
        _ => crate::codegen::abi::abi_types(ty, pointer_type, types),
    }
}

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    pub(super) fn compile_foreign_function(
        &mut self,
        function: &MirFunction,
        id: FuncId,
        table: &TypeTable,
    ) -> Result<(), CodegenError> {
        let foreign = function.foreign.as_ref().expect("foreign declaration");
        if !self.foreign_libraries.contains_key(&foreign.library) {
            // Loading invokes the selected library's initialization code. The
            // explicit extern declaration authorizes that native code boundary.
            let library =
                unsafe { libloading::Library::new(&foreign.library) }.map_err(|error| {
                    codegen_error(format!(
                        "cannot load C library '{}': {error}",
                        foreign.library
                    ))
                })?;
            self.foreign_libraries
                .insert(foreign.library.clone(), library);
        }
        // The declaration supplies the signature; C libraries do not expose
        // runtime type metadata. Keep the owning library alive for all JIT calls.
        let address = unsafe {
            *self.foreign_libraries[&foreign.library]
                .get::<*const u8>(foreign.symbol.as_bytes())
                .map_err(|error| {
                    codegen_error(format!(
                        "cannot resolve C symbol '{}' in '{}': {error}",
                        foreign.symbol, foreign.library
                    ))
                })?
        };
        if address.is_null() {
            return Err(codegen_error("C function symbol resolved to null"));
        }
        let pointer_type = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();
        let native_signature = Signature {
            params: function
                .parameters
                .iter()
                .map(|p| c_parameter(p.ty, pointer_type))
                .collect::<Result<_, _>>()?,
            returns: if function.return_type == Type::Unit {
                Vec::new()
            } else {
                vec![c_parameter(function.return_type, pointer_type)?]
            },
            call_conv,
        };
        let mut context = self.module.make_context();
        let joky_type = super::helpers::mir_function_type(function);
        context.func.signature = Signature {
            params: joky_type
                .parameters
                .iter()
                .flat_map(|ty| extern_joky_abi_types(*ty, pointer_type, table))
                .map(AbiParam::new)
                .collect(),
            returns: crate::codegen::abi::abi_types(function.return_type, pointer_type, table)
                .into_iter()
                .map(AbiParam::new)
                .collect(),
            call_conv,
        };
        // JIT runs in this process, so the resolved address stays valid. AOT
        // emits a separate executable: Linux ASLR means that address is not
        // the callee in the child. Import the C symbol and let the host linker
        // relocate it (libc is already on the `cc` link line).
        let foreign_import = if M::IS_AOT {
            let import_id = self
                .module
                .declare_function(&foreign.symbol, Linkage::Import, &native_signature)
                .map_err(codegen_error)?;
            Some(
                self.module
                    .declare_func_in_func(import_id, &mut context.func),
            )
        } else {
            None
        };
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let arguments = builder.block_params(entry).to_vec();
            let call = if let Some(foreign_import) = foreign_import {
                builder.ins().call(foreign_import, &arguments)
            } else {
                let signature = builder.import_signature(native_signature);
                let target = builder.ins().iconst(pointer_type, address as usize as i64);
                builder.ins().call_indirect(signature, target, &arguments)
            };
            let results = builder.inst_results(call).to_vec();
            builder.ins().return_(&results);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(id, &mut context)
            .map_err(codegen_error)?;
        Ok(())
    }

    /// Define a C-callable trampoline for a zero-capture closure callback:
    /// C invokes it with the host ABI, it forwards every argument to the
    /// closure call glue with a null environment, and returns the result.
    /// The address of this function is what the C library stores.
    pub(in crate::codegen) fn define_callback_trampoline(
        &mut self,
        trampoline_id: FuncId,
        glue_id: FuncId,
        fn_type_id: usize,
        types: &TypeTable,
    ) -> Result<(), CodegenError> {
        let pointer_type = self.module.target_config().pointer_type();
        let call_conv = self.module.isa().default_call_conv();
        let callback = types.function_type(fn_type_id);
        let mut context = self.module.make_context();
        context.func.signature = Signature {
            params: callback
                .parameters
                .iter()
                .map(|ty| c_parameter(*ty, pointer_type))
                .collect::<Result<_, _>>()?,
            returns: if callback.return_type == Type::Unit {
                Vec::new()
            } else {
                vec![c_parameter(callback.return_type, pointer_type)?]
            },
            call_conv,
        };
        {
            let glue = self.module.declare_func_in_func(glue_id, &mut context.func);
            let mut builder = FunctionBuilder::new(&mut context.func, &mut self.function_context);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            // A zero-capture closure ignores its environment slot.
            let mut arguments = vec![builder.ins().iconst(pointer_type, 0)];
            for (ty, value) in callback
                .parameters
                .iter()
                .zip(builder.block_params(entry).to_vec())
            {
                debug_assert_eq!(
                    crate::codegen::abi::abi_types(*ty, pointer_type, types).len(),
                    1,
                    "callback parameters are C-flat scalars"
                );
                arguments.push(value);
            }
            let call = builder.ins().call(glue, &arguments);
            let results = builder.inst_results(call).to_vec();
            builder.ins().return_(&results);
            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }
        self.module
            .define_function(trampoline_id, &mut context)
            .map_err(codegen_error)?;
        self.module.clear_context(&mut context);
        Ok(())
    }
}
