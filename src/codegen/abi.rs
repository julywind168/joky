//! Mapping between Joky values and flattened Cranelift ABI values.

use cranelift_codegen::ir::{types, Block, Value};
use cranelift_frontend::FunctionBuilder;

use crate::sema::{Type, TypeTable};
use crate::Diagnostic;

use super::environment::CompiledValue;
use super::types::cranelift_type;

pub(super) fn abi_types(
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    table: &TypeTable,
) -> Vec<cranelift_codegen::ir::Type> {
    match ty {
        Type::String => vec![pointer_type, pointer_type],
        Type::CPtr(_) | Type::CMutPtr(_) | Type::CStr => vec![pointer_type],
        Type::Unit => Vec::new(),
        Type::Bool => vec![types::I8],
        Type::Duration => vec![types::I64],
        Type::Tuple(id) => table
            .tuple_elements(id)
            .iter()
            .flat_map(|element| abi_types(*element, pointer_type, table))
            .collect(),
        Type::Struct(id) => table
            .struct_fields(id)
            .iter()
            .flat_map(|(_, field_type)| abi_types(*field_type, pointer_type, table))
            .collect(),
        Type::Enum(id) => std::iter::once(types::I32)
            .chain(table.enum_variants(id).iter().flat_map(|variant| {
                variant
                    .fields
                    .iter()
                    .flat_map(|(_, field_type)| abi_types(*field_type, pointer_type, table))
            }))
            .collect(),
        Type::Option(id) => std::iter::once(types::I32)
            .chain(abi_types(table.option_type(id), pointer_type, table))
            .collect(),
        Type::Result(id) => {
            let (ok, err) = table.result_types(id);
            std::iter::once(types::I32)
                .chain(abi_types(ok, pointer_type, table))
                .chain(abi_types(err, pointer_type, table))
                .collect()
        }
        Type::Bytes
        | Type::BytesCursor
        | Type::MapCursor(_)
        | Type::MapKeyCursor(_)
        | Type::MapValueCursor(_)
        | Type::MutListCursor(_)
        | Type::MutMapCursor(_)
        | Type::MutSetCursor(_)
        | Type::MutBytes
        | Type::Batch
        | Type::SeqBuilder
        | Type::Hasher
        | Type::Native(_)
        | Type::CCallback
        | Type::Class(_)
        | Type::Cown(_)
        | Type::List(_)
        | Type::MutList(_)
        | Type::Map(_)
        | Type::MutMap(_)
        | Type::MutSet(_) => {
            vec![pointer_type]
        }
        Type::Function(_) | Type::Dyn(_) => vec![pointer_type, pointer_type],
        _ => vec![cranelift_type(ty).expect("checked numeric type")],
    }
}

pub(super) fn append_result_params(
    builder: &mut FunctionBuilder<'_>,
    block: Block,
    ty: Type,
    pointer_type: cranelift_codegen::ir::Type,
    table: &TypeTable,
) -> Vec<Value> {
    abi_types(ty, pointer_type, table)
        .into_iter()
        .map(|ty| builder.append_block_param(block, ty))
        .collect()
}

pub(super) fn value_arguments(value: CompiledValue) -> Vec<Value> {
    match value {
        CompiledValue::Numeric { value, .. } | CompiledValue::Boolean { value } => vec![value],
        CompiledValue::String { pointer, length } => vec![pointer, length],
        CompiledValue::Bytes { pointer }
        | CompiledValue::MutBytes { pointer }
        | CompiledValue::NativeHandle { pointer, .. } => vec![pointer],
        CompiledValue::Tuple { elements, .. } => {
            elements.into_iter().flat_map(value_arguments).collect()
        }
        CompiledValue::Struct { fields, .. } => {
            fields.into_iter().flat_map(value_arguments).collect()
        }
        CompiledValue::Enum { tag, fields, .. } => std::iter::once(tag)
            .chain(fields.into_iter().flat_map(value_arguments))
            .collect(),
        CompiledValue::Class { pointer, .. }
        | CompiledValue::Cown { pointer, .. }
        | CompiledValue::List { pointer, .. }
        | CompiledValue::MutList { pointer, .. }
        | CompiledValue::Map { pointer, .. }
        | CompiledValue::MutMap { pointer, .. } => vec![pointer],
        CompiledValue::MutSet { pointer, .. } => vec![pointer],
        CompiledValue::Function {
            code, environment, ..
        } => vec![code, environment],
        CompiledValue::Unit => Vec::new(),
    }
}

pub(super) fn value_type(value: CompiledValue) -> Type {
    match value {
        CompiledValue::Numeric { ty, .. } => ty,
        CompiledValue::String { .. } => Type::String,
        CompiledValue::Bytes { .. } => Type::Bytes,
        CompiledValue::MutBytes { .. } => Type::MutBytes,
        CompiledValue::NativeHandle { ty, .. } => ty,
        CompiledValue::Boolean { .. } => Type::Bool,
        CompiledValue::Tuple { ty, .. } => ty,
        CompiledValue::Struct { ty, .. } => ty,
        CompiledValue::Enum { ty, .. } => ty,
        CompiledValue::Class { ty, .. }
        | CompiledValue::Cown { ty, .. }
        | CompiledValue::List { ty, .. }
        | CompiledValue::MutList { ty, .. }
        | CompiledValue::Map { ty, .. }
        | CompiledValue::MutMap { ty, .. } => ty,
        CompiledValue::MutSet { ty, .. } => ty,
        CompiledValue::Function { ty, .. } => ty,
        CompiledValue::Unit => Type::Unit,
    }
}

fn decode(
    params: &[Value],
    ty: Type,
    table: &TypeTable,
    context: &str,
) -> Result<CompiledValue, Diagnostic> {
    match (ty, params) {
        (Type::String, [pointer, length]) => Ok(CompiledValue::String {
            pointer: *pointer,
            length: *length,
        }),
        (Type::Bytes, [pointer]) => Ok(CompiledValue::Bytes { pointer: *pointer }),
        (Type::MutBytes, [pointer]) => Ok(CompiledValue::MutBytes { pointer: *pointer }),
        (ty @ (Type::CPtr(_) | Type::CMutPtr(_) | Type::CStr), [pointer]) => {
            Ok(CompiledValue::NativeHandle {
                pointer: *pointer,
                ty,
            })
        }
        (
            ty @ (Type::Batch
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
            | Type::CCallback),
            [pointer],
        ) => Ok(CompiledValue::NativeHandle {
            pointer: *pointer,
            ty,
        }),
        (Type::Bool, [value]) => Ok(CompiledValue::Boolean { value: *value }),
        (Type::Unit, []) => Ok(CompiledValue::Unit),
        (Type::Tuple(id), params) => {
            let mut offset = 0;
            let mut elements = Vec::new();
            for element_type in table.tuple_elements(id) {
                let count = abi_types(*element_type, types::I64, table).len();
                let end = offset + count;
                elements.push(decode(
                    slice(params, offset, end, "tuple", context)?,
                    *element_type,
                    table,
                    context,
                )?);
                offset = end;
            }
            ensure_consumed(offset, params.len(), "tuple", context)?;
            Ok(CompiledValue::Tuple { elements, ty })
        }
        (Type::Struct(id), params) => {
            let mut offset = 0;
            let mut fields = Vec::new();
            for (_, field_type) in table.struct_fields(id) {
                let count = abi_types(*field_type, types::I64, table).len();
                let end = offset + count;
                fields.push(decode(
                    slice(params, offset, end, "struct", context)?,
                    *field_type,
                    table,
                    context,
                )?);
                offset = end;
            }
            ensure_consumed(offset, params.len(), "struct", context)?;
            Ok(CompiledValue::Struct { fields, ty })
        }
        (Type::Enum(id), [tag, payload @ ..]) => {
            let mut offset = 0;
            let mut fields = Vec::new();
            for variant in table.enum_variants(id) {
                for (_, field_type) in &variant.fields {
                    let count = abi_types(*field_type, types::I64, table).len();
                    let end = offset + count;
                    fields.push(decode(
                        slice(payload, offset, end, "enum", context)?,
                        *field_type,
                        table,
                        context,
                    )?);
                    offset = end;
                }
            }
            ensure_consumed(offset, payload.len(), "enum", context)?;
            Ok(CompiledValue::Enum {
                tag: *tag,
                fields,
                ty,
            })
        }
        (Type::Option(id), [tag, payload @ ..]) => {
            let inner = table.option_type(id);
            let fields = decode(payload, inner, table, context).map(|value| vec![value])?;
            Ok(CompiledValue::Enum {
                tag: *tag,
                fields,
                ty,
            })
        }
        (Type::Result(id), [tag, payload @ ..]) => {
            let (ok, err) = table.result_types(id);
            let ok_count = abi_types(ok, types::I64, table).len();
            let ok_value = decode(
                slice(payload, 0, ok_count, "result", context)?,
                ok,
                table,
                context,
            )?;
            let err_value = decode(
                slice(payload, ok_count, payload.len(), "result", context)?,
                err,
                table,
                context,
            )?;
            Ok(CompiledValue::Enum {
                tag: *tag,
                fields: vec![ok_value, err_value],
                ty,
            })
        }
        (Type::Class(id), [pointer]) => Ok(CompiledValue::Class {
            pointer: *pointer,
            ty: Type::Class(id),
        }),
        (Type::Cown(id), [pointer]) => Ok(CompiledValue::Cown {
            pointer: *pointer,
            ty: Type::Cown(id),
        }),
        (Type::List(id), [pointer]) => Ok(CompiledValue::List {
            pointer: *pointer,
            ty: Type::List(id),
        }),
        (Type::MutList(id), [pointer]) => Ok(CompiledValue::MutList {
            pointer: *pointer,
            ty: Type::MutList(id),
        }),
        (Type::Map(id), [pointer]) => Ok(CompiledValue::Map {
            pointer: *pointer,
            ty: Type::Map(id),
        }),
        (Type::MutMap(id), [pointer]) => Ok(CompiledValue::MutMap {
            pointer: *pointer,
            ty: Type::MutMap(id),
        }),
        (Type::MutSet(id), [pointer]) => Ok(CompiledValue::MutSet {
            pointer: *pointer,
            ty: Type::MutSet(id),
        }),
        (ty @ (Type::Function(_) | Type::Dyn(_)), [code, environment]) => {
            Ok(CompiledValue::Function {
                code: *code,
                ty,
                environment: *environment,
            })
        }
        (ty, [value]) if ty.is_numeric() || ty == Type::Duration => {
            Ok(CompiledValue::Numeric { value: *value, ty })
        }
        _ => Err(Diagnostic::codegen(format!("invalid {context}"))),
    }
}

fn slice<'a>(
    values: &'a [Value],
    start: usize,
    end: usize,
    kind: &str,
    context: &str,
) -> Result<&'a [Value], Diagnostic> {
    values
        .get(start..end)
        .ok_or_else(|| Diagnostic::codegen(format!("invalid {kind} {context}")))
}

fn ensure_consumed(
    consumed: usize,
    available: usize,
    kind: &str,
    context: &str,
) -> Result<(), Diagnostic> {
    if consumed == available {
        Ok(())
    } else {
        Err(Diagnostic::codegen(format!("invalid {kind} {context}")))
    }
}

pub(super) fn value_from_params(
    params: &[Value],
    ty: Type,
    table: &TypeTable,
) -> Result<CompiledValue, Diagnostic> {
    decode(params, ty, table, "loop-carried value")
}

pub(super) fn result_arguments(value: CompiledValue, ty: Type) -> Result<Vec<Value>, Diagnostic> {
    if value_type(value.clone()) != ty {
        return Err(Diagnostic::codegen(
            "branches have different compiled values",
        ));
    }
    Ok(value_arguments(value))
}

pub(super) fn result_from_params(
    params: Vec<Value>,
    ty: Type,
    table: &TypeTable,
) -> Result<CompiledValue, Diagnostic> {
    decode(&params, ty, table, "result parameters")
}

/// Return byte offsets of managed pointer words in a flattened ABI value.
///
/// Aggregate values are stored in eight-byte slots even when an individual
/// component has a narrower Cranelift type. Keeping this layout calculation
/// next to `abi_types` makes it usable by both continuation cleanup and the
/// resumable handler payload transport.
pub(super) fn managed_pointer_offsets(ty: Type, base: usize, table: &TypeTable) -> Vec<usize> {
    managed_pointer_types(ty, base, table)
        .into_iter()
        .map(|(offset, _)| offset)
        .collect()
}

/// Return managed pointer words together with their leaf type.  Handler
/// environments use the leaf type to select the right drop glue for uniquely
/// owned class fields while all other managed values use `jk_drop`.
pub(super) fn managed_pointer_types(
    ty: Type,
    base: usize,
    table: &TypeTable,
) -> Vec<(usize, Type)> {
    match ty {
        Type::String
        | Type::Bytes
        | Type::BytesCursor
        | Type::MapCursor(_)
        | Type::MapKeyCursor(_)
        | Type::MapValueCursor(_)
        | Type::MutListCursor(_)
        | Type::MutMapCursor(_)
        | Type::MutSetCursor(_)
        | Type::MutBytes
        | Type::Batch
        | Type::SeqBuilder
        | Type::Hasher
        | Type::Native(_)
        | Type::CCallback
        | Type::Class(_)
        | Type::Cown(_)
        | Type::List(_)
        | Type::MutList(_)
        | Type::Map(_)
        | Type::MutMap(_)
        | Type::MutSet(_) => vec![(base, ty)],
        // A function value is represented by a code pointer followed by a
        // managed closure-environment pointer.
        Type::Function(_) | Type::Dyn(_) => vec![(base + 8, ty)],
        Type::Tuple(id) => {
            let mut offset = base;
            let mut pointers = Vec::new();
            for element in table.tuple_elements(id) {
                pointers.extend(managed_pointer_types(*element, offset, table));
                offset += abi_types(*element, cranelift_codegen::ir::types::I64, table).len() * 8;
            }
            pointers
        }
        Type::Struct(id) => {
            let mut offset = base;
            let mut pointers = Vec::new();
            for (_, field) in table.struct_fields(id) {
                pointers.extend(managed_pointer_types(*field, offset, table));
                offset += abi_types(*field, cranelift_codegen::ir::types::I64, table).len() * 8;
            }
            pointers
        }
        Type::Enum(id) => {
            let mut offset = base + 8;
            let mut pointers = Vec::new();
            for variant in table.enum_variants(id) {
                for (_, field) in &variant.fields {
                    pointers.extend(managed_pointer_types(*field, offset, table));
                    offset += abi_types(*field, cranelift_codegen::ir::types::I64, table).len() * 8;
                }
            }
            pointers
        }
        Type::Option(id) => managed_pointer_types(table.option_type(id), base + 8, table),
        Type::Result(id) => {
            let (ok, err) = table.result_types(id);
            let mut pointers = managed_pointer_types(ok, base + 8, table);
            let err_offset =
                base + 8 + abi_types(ok, cranelift_codegen::ir::types::I64, table).len() * 8;
            pointers.extend(managed_pointer_types(err, err_offset, table));
            pointers
        }
        Type::Unit
        | Type::CPtr(_)
        | Type::CMutPtr(_)
        | Type::CArray(_)
        | Type::CStr
        | Type::Bool
        | Type::Duration
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::F32
        | Type::F64
        | Type::Param(_)
        | Type::SelfType
        | Type::Associated(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_pointer_words_are_excluded_from_managed_cleanup() {
        let program = crate::syntax::parse_program(
            "fn f(value: (CStr, String, CPtr(UInt8), CMutPtr(UInt64))) {} fn main() {}",
        )
        .unwrap();
        let table = crate::sema::check_program(&program).unwrap();
        let ty = table
            .checked_annotation(&program.functions[0].parameters[0].ty, &[], None)
            .unwrap();
        assert_eq!(
            managed_pointer_types(ty, 0, &table),
            vec![(8, Type::String)]
        );
    }
}
