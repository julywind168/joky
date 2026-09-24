use cranelift_codegen::ir::{
    immediates::{Ieee32, Ieee64},
    types, InstBuilder,
};
use cranelift_frontend::FunctionBuilder;

use crate::mir::MirValueId;
use crate::sema::{Type, TypeTable};
use crate::Diagnostic;

use super::environment::CompiledValue;
use super::types::cranelift_type;

pub(super) fn compile_mir_constructor_values(
    fields: &[MirValueId],
    values: &std::collections::HashMap<MirValueId, CompiledValue>,
) -> Result<Vec<CompiledValue>, Diagnostic> {
    fields
        .iter()
        .map(|field| {
            values
                .get(field)
                .cloned()
                .ok_or_else(|| Diagnostic::codegen("MIR constructor field was not compiled"))
        })
        .collect()
}

pub(super) fn zero_compiled_value(
    builder: &mut FunctionBuilder<'_>,
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    type_table: &TypeTable,
) -> Result<CompiledValue, Diagnostic> {
    Ok(match ty {
        Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64 => CompiledValue::Numeric {
            value: builder
                .ins()
                .iconst(cranelift_type(ty).expect("integer type"), 0),
            ty,
        },
        Type::Duration => CompiledValue::Numeric {
            value: builder.ins().iconst(types::I64, 0),
            ty,
        },
        Type::F32 => CompiledValue::Numeric {
            value: builder.ins().f32const(Ieee32::with_float(0.0)),
            ty,
        },
        Type::F64 => CompiledValue::Numeric {
            value: builder.ins().f64const(Ieee64::with_float(0.0)),
            ty,
        },
        Type::String => CompiledValue::String {
            pointer: builder.ins().iconst(pointer_type, 0),
            length: builder.ins().iconst(pointer_type, 0),
        },
        Type::CArray(_) => {
            return Err(Diagnostic::codegen(
                "C ABI values cannot be constructed yet",
            ))
        }
        Type::Bytes => CompiledValue::Bytes {
            pointer: builder.ins().iconst(pointer_type, 0),
        },
        Type::CPtr(_) | Type::CMutPtr(_) | Type::CStr => CompiledValue::NativeHandle {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::MutBytes => CompiledValue::MutBytes {
            pointer: builder.ins().iconst(pointer_type, 0),
        },
        Type::Batch
        | Type::SeqBuilder
        | Type::BytesCursor
        | Type::MapCursor(_)
        | Type::MapKeyCursor(_)
        | Type::MapValueCursor(_)
        | Type::MutListCursor(_)
        | Type::MutMapCursor(_)
        | Type::MutSetCursor(_)
        | Type::Hasher
        | Type::Native(_)
        | Type::CCallback => CompiledValue::NativeHandle {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Bool => CompiledValue::Boolean {
            value: builder.ins().iconst(types::I8, 0),
        },
        Type::Unit => CompiledValue::Unit,
        Type::Tuple(id) => CompiledValue::Tuple {
            elements: type_table
                .tuple_elements(id)
                .iter()
                .map(|ty| zero_compiled_value(builder, *ty, pointer_type, type_table))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        Type::Struct(id) => CompiledValue::Struct {
            fields: type_table
                .struct_fields(id)
                .iter()
                .map(|(_, ty)| zero_compiled_value(builder, *ty, pointer_type, type_table))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        Type::Class(_) => CompiledValue::Class {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Cown(_) => CompiledValue::Cown {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Enum(id) => CompiledValue::Enum {
            tag: builder.ins().iconst(types::I32, 0),
            fields: type_table
                .enum_variants(id)
                .iter()
                .flat_map(|variant| variant.fields.iter())
                .map(|(_, ty)| zero_compiled_value(builder, *ty, pointer_type, type_table))
                .collect::<Result<Vec<_>, _>>()?,
            ty,
        },
        Type::Option(id) => CompiledValue::Enum {
            tag: builder.ins().iconst(types::I32, 0),
            fields: vec![zero_compiled_value(
                builder,
                type_table.option_type(id),
                pointer_type,
                type_table,
            )?],
            ty,
        },
        Type::Result(id) => {
            let (ok, err) = type_table.result_types(id);
            CompiledValue::Enum {
                tag: builder.ins().iconst(types::I32, 0),
                fields: vec![
                    zero_compiled_value(builder, ok, pointer_type, type_table)?,
                    zero_compiled_value(builder, err, pointer_type, type_table)?,
                ],
                ty,
            }
        }
        Type::List(_) => CompiledValue::List {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::MutList(_) => CompiledValue::MutList {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Map(_) => CompiledValue::Map {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::MutMap(_) => CompiledValue::MutMap {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::MutSet(_) => CompiledValue::MutSet {
            pointer: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Dyn(_) | Type::Function(_) => CompiledValue::Function {
            code: builder.ins().iconst(pointer_type, 0),
            environment: builder.ins().iconst(pointer_type, 0),
            ty,
        },
        Type::Param(_) | Type::SelfType | Type::Associated(_) => {
            return Err(Diagnostic::codegen(
                "generic type parameter reached code generation",
            ))
        }
    })
}
