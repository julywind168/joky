//! Class heap layout, field access, and type-specific destruction.

use std::collections::HashMap;

use cranelift_codegen::ir::{
    condcodes::IntCC, AbiParam, FuncRef, InstBuilder, MemFlagsData, Signature, Value,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Module};

use crate::diagnostic::CodegenError;
use crate::sema::{Type, TypeTable};
use crate::Diagnostic;

use super::abi::{abi_types, result_from_params, value_arguments};
use super::environment::CompiledValue;
use super::helpers::codegen_error;

fn layout_offsets(
    id: usize,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<Vec<Vec<i32>>, Diagnostic> {
    let mut offset = 0_usize;
    let mut fields = Vec::new();
    for (_, field_type) in types.class_fields(id) {
        let mut offsets = Vec::new();
        for component in abi_types(*field_type, pointer_type, types) {
            let alignment = usize::try_from(component.bytes())
                .expect("type size fits usize")
                .min(8);
            offset = (offset + alignment - 1) & !(alignment - 1);
            offsets.push(
                i32::try_from(offset)
                    .map_err(|_| Diagnostic::codegen("class layout exceeds supported size"))?,
            );
            offset += usize::try_from(component.bytes()).expect("type size fits usize");
        }
        fields.push(offsets);
    }
    Ok(fields)
}

pub(super) fn size(
    id: usize,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<u32, Diagnostic> {
    let offsets = layout_offsets(id, pointer_type, types)?;
    let mut size = 0_usize;
    for (field_index, (_, field_type)) in types.class_fields(id).iter().enumerate() {
        for (offset, component) in
            offsets[field_index]
                .iter()
                .zip(abi_types(*field_type, pointer_type, types))
        {
            size = size.max(
                usize::try_from(*offset).expect("positive field offset")
                    + usize::try_from(component.bytes()).expect("type size fits usize"),
            );
        }
    }
    u32::try_from(size.max(1))
        .map_err(|_| Diagnostic::codegen("class layout exceeds supported size"))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn store_field(
    builder: &mut FunctionBuilder<'_>,
    pointer: Value,
    class_id: usize,
    field_index: usize,
    value: CompiledValue,
    field_type: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<(), Diagnostic> {
    let offsets = layout_offsets(class_id, pointer_type, types)?;
    let values = value_arguments(value);
    let components = abi_types(field_type, pointer_type, types);
    if values.len() != components.len() {
        return Err(Diagnostic::codegen(
            "class field value has invalid ABI shape",
        ));
    }
    for (value, offset) in values.into_iter().zip(&offsets[field_index]) {
        builder
            .ins()
            .store(MemFlagsData::new(), value, pointer, *offset);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_field(
    builder: &mut FunctionBuilder<'_>,
    pointer: Value,
    class_id: usize,
    field_index: usize,
    field_type: Type,
    pointer_type: cranelift_codegen::ir::Type,
    types: &TypeTable,
) -> Result<CompiledValue, Diagnostic> {
    let offsets = layout_offsets(class_id, pointer_type, types)?;
    let values = abi_types(field_type, pointer_type, types)
        .into_iter()
        .zip(&offsets[field_index])
        .map(|(component, offset)| {
            builder
                .ins()
                .load(component, MemFlagsData::new(), pointer, *offset)
        })
        .collect::<Vec<_>>();
    result_from_params(values, field_type, types)
}

pub(super) fn emit_drop_value(
    builder: &mut FunctionBuilder<'_>,
    value: CompiledValue,
    class_drop_refs: &HashMap<usize, FuncRef>,
    managed_drop_ref: FuncRef,
    types: &TypeTable,
) -> Result<(), CodegenError> {
    match value {
        CompiledValue::Class {
            pointer,
            ty: Type::Class(id),
        } => {
            let drop_ref = class_drop_refs
                .get(&id)
                .ok_or_else(|| CodegenError::RuntimeError {
                    message: format!("class {id} has no drop glue"),
                })?;
            builder.ins().call(*drop_ref, &[pointer]);
        }
        CompiledValue::Function { environment, .. } => {
            builder.ins().call(managed_drop_ref, &[environment]);
        }
        CompiledValue::Tuple { elements, .. } => {
            for element in elements {
                emit_drop_value(builder, element, class_drop_refs, managed_drop_ref, types)?;
            }
        }
        CompiledValue::Struct { fields, .. } => {
            for field in fields {
                emit_drop_value(builder, field, class_drop_refs, managed_drop_ref, types)?;
            }
        }
        CompiledValue::Enum {
            tag,
            fields,
            ty: Type::Enum(enum_id),
        } => {
            let done = builder.create_block();
            let mut field_offset = 0;
            for (variant_index, variant) in types.enum_variants(enum_id).iter().enumerate() {
                let drop_variant = builder.create_block();
                let next_variant = builder.create_block();
                let is_variant = builder
                    .ins()
                    .icmp_imm_u(IntCC::Equal, tag, variant_index as i64);
                builder
                    .ins()
                    .brif(is_variant, drop_variant, &[], next_variant, &[]);

                builder.switch_to_block(drop_variant);
                builder.seal_block(drop_variant);
                for field in &fields[field_offset..field_offset + variant.fields.len()] {
                    emit_drop_value(
                        builder,
                        field.clone(),
                        class_drop_refs,
                        managed_drop_ref,
                        types,
                    )?;
                }
                builder.ins().jump(done, &[]);

                field_offset += variant.fields.len();
                builder.switch_to_block(next_variant);
                builder.seal_block(next_variant);
            }
            builder.ins().jump(done, &[]);
            builder.switch_to_block(done);
            builder.seal_block(done);
        }
        CompiledValue::Enum {
            tag,
            fields,
            ty: Type::Option(option_id),
        } => {
            if types.needs_drop(types.option_type(option_id)) {
                let drop_some = builder.create_block();
                let done = builder.create_block();
                let is_some = builder.ins().icmp_imm_u(IntCC::Equal, tag, 0);
                builder.ins().brif(is_some, drop_some, &[], done, &[]);
                builder.switch_to_block(drop_some);
                builder.seal_block(drop_some);
                emit_drop_value(
                    builder,
                    fields[0].clone(),
                    class_drop_refs,
                    managed_drop_ref,
                    types,
                )?;
                builder.ins().jump(done, &[]);
                builder.switch_to_block(done);
                builder.seal_block(done);
            }
        }
        CompiledValue::Enum {
            tag,
            fields,
            ty: Type::Result(result_id),
        } => {
            let (ok, err) = types.result_types(result_id);
            if types.needs_drop(ok) || types.needs_drop(err) {
                let drop_ok = builder.create_block();
                let drop_err = builder.create_block();
                let done = builder.create_block();
                let is_ok = builder.ins().icmp_imm_u(IntCC::Equal, tag, 0);
                builder.ins().brif(is_ok, drop_ok, &[], drop_err, &[]);
                builder.switch_to_block(drop_ok);
                builder.seal_block(drop_ok);
                emit_drop_value(
                    builder,
                    fields[0].clone(),
                    class_drop_refs,
                    managed_drop_ref,
                    types,
                )?;
                builder.ins().jump(done, &[]);
                builder.switch_to_block(drop_err);
                builder.seal_block(drop_err);
                emit_drop_value(
                    builder,
                    fields[1].clone(),
                    class_drop_refs,
                    managed_drop_ref,
                    types,
                )?;
                builder.ins().jump(done, &[]);
                builder.switch_to_block(done);
                builder.seal_block(done);
            }
        }
        CompiledValue::NativeHandle { ty, .. } if ty.is_c_pointer() => {}
        CompiledValue::String { pointer, .. }
        | CompiledValue::Bytes { pointer }
        | CompiledValue::MutBytes { pointer }
        | CompiledValue::NativeHandle { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::Cown { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::List { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::Map { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::MutList { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::MutMap { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::MutSet { pointer, .. } => {
            builder.ins().call(managed_drop_ref, &[pointer]);
        }
        CompiledValue::Numeric { .. } | CompiledValue::Boolean { .. } | CompiledValue::Unit => {}
        CompiledValue::Class { .. } | CompiledValue::Enum { .. } => {
            return Err(CodegenError::RuntimeError {
                message: "owned value has invalid codegen type metadata".to_owned(),
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn define_drop_glue(
    module: &mut impl cranelift_module::Module,
    class_id: usize,
    glue_id: FuncId,
    user_drop_id: Option<FuncId>,
    glue_ids: &HashMap<usize, FuncId>,
    runtime_drop_id: FuncId,
    pointer_type: cranelift_codegen::ir::Type,
    call_conv: cranelift_codegen::isa::CallConv,
    types: &TypeTable,
) -> Result<(), CodegenError> {
    let frontend_config = module.target_config();
    let mut context = module.make_context();
    context.func.signature = Signature {
        params: vec![AbiParam::new(pointer_type)],
        returns: Vec::new(),
        call_conv,
    };

    let user_drop_ref = user_drop_id.map(|id| module.declare_func_in_func(id, &mut context.func));
    let region_refs = if user_drop_ref.is_some() {
        let signature = Signature {
            params: vec![],
            returns: vec![],
            call_conv,
        };
        let enter = module
            .declare_function(
                joky_runtime_abi::symbols::ENTER_SYMBOL,
                cranelift_module::Linkage::Import,
                &signature,
            )
            .map_err(codegen_error)?;
        let exit = module
            .declare_function(
                joky_runtime_abi::symbols::EXIT_SYMBOL,
                cranelift_module::Linkage::Import,
                &signature,
            )
            .map_err(codegen_error)?;
        Some((
            module.declare_func_in_func(enter, &mut context.func),
            module.declare_func_in_func(exit, &mut context.func),
        ))
    } else {
        None
    };
    let runtime_drop_ref = module.declare_func_in_func(runtime_drop_id, &mut context.func);
    let nested_drop_refs = glue_ids
        .iter()
        .map(|(id, function)| {
            (
                *id,
                module.declare_func_in_func(*function, &mut context.func),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut function_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let pointer = builder.block_params(entry)[0];

        // Cancelled calls and moved frame slots carry a null handle. Like
        // jk_drop, class glue must accept that sentinel without reading fields.
        let drop_fields = builder.create_block();
        let done = builder.create_block();
        let is_null = builder.ins().icmp_imm_u(IntCC::Equal, pointer, 0);
        builder.ins().brif(is_null, done, &[], drop_fields, &[]);
        builder.switch_to_block(drop_fields);
        if let (Some(user_drop), Some((enter, _))) = (user_drop_ref, region_refs) {
            builder.ins().call(enter, &[]);
            builder.ins().call(user_drop, &[pointer]);
        }

        for (field_index, (_, field_type)) in types.class_fields(class_id).iter().enumerate() {
            if !types.needs_drop(*field_type) {
                continue;
            }
            let field = load_field(
                &mut builder,
                pointer,
                class_id,
                field_index,
                *field_type,
                pointer_type,
                types,
            )
            .map_err(|error| CodegenError::RuntimeError {
                message: error.to_string(),
            })?;
            emit_drop_value(
                &mut builder,
                field,
                &nested_drop_refs,
                runtime_drop_ref,
                types,
            )?;
        }
        if let Some((_, exit)) = region_refs {
            builder.ins().call(exit, &[]);
        }
        builder.ins().call(runtime_drop_ref, &[pointer]);
        builder.ins().jump(done, &[]);
        builder.switch_to_block(done);
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }

    module
        .define_function(glue_id, &mut context)
        .map_err(codegen_error)?;
    module.clear_context(&mut context);
    Ok(())
}

#[cfg(test)]
mod process_layout_tests {
    use super::*;

    #[test]
    fn command_memory_offsets_match_native_process_decoder() {
        let source = format!(
            "{}\nfn main() {{}}",
            include_str!("../../std/joky/process.jk")
        );
        let checked =
            crate::sema::check_program(&crate::syntax::parse_program(&source).unwrap()).unwrap();
        let types = &checked.module.types;
        let Type::Class(id) = types.class_type("Command").unwrap() else {
            unreachable!()
        };
        assert_eq!(
            layout_offsets(id, cranelift_codegen::ir::types::I64, types).unwrap(),
            vec![vec![0, 8], vec![16], vec![24, 32, 40], vec![48, 56, 64],]
        );
        assert_eq!(
            size(id, cranelift_codegen::ir::types::I64, types).unwrap(),
            72
        );
        let Type::Class(pipeline) = types.class_type("Pipeline").unwrap() else {
            unreachable!()
        };
        assert_eq!(
            size(pipeline, cranelift_codegen::ir::types::I64, types).unwrap(),
            8
        );
        assert_eq!(
            layout_offsets(pipeline, cranelift_codegen::ir::types::I64, types).unwrap(),
            vec![vec![0]]
        );
        let stage = types.struct_type("Stage").unwrap();
        assert_eq!(
            abi_types(stage, cranelift_codegen::ir::types::I64, types).len(),
            9
        );
        let effect = types.effects().by_name("process").unwrap();
        for (name, words) in [
            ("status", 6),
            ("output", 8),
            ("pipeline_status", 4),
            ("pipeline_output", 6),
        ] {
            let id = types.effects().operation_by_name(effect, name).unwrap();
            let operation = types.effects().operation_info(id).unwrap();
            assert_eq!(
                abi_types(
                    operation.return_type,
                    cranelift_codegen::ir::types::I64,
                    types
                )
                .len(),
                words
            );
        }
    }
}
