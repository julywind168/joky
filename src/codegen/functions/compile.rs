//! MIR function body compilation for the Cranelift backend.

use cranelift_codegen::ir::{AbiParam, Signature};

use crate::sema::{Type, TypeTable};

use super::super::abi::abi_types;
use super::super::cranelift::{CraneliftBackend, ModuleLifecycle};
use super::super::environment::FunctionType;

impl<M: ModuleLifecycle> CraneliftBackend<M> {
    /// Build the function's Cranelift signature
    pub(in crate::codegen) fn build_signature(
        &self,
        function_type: &FunctionType,
        is_main: bool,
        types: &TypeTable,
        pointer_type: cranelift_codegen::ir::Type,
        call_conv: cranelift_codegen::isa::CallConv,
    ) -> Signature {
        let mut params = if is_main {
            Vec::new()
        } else {
            function_type
                .receiver
                .iter()
                .chain(function_type.parameters.iter())
                .flat_map(|ty| match ty {
                    // An extern callback parameter carries a single C
                    // trampoline address instead of a two-word closure value.
                    Type::Function(_) if function_type.foreign => vec![pointer_type],
                    _ => abi_types(*ty, pointer_type, types),
                })
                .map(AbiParam::new)
                .collect()
        };
        if function_type.pending_abi && !is_main {
            params.push(AbiParam::new(pointer_type));
        }
        let returns = if is_main {
            if function_type.pending_abi {
                vec![AbiParam::new(cranelift_codegen::ir::types::I8)]
            } else {
                Vec::new()
            }
        } else {
            let mut returns = abi_types(function_type.return_type, pointer_type, types)
                .into_iter()
                .map(AbiParam::new)
                .collect::<Vec<_>>();
            if function_type.pending_abi {
                returns.insert(0, AbiParam::new(cranelift_codegen::ir::types::I8));
            }
            returns
        };

        Signature {
            params,
            returns,
            call_conv,
        }
    }
}
