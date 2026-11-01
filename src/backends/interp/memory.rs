//! Private interpreter memory operations. References never enter public values.
use super::*;
use crate::{runtime::arena::Arena, Type};

/// Each activation owns its stable local storage, including across instruction
/// slices. Loop re-entry reinitializes a place instead of growing the task arena.
pub(super) struct Locals {
    arena: Rc<std::cell::RefCell<Arena>>,
    pointers: HashMap<usize, std::ptr::NonNull<u8>>,
}

impl Default for Locals {
    fn default() -> Self {
        Self {
            arena: Rc::new(std::cell::RefCell::new(Arena::for_locals())),
            pointers: HashMap::new(),
        }
    }
}

impl Locals {
    /// Transferred cleanup hooks can outlive the activation that stored their bytes.
    pub(super) fn keep_alive(&self) -> Rc<std::cell::RefCell<Arena>> {
        self.arena.clone()
    }
}

pub(super) struct Field {
    slot: usize,
    offset: usize,
    ty: Type,
}
#[derive(Clone, Copy)]
pub(super) struct OwnerTransfer {
    pub source: usize,
    pub destination: usize,
    pub first: usize,
    pub count: usize,
}

pub(super) enum Instruction {
    ArenaNew {
        dst: usize,
    },
    EntryProject {
        dst: usize,
        reference: usize,
        cursor: bool,
        column: usize,
        load_owner: bool,
        dynamic: bool,
    },
    Project {
        dst: usize,
        reference: usize,
        offset: usize,
        load_owner: bool,
        dynamic: bool,
    },
    Alloc {
        local: bool,
        arena: Option<usize>,
        dst: usize,
        size: usize,
        align: usize,
        fields: Vec<Field>,
    },
    Read {
        reference: usize,
        fields: Vec<Field>,
        owner: Option<OwnerTransfer>,
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
                .storage_scalars()
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
        Op::EntryProject { dst, src, column } => Instruction::EntryProject {
            dst: dst.index,
            reference: src.index,
            cursor: matches!(
                src.ty.reference_type().unwrap().target,
                Type::BufferPairIterator(_)
            ),
            column: *column,
            dynamic: matches!(dst.ty.reference_type().unwrap().target, Type::DynTrait(_)),
            load_owner: dst.ty.reference_type().unwrap().target.is_owner()
                && !dst.ty.reference_type().unwrap().target.is_inline_owner(),
        },
        Op::Project { dst, src, .. } | Op::PatternProject { dst, src, .. } => {
            let offset = match op {
                Op::PatternProject { offset, .. } => *offset,
                Op::Project { path, .. } => {
                    src.ty
                        .reference_type()
                        .unwrap()
                        .target
                        .storage_field_path(path)
                        .map_err(layout_error)?
                        .0
                }
                _ => unreachable!(),
            };
            Instruction::Project {
                dst: dst.index,
                reference: src.index,
                offset,
                dynamic: matches!(dst.ty.reference_type().unwrap().target, Type::DynTrait(_)),
                load_owner: dst.ty.reference_type().unwrap().target.is_owner()
                    && !dst.ty.reference_type().unwrap().target.is_inline_owner(),
            }
        }
        Op::ArenaNew { dst } => Instruction::ArenaNew { dst: dst.index },
        Op::Alloc { dst, value, target }
        | Op::Local { dst, value, target }
        | Op::ArenaAlloc {
            dst, value, target, ..
        } => {
            let layout = target.storage_layout().map_err(layout_error)?;
            Instruction::Alloc {
                local: matches!(op, Op::Local { .. }),
                arena: match op {
                    Op::ArenaAlloc { arena, .. } => Some(arena.index),
                    _ => None,
                },
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
        }
        | Op::ReadOwner {
            dst,
            reference,
            path,
            ..
        } => {
            let (base, target) = reference
                .ty
                .reference_type()
                .unwrap()
                .target
                .storage_field_path(path)
                .map_err(layout_error)?;
            Instruction::Read {
                owner: if let Op::ReadOwner {
                    owner,
                    ownership_path,
                    ..
                } = op
                {
                    let (first, count) = owner.owner_range(&owner.field_path(ownership_path));
                    Some(OwnerTransfer {
                        source: owner.owner_key(),
                        destination: dst.as_ref().unwrap().owner_key(),
                        first,
                        count,
                    })
                } else {
                    None
                },
                reference: reference.index,
                fields: fields(&target, dst, base)?,
            }
        }
        Op::WriteMemory {
            reference,
            path,
            value,
            target,
        }
        | Op::StoreOwner {
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
                .storage_field_path(path)
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
    locals: &mut Locals,
) -> Result<(), Error> {
    // The shared IR validates types, complete initialization, permission and
    // lifetimes. Execution owns the arena until all of its frames are discarded.
    match op {
        Instruction::ArenaNew { dst } => {
            slots[*dst] =
                Value::Reference(crate::runtime::explicit_arena::new(arena)?.as_ptr().cast());
        }
        Instruction::EntryProject {
            dst,
            reference,
            cursor,
            column,
            load_owner,
            dynamic,
        } => {
            let Value::Reference(pointer) = slots[*reference] else {
                return Err(abi_error("Invalid entry reference"));
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive entry reference"));
            }
            // A compiler-authored cursor projection follows a successful advance;
            // otherwise the source is a stable Entry snapshot. Both source loans
            // retain the selected initialized element storage.
            let field = if *cursor {
                unsafe {
                    (*pointer.cast::<crate::runtime::buffer::BufferPairIterator>()).project(*column)
                }
            } else {
                let entry = unsafe { &*pointer.cast::<crate::runtime::buffer::BufferEntry>() };
                if *column == 0 {
                    entry.key
                } else {
                    entry.value
                }
            };
            slots[*dst] = Value::Reference(if *load_owner {
                unsafe { field.cast::<*mut u8>().read() }
            } else {
                field
            });
            if *dynamic {
                let Value::Reference(owner) = slots[*dst] else {
                    unreachable!()
                };
                unsafe {
                    super::buffers::dyn_borrow(slots, *dst, owner.cast())?;
                }
            }
        }
        Instruction::Project {
            dst,
            reference,
            offset,
            load_owner,
            dynamic,
        } => {
            let Value::Reference(pointer) = slots[*reference] else {
                return Err(abi_error("Invalid local reference"));
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive reference payload"));
            }
            // Shared IR proves an initialized, live inline local field. Layout
            // keeps this pointer within its owner (one-past is valid for Unit).
            let field = unsafe { pointer.add(*offset) };
            slots[*dst] = Value::Reference(if *load_owner {
                unsafe { field.cast::<*mut u8>().read_unaligned() }
            } else {
                field
            });
            if *dynamic {
                let Value::Reference(owner) = slots[*dst] else {
                    unreachable!()
                };
                unsafe {
                    super::buffers::dyn_borrow(slots, *dst, owner.cast())?;
                }
            }
        }
        Instruction::Alloc {
            local,
            arena: explicit,
            dst,
            size,
            align,
            fields,
        } => {
            let pointer = if *local {
                if let Some(pointer) = locals.pointers.get(dst) {
                    *pointer
                } else {
                    let pointer = locals.arena.borrow_mut().allocate_local(*size, *align)?;
                    locals.pointers.insert(*dst, pointer);
                    pointer
                }
            } else if let Some(owner) = explicit {
                let Value::Reference(owner) = slots[*owner] else {
                    return Err(abi_error("Invalid explicit arena reference"));
                };
                unsafe { crate::runtime::explicit_arena::allocate(owner.cast(), *size, *align)? }
            } else {
                arena.allocate(*size, *align)?
            };
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
        Instruction::Read {
            reference, fields, ..
        } => {
            let Value::Reference(pointer) = slots[*reference] else {
                return Err(abi_error("Invalid arena reference"));
            };
            if pointer.is_null() {
                return Err(abi_error("Inactive reference payload"));
            }
            for field in fields {
                let ptr = unsafe { pointer.add(field.offset) };
                slots[field.slot] = unsafe { read_scalar(ptr, &field.ty) };
            }
        }
    }
    Ok(())
}
unsafe fn write(pointer: *mut u8, fields: &[Field], slots: &[Value]) {
    for field in fields {
        let ptr = pointer.add(field.offset);
        write_scalar(ptr, &slots[field.slot]);
    }
}

/// Pointers and scalar kinds are supplied only by checked memory/buffer IR.
pub(super) unsafe fn read_scalar(ptr: *const u8, ty: &Type) -> Value {
    match ty {
        ty if ty.is_domain_capability() => Value::Named(ptr.cast::<u64>().read_unaligned()),
        Type::Float32 => Value::Float32(ptr.cast::<f32>().read_unaligned()),
        Type::Float64 => Value::Float64(ptr.cast::<f64>().read_unaligned()),
        Type::Int8 => Value::Int8(ptr.cast::<i8>().read_unaligned()),
        Type::Int16 => Value::Int16(ptr.cast::<i16>().read_unaligned()),
        Type::Int32 => Value::Int32(ptr.cast::<i32>().read_unaligned()),
        Type::Int64 => Value::Int64(ptr.cast::<i64>().read_unaligned()),
        Type::UInt8 => Value::UInt8(ptr.cast::<u8>().read_unaligned()),
        Type::UInt16 => Value::UInt16(ptr.cast::<u16>().read_unaligned()),
        Type::UInt32 => Value::UInt32(ptr.cast::<u32>().read_unaligned()),
        Type::Rune => Value::Rune(ptr.cast::<u32>().read_unaligned()),
        Type::UInt64 => Value::UInt64(ptr.cast::<u64>().read_unaligned()),
        Type::Bool => Value::Bool(ptr.read() != 0),
        Type::Function(_) => Value::Function(ptr.cast::<usize>().read_unaligned()),
        Type::Ptr => Value::Ptr(ptr.cast::<*mut u8>().read_unaligned()),
        Type::Reference(_)
        | Type::Buffer(_)
        | Type::String
        | Type::Resource(_)
        | Type::DynTrait(_) => Value::Reference(ptr.cast::<*mut u8>().read_unaligned()),
        _ => unreachable!("validated Copy memory"),
    }
}

pub(super) unsafe fn write_scalar(ptr: *mut u8, value: &Value) {
    match *value {
        Value::Function(value) => ptr.cast::<usize>().write_unaligned(value),
        Value::Float32(value) => ptr.cast::<f32>().write_unaligned(value),
        Value::Float64(value) => ptr.cast::<f64>().write_unaligned(value),
        Value::Int8(value) => ptr.cast::<i8>().write_unaligned(value),
        Value::Int16(value) => ptr.cast::<i16>().write_unaligned(value),
        Value::Int32(value) => ptr.cast::<i32>().write_unaligned(value),
        Value::Int64(value) => ptr.cast::<i64>().write_unaligned(value),
        Value::UInt8(value) => ptr.cast::<u8>().write_unaligned(value),
        Value::UInt16(value) => ptr.cast::<u16>().write_unaligned(value),
        Value::UInt32(value) => ptr.cast::<u32>().write_unaligned(value),
        Value::Rune(value) => ptr.cast::<u32>().write_unaligned(value),
        Value::UInt64(value) | Value::Named(value) => ptr.cast::<u64>().write_unaligned(value),
        Value::Bool(value) => ptr.write(u8::from(value)),
        Value::Ptr(value) | Value::Reference(value) => ptr.cast::<*mut u8>().write_unaligned(value),
        _ => unreachable!("validated Copy memory"),
    }
}

/// Snapshot a complete owner for destructor metadata, separate from Copy locals.
pub(super) fn snapshot(
    key: usize,
    value: &crate::core::ir::Slot,
    slots: &[Value],
    locals: &mut Locals,
) -> Result<*mut u8, Error> {
    let layout = value
        .ty
        .storage_layout()
        .map_err(|e| abi_error(e.to_string()))?;
    let pointer = if let Some(pointer) = locals.pointers.get(&key) {
        *pointer
    } else {
        let pointer = locals
            .arena
            .borrow_mut()
            .allocate(layout.size, layout.align)?;
        locals.pointers.insert(key, pointer);
        pointer
    };
    for ((offset, _), scalar) in value
        .ty
        .storage_scalars()
        .unwrap()
        .into_iter()
        .zip(value.scalars())
    {
        unsafe {
            write_scalar(pointer.as_ptr().add(offset), &slots[scalar.index]);
        }
    }
    Ok(pointer.as_ptr())
}

pub(super) fn moved_owner(op: &Instruction) -> Option<OwnerTransfer> {
    match op {
        Instruction::Read { owner, .. } => *owner,
        _ => None,
    }
}
