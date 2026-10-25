//! Typed access to initialized flat values; skip padding and inactive enum payloads.
use crate::{Error, ErrorCode, ErrorPhase, Type};
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidCall, message)
}
/// Store one admitted scalar lane without transferring ownership.
/// # Safety
/// `dst` points to writable storage of the exact scalar type and size; owner bits
/// are valid handles whose cleanup remains the caller's responsibility.
pub(crate) unsafe fn write_scalar(dst: *mut u8, ty: &Type, bits: u64) {
    match ty {
        Type::Bool | Type::Int8 | Type::UInt8 => dst.write(bits as u8),
        Type::Int16 | Type::UInt16 => dst.cast::<u16>().write_unaligned(bits as u16),
        Type::Int32 | Type::UInt32 | Type::Rune | Type::Float32 => {
            dst.cast::<u32>().write_unaligned(bits as u32)
        }
        Type::String | Type::Buffer(_) => dst.cast::<usize>().write_unaligned(bits as usize),
        _ => dst.cast::<u64>().write_unaligned(bits),
    }
}
/// Read an initialized flat value, skipping padding and inactive payloads.
/// # Safety
/// `src` is live storage of the exact reference-free, non-place `ty`; `raw` is
/// zero-filled and has `ty.scalar_types().len()` lanes. Owners remain borrowed.
pub(crate) unsafe fn read_memory(ty: &Type, src: *const u8, raw: &mut [u64]) -> Result<(), Error> {
    use crate::memory_layout::MemoryLayoutShape;
    let layout = ty
        .storage_layout()
        .map_err(|_| invalid("Invalid inline value layout"))?;
    match (ty, layout.shape) {
        (Type::Record(r), MemoryLayoutShape::Record(fields)) => {
            for (field, loc) in r.fields.iter().zip(fields) {
                read_memory(
                    &field.ty,
                    src.add(loc.offset),
                    &mut raw[field.offset..field.offset + field.ty.scalar_types().len()],
                )?;
            }
        }
        (Type::Tuple(t), MemoryLayoutShape::Tuple(fields)) => {
            let mut offset = 0;
            for (ty, loc) in t.elements.iter().zip(fields) {
                let end = offset + ty.scalar_types().len();
                read_memory(ty, src.add(loc.offset), &mut raw[offset..end])?;
                offset = end;
            }
        }
        (Type::Enum(e), MemoryLayoutShape::Enum { tag, variants }) => {
            let index = src.add(tag.offset).cast::<i32>().read_unaligned();
            let variant = usize::try_from(index)
                .ok()
                .and_then(|i| e.variants.get(i))
                .ok_or_else(|| invalid("Invalid inline enum tag"))?;
            raw[0] = index as u64;
            if let Some(payload) = &variant.payload {
                let loc = variants[index as usize].payload.unwrap();
                read_memory(
                    payload,
                    src.add(loc.offset),
                    &mut raw[variant.offset..variant.offset + payload.scalar_types().len()],
                )?;
            }
        }
        (Type::Unit, _) => {}
        _ => {
            raw[0] = match ty {
                Type::Int8 => src.cast::<i8>().read_unaligned() as u64,
                Type::Int16 => src.cast::<i16>().read_unaligned() as u64,
                Type::Int32 => src.cast::<i32>().read_unaligned() as u64,
                Type::Bool | Type::UInt8 => src.read() as u64,
                Type::UInt16 => src.cast::<u16>().read_unaligned() as u64,
                Type::UInt32 | Type::Rune | Type::Float32 => {
                    src.cast::<u32>().read_unaligned() as u64
                }
                Type::String | Type::Buffer(_) => src.cast::<usize>().read_unaligned() as u64,
                _ => src.cast::<u64>().read_unaligned(),
            }
        }
    }
    Ok(())
}
