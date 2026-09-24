use std::collections::HashMap;

use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Module};

use crate::diagnostic::CodegenError;
use crate::sema::{Type, TypeTable};

use super::super::super::abi::{abi_types, result_from_params, value_arguments, value_from_params};
use super::super::super::classes::emit_drop_value;
use super::super::super::environment::CompiledValue;

fn closure_layout(
    capture_types: &[Type],
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(Vec<Vec<i32>>, u32), CodegenError> {
    let mut size = 0_usize;
    let mut offsets = Vec::with_capacity(capture_types.len());
    for ty in capture_types {
        let mut fields = Vec::new();
        for component in abi_types(*ty, pointer_type, types) {
            let alignment = usize::try_from(component.bytes())
                .expect("component size")
                .min(8);
            size = (size + alignment - 1) & !(alignment - 1);
            fields.push(i32::try_from(size).map_err(|_| CodegenError::RuntimeError {
                message: "closure environment exceeds supported size".to_owned(),
            })?);
            size += usize::try_from(component.bytes()).expect("component size");
        }
        offsets.push(fields);
    }
    Ok((
        offsets,
        u32::try_from(size.max(1)).map_err(|_| CodegenError::RuntimeError {
            message: "closure environment exceeds supported size".to_owned(),
        })?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::functions) fn compile_closure_environment(
    builder: &mut FunctionBuilder<'_>,
    code: Value,
    ty: Type,
    captures: Vec<CompiledValue>,
    capture_types: Vec<Type>,
    drop_callback: Value,
    allocate_ref: cranelift_codegen::ir::FuncRef,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    let (offsets, size) = closure_layout(&capture_types, pointer_type, types)?;
    let size_value = builder.ins().iconst(pointer_type, i64::from(size));
    let align = builder.ins().iconst(pointer_type, 8);
    let pointer = builder
        .ins()
        .call(allocate_ref, &[size_value, align, drop_callback]);
    let pointer = builder.inst_results(pointer)[0];
    for ((value, ty), offsets) in captures.into_iter().zip(&capture_types).zip(&offsets) {
        let components = value_arguments(value);
        if components.len() != offsets.len()
            || components.len() != abi_types(*ty, pointer_type, types).len()
        {
            return Err(CodegenError::RuntimeError {
                message: "closure capture has invalid ABI shape".to_owned(),
            });
        }
        for (component, offset) in components.into_iter().zip(offsets) {
            builder
                .ins()
                .store(MemFlagsData::new(), component, pointer, *offset);
        }
    }
    Ok(CompiledValue::Function {
        code,
        ty,
        environment: pointer,
    })
}

pub(in crate::codegen::functions) fn load_closure_captures(
    builder: &mut FunctionBuilder<'_>,
    pointer: Value,
    capture_types: &[Type],
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<Vec<CompiledValue>, CodegenError> {
    let (offsets, _) = closure_layout(capture_types, pointer_type, types)?;
    capture_types
        .iter()
        .zip(offsets)
        .map(|(ty, offsets)| {
            let components = abi_types(*ty, pointer_type, types)
                .into_iter()
                .zip(offsets)
                .map(|(component, offset)| {
                    builder
                        .ins()
                        .load(component, MemFlagsData::new(), pointer, offset)
                })
                .collect::<Vec<_>>();
            result_from_params(components, *ty, types).map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen) fn define_closure_drop_glue(
    module: &mut impl cranelift_module::Module,
    glue_id: FuncId,
    capture_types: &[Type],
    class_drop_ids: &HashMap<usize, FuncId>,
    runtime_drop_id: FuncId,
    pointer_type: cranelift_codegen::ir::Type,
    call_conv: cranelift_codegen::isa::CallConv,
    types: &TypeTable,
    dynamic_slots: Option<usize>,
) -> Result<(), CodegenError> {
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = Signature {
        params: vec![AbiParam::new(pointer_type)],
        returns: Vec::new(),
        call_conv,
    };
    let runtime_drop_ref = module.declare_func_in_func(runtime_drop_id, &mut context.func);
    let class_drop_refs = class_drop_ids
        .iter()
        .map(|(id, func)| (*id, module.declare_func_in_func(*func, &mut context.func)))
        .collect();
    let mut function_context = cranelift_frontend::FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let environment = builder.block_params(entry)[0];
        let end = dynamic_slots.map(|_| builder.create_block());
        let environment = if let Some(slots) = dynamic_slots {
            let active = builder
                .ins()
                .load(pointer_type, MemFlagsData::new(), environment, 0);
            let drop = builder.create_block();
            builder.ins().brif(active, drop, &[], end.unwrap(), &[]);
            builder.switch_to_block(drop);
            builder.ins().iadd_imm_s(
                environment,
                ((slots + 1) * pointer_type.bytes() as usize) as i64,
            )
        } else {
            environment
        };
        for capture in load_closure_captures(
            &mut builder,
            environment,
            capture_types,
            pointer_type,
            types,
        )? {
            emit_drop_value(
                &mut builder,
                capture,
                &class_drop_refs,
                runtime_drop_ref,
                types,
            )?;
        }
        if let Some(end) = end {
            builder.ins().jump(end, &[]);
            builder.switch_to_block(end);
        }
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }
    module
        .define_function(glue_id, &mut context)
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
    module.clear_context(&mut context);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen) fn define_closure_call_glue(
    module: &mut impl cranelift_module::Module,
    glue_id: FuncId,
    target_id: FuncId,
    capture_types: &[Type],
    target_parameters: &[Type],
    return_type: Type,
    pending_abi: bool,
    pointer_type: cranelift_codegen::ir::Type,
    call_conv: cranelift_codegen::isa::CallConv,
    types: &TypeTable,
    dynamic: Option<(usize, bool)>,
) -> Result<(), CodegenError> {
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    let mut params = std::iter::once(AbiParam::new(pointer_type))
        .chain(
            target_parameters[capture_types.len()..]
                .iter()
                .flat_map(|ty| abi_types(*ty, pointer_type, types))
                .map(AbiParam::new),
        )
        .collect::<Vec<_>>();
    if pending_abi {
        params.push(AbiParam::new(pointer_type));
    }
    let mut returns = abi_types(return_type, pointer_type, types)
        .into_iter()
        .map(AbiParam::new)
        .collect::<Vec<_>>();
    if pending_abi {
        returns.insert(0, AbiParam::new(cranelift_codegen::ir::types::I8));
    }
    context.func.signature = Signature {
        params,
        returns,
        call_conv,
    };
    let target_ref = module.declare_func_in_func(target_id, &mut context.func);
    let mut function_context = cranelift_frontend::FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let params = builder.block_params(entry).to_vec();
        let mut arguments = Vec::new();
        let captures = if let Some((slots, consuming)) = dynamic {
            if consuming {
                // The callee now owns the payload, including on Pending/cancellation.
                // The caller retains only an empty managed wrapper to clean up.
                let zero = builder.ins().iconst(pointer_type, 0);
                builder.ins().store(MemFlagsData::new(), zero, params[0], 0);
            }
            builder.ins().iadd_imm_s(
                params[0],
                ((slots + 1) * pointer_type.bytes() as usize) as i64,
            )
        } else {
            params[0]
        };
        for capture in
            load_closure_captures(&mut builder, captures, capture_types, pointer_type, types)?
        {
            arguments.extend(value_arguments(capture));
        }
        let mut offset = 1;
        for ty in &target_parameters[capture_types.len()..] {
            let count = abi_types(*ty, pointer_type, types).len();
            let value = value_from_params(&params[offset..offset + count], *ty, types).map_err(
                |error| CodegenError::RuntimeError {
                    message: error.to_string(),
                },
            )?;
            arguments.extend(value_arguments(value));
            offset += count;
        }
        if pending_abi {
            let parent = params
                .last()
                .copied()
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: "pending closure call is missing parent".to_owned(),
                })?;
            arguments.push(parent);
        }
        let call = builder.ins().call(target_ref, &arguments);
        let results = builder.inst_results(call).to_vec();
        builder.ins().return_(&results);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }
    module
        .define_function(glue_id, &mut context)
        .map_err(|error| CodegenError::RuntimeError {
            message: error.to_string(),
        })?;
    module.clear_context(&mut context);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::codegen::functions) fn compile_dynamic_environment(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    payload: CompiledValue,
    concrete: Type,
    methods: &[crate::mir::MirFunctionId],
    environment: &super::super::super::environment::Environment,
    allocate: cranelift_codegen::ir::FuncRef,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, CodegenError> {
    // Prefix: payload-live flag, then sorted method entry pointers. The payload
    // uses the same flattened layout as closure captures and Pending arguments.
    let (offsets, payload_size) = closure_layout(&[concrete], pointer_type, types)?;
    let prefix = (methods.len() + 1) * pointer_type.bytes() as usize;
    let size = builder
        .ins()
        .iconst(pointer_type, prefix as i64 + i64::from(payload_size));
    let align = builder.ins().iconst(pointer_type, 8);
    let drop = builder
        .ins()
        .func_addr(pointer_type, environment.closure_drop_refs[&methods[0]]);
    let call = builder.ins().call(allocate, &[size, align, drop]);
    let pointer = builder.inst_results(call)[0];
    let active = builder.ins().iconst(pointer_type, 1);
    builder.ins().store(MemFlagsData::new(), active, pointer, 0);
    for (slot, method) in methods.iter().enumerate() {
        let code = builder
            .ins()
            .func_addr(pointer_type, environment.closure_call_refs[method]);
        builder.ins().store(
            MemFlagsData::new(),
            code,
            pointer,
            ((slot + 1) * pointer_type.bytes() as usize) as i32,
        );
    }
    for (value, offset) in value_arguments(payload).into_iter().zip(&offsets[0]) {
        builder
            .ins()
            .store(MemFlagsData::new(), value, pointer, prefix as i32 + *offset);
    }
    let vtable = builder
        .ins()
        .iadd_imm_s(pointer, i64::from(pointer_type.bytes()));
    Ok(CompiledValue::Function {
        code: vtable,
        environment: pointer,
        ty,
    })
}
