//! Private interpreter memory operations. References never enter public values.
use super::*;
use crate::{runtime::arena::Arena, Type};

pub(super) struct Field {
    slot: usize,
    offset: usize,
    ty: Type,
}
pub(super) enum Instruction {
    Alloc {
        dst: usize,
        size: usize,
        align: usize,
        fields: Vec<Field>,
    },
    Read {
        reference: usize,
        fields: Vec<Field>,
    },
    Write {
        reference: usize,
        fields: Vec<Field>,
    },
}

pub(super) fn lower(op: &Op) -> Result<Instruction, Error> {
    let layout_error = |e: crate::core::types::memory_layout::MemoryLayoutError| {
        runtime_error(ErrorCode::InvalidState, e.to_string())
    };
    let fields = |target: &Type,
                  slot: &Option<crate::core::ir::Slot>,
                  base: usize|
     -> Result<Vec<Field>, Error> {
        Ok(if let Some(slot) = slot {
            target
                .memory_scalars()
                .map_err(layout_error)?
                .into_iter()
                .zip(slot.scalars())
                .map(|((offset, ty), slot)| Field {
                    slot: slot.index,
                    offset: base + offset,
                    ty,
                })
                .collect()
        } else {
            Vec::new()
        })
    };
    Ok(match op {
        Op::Alloc { dst, value, target } => {
            let layout = target.memory_layout().map_err(layout_error)?;
            Instruction::Alloc {
                dst: dst.index,
                size: layout.size,
                align: layout.align,
                fields: fields(target, value, 0)?,
            }
        }
        Op::ReadMemory {
            dst,
            reference,
            path,
        } => {
            let (base, target) = reference
                .ty
                .reference_type()
                .unwrap()
                .target
                .memory_field_path(path)
                .map_err(layout_error)?;
            Instruction::Read {
                reference: reference.index,
                fields: fields(&target, dst, base)?,
            }
        }
        Op::WriteMemory {
            reference,
            path,
            value,
            target,
        } => {
            let (base, _) = reference
                .ty
                .reference_type()
                .unwrap()
                .target
                .memory_field_path(path)
                .map_err(layout_error)?;
            Instruction::Write {
                reference: reference.index,
                fields: fields(target, value, base)?,
            }
        }
        _ => unreachable!(),
    })
}

pub(super) fn execute(
    op: &Instruction,
    slots: &mut [Value],
    arena: &mut Arena,
) -> Result<(), Error> {
    // The shared IR validates types, complete initialization, permission and
    // lifetimes. Execution owns the arena until all of its frames are discarded.
    match op {
        Instruction::Alloc {
            dst,
            size,
            align,
            fields,
        } => {
            let pointer = arena.allocate(*size, *align)?;
            unsafe {
                write(pointer.as_ptr(), fields, slots);
            }
            slots[*dst] = Value::Reference(pointer.as_ptr());
        }
        Instruction::Write { reference, fields } => {
            let Value::Reference(pointer) = slots[*reference] else {
                return Err(abi_error("Invalid arena reference"));
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive reference payload"));
            }
            unsafe {
                write(pointer, fields, slots);
            }
        }
        Instruction::Read { reference, fields } => {
            let Value::Reference(pointer) = slots[*reference] else {
                return Err(abi_error("Invalid arena reference"));
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive reference payload"));
            }
            for field in fields {
                let ptr = unsafe { pointer.add(field.offset) };
                slots[field.slot] = unsafe {
                    match field.ty {
                        Type::Int32 => Value::Int32(ptr.cast::<i32>().read_unaligned()),
                        Type::Int64 => Value::Int64(ptr.cast::<i64>().read_unaligned()),
                        Type::UInt32 => Value::UInt32(ptr.cast::<u32>().read_unaligned()),
                        Type::UInt64 => Value::UInt64(ptr.cast::<u64>().read_unaligned()),
                        Type::Bool => Value::Bool(ptr.read() != 0),
                        Type::Reference(_) => {
                            Value::Reference(ptr.cast::<*mut u8>().read_unaligned())
                        }
                        _ => unreachable!("validated Copy memory"),
                    }
                };
            }
        }
    }
    Ok(())
}
unsafe fn write(pointer: *mut u8, fields: &[Field], slots: &[Value]) {
    for field in fields {
        let ptr = pointer.add(field.offset);
        match slots[field.slot] {
            Value::Int32(value) => ptr.cast::<i32>().write_unaligned(value),
            Value::Int64(value) => ptr.cast::<i64>().write_unaligned(value),
            Value::UInt32(value) => ptr.cast::<u32>().write_unaligned(value),
            Value::UInt64(value) => ptr.cast::<u64>().write_unaligned(value),
            Value::Bool(value) => ptr.write(u8::from(value)),
            Value::Reference(value) => ptr.cast::<*mut u8>().write_unaligned(value),
            _ => unreachable!("validated Copy memory"),
        }
    }
}
