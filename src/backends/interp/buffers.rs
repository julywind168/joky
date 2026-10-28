//! Execute checked storage IR against the task-owned runtime control blocks.
use super::*;
use crate::{
    parser::ast::BufferIntrinsic,
    runtime::{
        arena::Arena,
        buffer::{Buffer, BufferEntry, BufferIterator, BufferPairIterator},
    },
};

pub(super) unsafe fn dyn_borrow(
    slots: &mut [Value],
    dst: usize,
    owner: *mut Buffer,
) -> Result<(), Error> {
    let Some(crate::runtime::buffer::DynDispatch::Interpreted { table, indirect }) =
        &(*owner).dynamic
    else {
        return Err(abi_error("Invalid owning dyn table"));
    };
    let data = (*owner).load(0)?;
    slots[dst] = Value::Reference(if *indirect {
        data.cast::<*mut u8>().read()
    } else {
        data
    });
    slots[dst + 1] = Value::DynTable(table.clone());
    Ok(())
}
fn pointer(value: &Value) -> Result<*mut Buffer, Error> {
    match value {
        Value::Reference(p) if !p.is_null() => Ok(p.cast()),
        _ => Err(runtime_error(
            ErrorCode::InvalidState,
            "Invalid buffer handle",
        )),
    }
}
fn count(value: &Value) -> Result<u64, Error> {
    match value {
        Value::UInt64(n) => Ok(*n),
        _ => Err(runtime_error(
            ErrorCode::InvalidState,
            "Invalid buffer count",
        )),
    }
}
/// Layout and scalar offsets are resolved once, never on the execution path.
pub(super) struct Instruction {
    pub(super) op: BufferOp,
    layout: Option<crate::memory_layout::MemoryLayout>,
    fields: Vec<(usize, crate::Type)>,
    pair_offsets: Option<[usize; 2]>,
    program: Arc<[crate::runtime::place::CopyNode]>,
}

pub(super) fn lower(op: &BufferOp, layouts: &mut places::Layouts) -> Result<Instruction, Error> {
    let (layout, fields) = if let BufferOp::Storage { element, .. } = op {
        let invalid = |e: crate::memory_layout::MemoryLayoutError| {
            runtime_error(ErrorCode::InvalidState, e.to_string())
        };
        (
            Some(element.storage_layout().map_err(invalid)?),
            if element.is_place() {
                Ok(Vec::new())
            } else if element.is_owner() {
                element.storage_scalars()
            } else {
                element.memory_scalars()
            }
            .map_err(invalid)?,
        )
    } else if let BufferOp::Replace {
        dst, owner, path, ..
    } = op
    {
        let (base, _) = owner
            .ty
            .storage_field_path(path)
            .expect("validated owner field");
        (
            None,
            dst.ty
                .storage_scalars()
                .expect("validated owner storage")
                .into_iter()
                .map(|(offset, ty)| (base + offset, ty))
                .collect(),
        )
    } else {
        (None, Vec::new())
    };
    Ok(Instruction {
        program: match op {
            BufferOp::Storage { element, .. } if element.is_place() => {
                layouts.get(element).program.clone()
            }
            _ => Arc::default(),
        },
        pair_offsets: match op {
            BufferOp::Storage {
                intrinsic: BufferIntrinsic::PairIter | BufferIntrinsic::PairIterMut,
                element,
                ..
            } => BufferIntrinsic::pair_offsets(element),
            _ => None,
        },
        op: op.clone(),
        layout,
        fields,
    })
}

pub(super) fn execute(
    instruction: &Instruction,
    slots: &mut [Value],
    arena: &mut Arena,
    cleanups: &mut crate::runtime::cleanup::Cleanups,
) -> Result<(), Error> {
    match &instruction.op {
        BufferOp::Move { dst, src }
        | BufferOp::Extract { dst, src, .. }
        | BufferOp::Detach { dst, src, .. } => {
            if dst.index != src.index {
                for (dst, src) in dst.scalars().zip(src.scalars()) {
                    slots[dst.index] = std::mem::replace(&mut slots[src.index], Value::Unit);
                }
            }
        }
        BufferOp::Replace {
            dst, src, place, ..
        } => {
            let Value::Reference(pointer) = slots[place.index] else {
                unreachable!("validated owner place")
            };
            for (((offset, _), dst), src) in instruction
                .fields
                .iter()
                .zip(dst.scalars())
                .zip(src.scalars())
            {
                let value = std::mem::replace(&mut slots[src.index], Value::Unit);
                unsafe {
                    memory::write_scalar(pointer.add(*offset), &value);
                }
                slots[dst.index] = value;
            }
        }
        BufferOp::SwapReferences {
            left,
            right,
            target,
        } => {
            let left = pointer(&slots[left.index])?;
            let right = pointer(&slots[right.index])?;
            unsafe {
                if target.is_owner() && !target.is_inline_owner() {
                    std::ptr::swap(left, right);
                } else {
                    std::ptr::swap_nonoverlapping(
                        left.cast::<u8>(),
                        right.cast::<u8>(),
                        target.storage_layout().unwrap().size,
                    );
                }
            }
        }
        BufferOp::Swap {
            reference,
            value,
            place,
        } => {
            let left = pointer(&slots[reference.index])?;
            let right = pointer(
                &slots[place
                    .as_ref()
                    .map_or_else(|| value.owner_key(), |p| p.index)],
            )?;
            unsafe {
                if place.is_some() {
                    std::ptr::swap_nonoverlapping(
                        left.cast::<u8>(),
                        right.cast::<u8>(),
                        value.ty.storage_layout().unwrap().size,
                    );
                } else {
                    std::ptr::swap(left, right);
                }
            }
        }
        BufferOp::Borrow { dst, src, place } => {
            let value = slots[place.as_ref().map_or_else(|| src.owner_key(), |p| p.index)].clone();
            if dst.ty.dyn_interface().is_some() {
                let mut owner = pointer(&value)?;
                if place.is_some() {
                    owner = unsafe { owner.cast::<*mut Buffer>().read() };
                }
                unsafe {
                    dyn_borrow(slots, dst.index, owner)?;
                }
            } else {
                slots[dst.index] = value;
            }
        }
        BufferOp::Access { .. } => unreachable!("owner access markers are erased after checking"),
        BufferOp::End(owner) => {
            // Copy fields may be the result of a temporary projection. Ending
            // the owner releases its resources, not those independent values.
            for scalar in owner.scalars().filter(|s| s.ty.is_owner_lane()) {
                slots[scalar.index] = Value::Unit;
            }
        }
        BufferOp::Storage {
            intrinsic,
            args,
            dst,
            element,
        } => {
            let arg = |i: usize| &slots[args[i].index];
            let layout = instruction.layout.as_ref().unwrap();
            // Handles stay live across internal time slices. Each instruction
            // completes its checked read/write/commit before advancing the PC.
            let value = unsafe {
                match intrinsic {
                    BufferIntrinsic::PairIter | BufferIntrinsic::PairIterMut => {
                        let owner = pointer(arg(0))?.cast::<u8>();
                        let [key, value] = instruction.pair_offsets.unwrap();
                        let keys = owner.add(key).cast::<*const Buffer>().read();
                        let values = owner.add(value).cast::<*const Buffer>().read();
                        Value::Reference(
                            BufferPairIterator::new(arena, &*keys, &*values)?
                                .as_ptr()
                                .cast(),
                        )
                    }
                    BufferIntrinsic::PairAdvance => {
                        Value::Bool((*pointer(arg(0))?.cast::<BufferPairIterator>()).advance())
                    }
                    BufferIntrinsic::PairCurrent | BufferIntrinsic::PairCurrentMut => {
                        Value::Reference(
                            (*pointer(arg(0))?.cast::<BufferPairIterator>())
                                .current(arena)?
                                .as_ptr()
                                .cast(),
                        )
                    }
                    BufferIntrinsic::Iter
                    | BufferIntrinsic::IterMut
                    | BufferIntrinsic::SliceIter
                    | BufferIntrinsic::SliceIterMut => {
                        Value::Reference((*pointer(arg(0))?).iterator(arena)?.as_ptr().cast())
                    }
                    BufferIntrinsic::IterAdvance => {
                        Value::Bool((*pointer(arg(0))?.cast::<BufferIterator>()).advance())
                    }
                    BufferIntrinsic::Allocate => Value::Reference(
                        Buffer::allocate(arena, count(arg(0))?, layout.size, layout.align)?
                            .as_ptr()
                            .cast(),
                    ),
                    BufferIntrinsic::Transfer => {
                        Buffer::transfer(pointer(arg(0))?, pointer(arg(1))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::CapacityLimit => {
                        Value::UInt64(Buffer::capacity_limit(layout.size))
                    }
                    BufferIntrinsic::Len | BufferIntrinsic::SliceLen => {
                        Value::UInt64((*pointer(arg(0))?).len())
                    }
                    BufferIntrinsic::BorrowRef
                    | BufferIntrinsic::BorrowMutRef
                    | BufferIntrinsic::SliceBorrowRef
                    | BufferIntrinsic::SliceBorrowMutRef
                    | BufferIntrinsic::IterCurrent
                    | BufferIntrinsic::IterCurrentMut
                    | BufferIntrinsic::EntryKey
                    | BufferIntrinsic::EntryValue
                    | BufferIntrinsic::EntryValueMut => {
                        let element = intrinsic.borrowed_element(element);
                        let data = if matches!(
                            intrinsic,
                            BufferIntrinsic::EntryKey
                                | BufferIntrinsic::EntryValue
                                | BufferIntrinsic::EntryValueMut
                        ) {
                            let entry = &*pointer(arg(0))?.cast::<BufferEntry>();
                            if *intrinsic == BufferIntrinsic::EntryKey {
                                entry.key
                            } else {
                                entry.value
                            }
                        } else if matches!(
                            intrinsic,
                            BufferIntrinsic::IterCurrent | BufferIntrinsic::IterCurrentMut
                        ) {
                            (*pointer(arg(0))?.cast::<BufferIterator>()).current()?
                        } else {
                            (*pointer(arg(0))?).load(count(arg(1))?)?
                        };
                        if matches!(element, Type::DynTrait(_)) {
                            dyn_borrow(
                                slots,
                                dst.as_ref().unwrap().index,
                                data.cast::<*mut Buffer>().read(),
                            )?;
                            return Ok(());
                        }
                        Value::Reference(if !element.is_owner() || element.is_inline_owner() {
                            data
                        } else {
                            data.cast::<*mut u8>().read()
                        })
                    }
                    BufferIntrinsic::Borrow
                    | BufferIntrinsic::BorrowMut
                    | BufferIntrinsic::SliceBorrow
                    | BufferIntrinsic::SliceBorrowMut => {
                        Value::Reference((*pointer(arg(0))?).load(count(arg(1))?)?)
                    }
                    BufferIntrinsic::Slice
                    | BufferIntrinsic::SliceMut
                    | BufferIntrinsic::Subslice
                    | BufferIntrinsic::SubsliceMut => Value::Reference(
                        (*pointer(arg(0))?)
                            .slice(arena, count(arg(1))?, count(arg(2))?)?
                            .as_ptr()
                            .cast(),
                    ),
                    BufferIntrinsic::SwapElements => {
                        (*pointer(arg(0))?).swap_elements(count(arg(1))?, count(arg(2))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::Capacity => Value::UInt64((*pointer(arg(0))?).capacity()),
                    BufferIntrinsic::Load
                    | BufferIntrinsic::TakeLast
                    | BufferIntrinsic::ElementLoad
                    | BufferIntrinsic::SliceLoad => {
                        let data = if *intrinsic == BufferIntrinsic::TakeLast {
                            (*pointer(arg(0))?).take_last()?
                        } else if *intrinsic == BufferIntrinsic::ElementLoad {
                            pointer(arg(0))?.cast::<u8>()
                        } else {
                            (*pointer(arg(0))?).load(count(arg(1))?)?
                        };
                        if let Some(dst) = dst {
                            if *intrinsic == BufferIntrinsic::TakeLast && dst.ty.is_owner() {
                                let entry = (*pointer(arg(0))?)
                                    .element_entry(data)
                                    .expect("owning element cleanup");
                                cleanups.insert(dst.owner_key(), entry);
                            }
                            if dst.ty.is_place() {
                                let p = cleanups.places.materialize(
                                    arena,
                                    dst.index,
                                    layout.size,
                                    layout.align,
                                )?;
                                crate::runtime::place::copy(p.as_ptr(), data, &instruction.program);
                                slots[dst.index] = Value::Place(p.as_ptr());
                            }
                            for ((offset, ty), dst) in instruction.fields.iter().zip(dst.scalars())
                            {
                                slots[dst.index] = memory::read_scalar(data.add(*offset), ty);
                            }
                        }
                        return Ok(());
                    }
                    BufferIntrinsic::Store
                    | BufferIntrinsic::InitializeNext
                    | BufferIntrinsic::ElementStore
                    | BufferIntrinsic::SliceStore => {
                        let buffer = pointer(arg(0))?;
                        let initializing = *intrinsic == BufferIntrinsic::InitializeNext;
                        let data = if initializing {
                            (*buffer).initialize_next()?
                        } else if *intrinsic == BufferIntrinsic::ElementStore {
                            buffer.cast::<u8>()
                        } else if *intrinsic == BufferIntrinsic::Store {
                            (*buffer).prepare_store(arena, count(arg(1))?)?
                        } else {
                            (*buffer).load(count(arg(1))?)?
                        };
                        let value = args.last().unwrap();
                        if value.ty.is_place() {
                            crate::runtime::place::copy(
                                data,
                                places::pointer(&slots[value.index]),
                                &instruction.program,
                            );
                        }
                        for ((offset, _), src) in instruction.fields.iter().zip(value.scalars()) {
                            memory::write_scalar(data.add(*offset), &slots[src.index]);
                        }
                        if initializing {
                            (*buffer).commit_next();
                        }
                        Value::Unit
                    }
                    BufferIntrinsic::Truncate => {
                        (*pointer(arg(0))?).truncate_owned(arena, count(arg(1))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::Release => {
                        Buffer::release(arena, pointer(arg(0))?)?;
                        slots[args[0].index] = Value::Unit;
                        Value::Unit
                    }
                    BufferIntrinsic::Replace => {
                        Buffer::replace(arena, pointer(arg(0))?, pointer(arg(1))?)?;
                        slots[args[1].index] = Value::Unit;
                        Value::Unit
                    }
                }
            };
            if let Some(dst) = dst {
                slots[dst.index] = value;
            }
        }
    }
    Ok(())
}
