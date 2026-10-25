//! Host conversion of checked inline Copy values, following physical fields only.
use super::{invalid, InstanceValue, TaskValue};
use crate::runtime::value_storage::{read_memory, write_scalar};
use crate::{memory_layout::MemoryLayoutShape, Error, Type};

/// # Safety
/// The destination has the exact checked layout; the host value already matches it.
pub(super) unsafe fn write(value: &TaskValue, ty: &Type, dst: *mut u8) -> Result<(), Error> {
    match value {
        TaskValue::Scalar(value) => {
            if *ty != Type::Unit {
                write_scalar(dst, ty, value.bits());
            }
        }
        TaskValue::FixedArray { elements, .. } => {
            let (element, _) = ty.fixed_array_element_type().unwrap();
            let layout = ty
                .memory_layout()
                .map_err(|_| invalid("Invalid Copy layout"))?;
            let MemoryLayoutShape::Array { stride, .. } = layout.shape else {
                unreachable!()
            };
            for (index, value) in elements.iter().enumerate() {
                write(value, element, dst.add(index * stride))?;
            }
        }
        TaskValue::Tuple(values) | TaskValue::Record { fields: values, .. } => {
            for (index, value) in values.iter().enumerate() {
                let (offset, child) = ty.place_child(index);
                write(value, &child, dst.add(offset))?;
            }
        }
        TaskValue::Enum {
            variant, payload, ..
        } => {
            dst.cast::<i32>().write_unaligned(*variant as i32);
            if let Some(payload) = payload {
                let (offset, child) = ty.place_child(*variant);
                write(payload, &child, dst.add(offset))?;
            }
        }
        _ => return Err(invalid("Expected inline Copy value")),
    }
    Ok(())
}

unsafe fn children(
    ty: &Type,
    src: *const u8,
    mut visit: impl FnMut(&Type, *const u8) -> Result<(), Error>,
) -> Result<(), Error> {
    match ty {
        Type::FixedArray(a) => {
            let layout = ty
                .memory_layout()
                .map_err(|_| invalid("Invalid Copy layout"))?;
            let MemoryLayoutShape::Array { length, stride, .. } = layout.shape else {
                unreachable!()
            };
            for index in 0..length as usize {
                visit(&a.element, src.add(index * stride))?;
            }
        }
        Type::Tuple(t) => {
            for i in 0..t.elements.len() {
                let (offset, ty) = ty.place_child(i);
                visit(&ty, src.add(offset))?;
            }
        }
        Type::Record(r) => {
            for i in 0..r.fields.len() {
                let (offset, ty) = ty.place_child(i);
                visit(&ty, src.add(offset))?;
            }
        }
        Type::Enum(e) => {
            let index = src.cast::<i32>().read_unaligned() as usize;
            let variant = e
                .variants
                .get(index)
                .ok_or_else(|| invalid("Invalid Copy enum tag"))?;
            if variant.payload.is_some() {
                let (offset, ty) = ty.place_child(index);
                visit(&ty, src.add(offset))?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// # Safety
/// Source has the exact checked Copy layout and stays live throughout traversal.
pub(super) unsafe fn measure(ty: &Type, src: *const u8) -> Result<usize, Error> {
    let mut bytes = std::mem::size_of::<TaskValue>();
    children(ty, src, |ty, src| {
        bytes = bytes
            .checked_add(measure(ty, src)?)
            .ok_or_else(|| invalid("Copy result size overflow"))?;
        Ok(())
    })?;
    Ok(bytes)
}
/// # Safety
/// Source has the exact checked Copy layout and stays live throughout conversion.
pub(super) unsafe fn read(ty: &Type, src: *const u8) -> Result<TaskValue, Error> {
    if let Type::Enum(e) = ty {
        let index = src.cast::<i32>().read_unaligned() as usize;
        let variant = e
            .variants
            .get(index)
            .ok_or_else(|| invalid("Invalid Copy enum tag"))?;
        let payload = if variant.payload.is_some() {
            let (offset, child) = ty.place_child(index);
            Some(Box::new(read(&child, src.add(offset))?))
        } else {
            None
        };
        return Ok(TaskValue::Enum {
            ty: ty.clone(),
            variant: index,
            payload,
        });
    }
    let count = match ty {
        Type::FixedArray(_) => ty.fixed_array_element_type().unwrap().1 as usize,
        Type::Tuple(t) => t.elements.len(),
        Type::Record(r) => r.fields.len(),
        _ => 0,
    };
    let mut values = Vec::with_capacity(count);
    children(ty, src, |ty, src| {
        values.push(read(ty, src)?);
        Ok(())
    })?;
    Ok(match ty {
        Type::FixedArray(_) => TaskValue::FixedArray {
            ty: ty.clone(),
            elements: values,
        },
        Type::Tuple(_) => TaskValue::Tuple(values),
        Type::Record(_) => TaskValue::Record {
            ty: ty.clone(),
            fields: values,
        },
        _ => {
            let mut words = [0];
            read_memory(ty, src, &mut words)?;
            TaskValue::Scalar(
                InstanceValue::from_bits(ty, words[0])
                    .ok_or_else(|| invalid("Invalid Copy scalar"))?,
            )
        }
    })
}
