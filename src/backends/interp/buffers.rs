//! Execute checked storage IR against the task-owned runtime control blocks.
use super::*;
use crate::{
    parser::ast::BufferIntrinsic,
    runtime::{arena::Arena, buffer::Buffer},
};

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
    op: BufferOp,
    layout: Option<crate::memory_layout::MemoryLayout>,
    fields: Vec<(usize, crate::Type)>,
}

pub(super) fn lower(op: &BufferOp) -> Result<Instruction, Error> {
    let (layout, fields) = if let BufferOp::Storage { element, .. } = op {
        let invalid = |e: crate::memory_layout::MemoryLayoutError| {
            runtime_error(ErrorCode::InvalidState, e.to_string())
        };
        (
            Some(element.memory_layout().map_err(invalid)?),
            element.memory_scalars().map_err(invalid)?,
        )
    } else {
        (None, Vec::new())
    };
    Ok(Instruction {
        op: op.clone(),
        layout,
        fields,
    })
}

pub(super) fn execute(
    instruction: &Instruction,
    slots: &mut [Value],
    arena: &mut Arena,
) -> Result<(), Error> {
    match &instruction.op {
        BufferOp::Move { dst, src } => {
            if dst.index != src.index {
                slots[dst.index] = std::mem::replace(&mut slots[src.index], Value::Unit);
            }
        }
        BufferOp::Borrow { dst, src } => slots[dst.index] = slots[src.index].clone(),
        BufferOp::End(owner) => slots[owner.index] = Value::Unit,
        BufferOp::Storage {
            intrinsic,
            args,
            dst,
            ..
        } => {
            let arg = |i: usize| &slots[args[i].index];
            let layout = instruction.layout.as_ref().unwrap();
            // Handles stay live across internal time slices. Each instruction
            // completes its checked read/write/commit before advancing the PC.
            let value = unsafe {
                match intrinsic {
                    BufferIntrinsic::Allocate => Value::Reference(
                        Buffer::allocate(arena, count(arg(0))?, layout.size, layout.align)?
                            .as_ptr()
                            .cast(),
                    ),
                    BufferIntrinsic::CapacityLimit => {
                        Value::UInt64(Buffer::capacity_limit(layout.size))
                    }
                    BufferIntrinsic::Len | BufferIntrinsic::SliceLen => {
                        Value::UInt64((*pointer(arg(0))?).len())
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
                    BufferIntrinsic::Capacity => Value::UInt64((*pointer(arg(0))?).capacity()),
                    BufferIntrinsic::Load
                    | BufferIntrinsic::ElementLoad
                    | BufferIntrinsic::SliceLoad => {
                        let data = if *intrinsic == BufferIntrinsic::ElementLoad {
                            pointer(arg(0))?.cast::<u8>()
                        } else {
                            (*pointer(arg(0))?).load(count(arg(1))?)?
                        };
                        if let Some(dst) = dst {
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
                        } else {
                            (*buffer).load(count(arg(1))?)?
                        };
                        let value = args.last().unwrap();
                        for ((offset, _), src) in instruction.fields.iter().zip(value.scalars()) {
                            memory::write_scalar(data.add(*offset), &slots[src.index]);
                        }
                        if initializing {
                            (*buffer).commit_next();
                        }
                        Value::Unit
                    }
                    BufferIntrinsic::Truncate => {
                        (*pointer(arg(0))?).truncate(count(arg(1))?)?;
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
