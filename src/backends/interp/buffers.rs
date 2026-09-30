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
fn element(value: &Value) -> Result<i32, Error> {
    match value {
        Value::Int32(n) => Ok(*n),
        _ => Err(runtime_error(
            ErrorCode::InvalidState,
            "Invalid buffer element",
        )),
    }
}

pub(super) fn execute(op: &BufferOp, slots: &mut [Value], arena: &mut Arena) -> Result<(), Error> {
    match op {
        BufferOp::Move { dst, src } => {
            if dst.index != src.index {
                slots[dst.index] = std::mem::replace(&mut slots[src.index], Value::Unit);
            }
        }
        BufferOp::Borrow { dst, src } => {
            slots[dst.index] = slots[src.index].clone();
        }
        BufferOp::End(owner) => {
            slots[owner.index] = Value::Unit;
        }
        BufferOp::Storage {
            intrinsic,
            args,
            dst,
        } => {
            let arg = |i: usize| &slots[args[i].index];
            // Only shared-IR-checked programs create these instructions. All
            // pointers are stable and stay live across internal time slices.
            let value = unsafe {
                match intrinsic {
                    BufferIntrinsic::Allocate => {
                        Value::Reference(Buffer::allocate(arena, count(arg(0))?)?.as_ptr().cast())
                    }
                    BufferIntrinsic::CapacityLimit => Value::UInt64(Buffer::capacity_limit()),
                    BufferIntrinsic::Len => Value::UInt64((*pointer(arg(0))?).len()),
                    BufferIntrinsic::Capacity => Value::UInt64((*pointer(arg(0))?).capacity()),
                    BufferIntrinsic::Load => {
                        Value::Int32((*pointer(arg(0))?).load(count(arg(1))?)?)
                    }
                    BufferIntrinsic::Store => {
                        (*pointer(arg(0))?).store(count(arg(1))?, element(arg(2))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::InitializeNext => {
                        (*pointer(arg(0))?).initialize_next(element(arg(1))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::Truncate => {
                        (*pointer(arg(0))?).truncate(count(arg(1))?)?;
                        Value::Unit
                    }
                    BufferIntrinsic::Replace => {
                        Buffer::replace(pointer(arg(0))?, pointer(arg(1))?)?;
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
