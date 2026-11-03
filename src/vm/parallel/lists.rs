//! Host List sequences are converted through the trusted standard declaration.
//! Worker-to-worker packets keep the original three buffers and index topology.
use super::{arrays, invalid, TaskValue};
use crate::{
    core::types::{EnumType, RecordField},
    runtime::{
        arena::Arena,
        buffer::Buffer,
        parallel::{
            packet::{Packet, Shape},
            reserve_current,
        },
        value_storage::{read_memory, write_scalar},
    },
    Error, Type,
};
use std::ptr::NonNull;

fn fields(ty: &Type) -> Result<&[RecordField], Error> {
    match ty {
        Type::Record(r) => Ok(&r.fields),
        _ => Err(invalid("Invalid List record layout")),
    }
}
fn enumeration(ty: &Type) -> Result<&EnumType, Error> {
    match ty {
        Type::Enum(e) => Ok(e),
        _ => Err(invalid("Invalid List enum layout")),
    }
}
fn variant(ty: &Type, name: &str) -> Result<usize, Error> {
    enumeration(ty)?
        .variants
        .iter()
        .position(|v| v.name == name)
        .ok_or_else(|| invalid("Invalid List variant"))
}
fn set_option(ty: &Type, words: &mut [u64], value: Option<u64>) -> Result<(), Error> {
    let index = variant(ty, if value.is_some() { "Some" } else { "None" })?;
    words[0] = index as u64;
    if let Some(value) = value {
        words[enumeration(ty)?.variants[index].offset] = value;
    }
    Ok(())
}
fn option(ty: &Type, words: &[u64]) -> Result<Option<u64>, Error> {
    let index = usize::try_from(words[0]).map_err(|_| invalid("Invalid List option"))?;
    if index == variant(ty, "None")? {
        Ok(None)
    } else if index == variant(ty, "Some")? {
        Ok(Some(words[enumeration(ty)?.variants[index].offset]))
    } else {
        Err(invalid("Invalid List option"))
    }
}

/// Build pointer-free index metadata with constant temporary storage.
fn indices(
    ty: &Type,
    count: usize,
    arena: &mut Arena,
    mut fill: impl FnMut(usize, &mut [u64]) -> Result<(), Error>,
) -> Result<NonNull<Buffer>, Error> {
    let element = ty
        .array_element_type()
        .ok_or_else(|| invalid("Invalid List index array"))?;
    let _scratch =
        reserve_current(element.scalar_types().len() * (8 + std::mem::size_of::<(usize, Type)>()))?;
    // These are private UInt64 / NodeSlot buffers, never owning user data.
    if element.is_place() || element.scalar_types().iter().any(Type::is_owner_lane) {
        return Err(invalid("Invalid List index storage"));
    }
    let layout = element
        .storage_layout()
        .map_err(|_| invalid("Invalid List index layout"))?;
    let locations = element
        .storage_scalars()
        .map_err(|_| invalid("Invalid List index lanes"))?;
    let mut words = vec![0; element.scalar_types().len()];
    let mut owner = Buffer::allocate(arena, count as u64, layout.size, layout.align)?;
    let built = (|| {
        for index in 0..count {
            words.fill(0);
            fill(index, &mut words)?;
            unsafe {
                let dst = owner.as_ref().initialize_next()?;
                for ((offset, scalar), bits) in locations.iter().zip(&words) {
                    write_scalar(dst.add(*offset), scalar, *bits);
                }
                owner.as_mut().commit_next();
            }
        }
        Ok(())
    })();
    if let Err(error) = built {
        unsafe { Buffer::release(arena, owner.as_ptr()) }.expect("pointer-free List indices");
        return Err(error);
    }
    Ok(owner)
}

pub(super) fn input(
    ty: &Type,
    elements: &[TaskValue],
    packet: &mut Packet,
    base: usize,
    arena: &mut Arena,
) -> Result<(), Error> {
    if ty.list_element_type().is_none() {
        return Err(invalid("Expected List type"));
    }
    let f = fields(ty)?;
    let raw = arrays::input(&f[0].ty, elements, arena)?;
    unsafe {
        packet.push_buffer(base + f[0].offset, raw, arena)?;
    }
    let raw = indices(&f[1].ty, elements.len(), arena, |index, words| {
        words[0] = index as u64;
        Ok(())
    })?;
    unsafe {
        packet.push_buffer(base + f[1].offset, raw, arena)?;
    }
    let node = f[2].ty.array_element_type().unwrap();
    let used = variant(node, "Used")?;
    let v = &enumeration(node)?.variants[used];
    let links = fields(
        v.payload
            .as_ref()
            .ok_or_else(|| invalid("Invalid List links"))?,
    )?;
    let raw = indices(&f[2].ty, elements.len(), arena, |index, words| {
        words[0] = used as u64;
        set_option(
            &links[0].ty,
            &mut words[v.offset + links[0].offset..],
            index.checked_sub(1).map(|i| i as u64),
        )?;
        set_option(
            &links[1].ty,
            &mut words[v.offset + links[1].offset..],
            (index + 1 < elements.len()).then_some(index as u64 + 1),
        )?;
        words[v.offset + links[2].offset] = index as u64;
        Ok(())
    })?;
    unsafe {
        packet.push_buffer(base + f[2].offset, raw, arena)?;
    }
    for (field, value) in f[3..].iter().zip([
        (!elements.is_empty()).then_some(0),
        elements.len().checked_sub(1).map(|i| i as u64),
        None,
    ]) {
        set_option(&field.ty, &mut packet.words[base + field.offset..], value)?;
    }
    Ok(())
}

/// # Safety
/// `words` contains a live List of the exact trusted type, retained by a packet.
pub(super) unsafe fn length(ty: &Type, words: &[u64]) -> Result<usize, Error> {
    arrays::length(words[fields(ty)?[0].offset])
}

/// Visit payloads in logical order and check both the live and free chains.
/// # Safety
/// `words` contains a live List of the exact trusted type, exclusively retained
/// by a checked packet. The visitor must not mutate, release or retain raw lanes.
pub(super) unsafe fn visit(
    ty: &Type,
    words: &[u64],
    preflight: bool,
    mut visit: impl FnMut(&Type, &[u64]) -> Result<(), Error>,
) -> Result<(), Error> {
    let element = ty
        .list_element_type()
        .ok_or_else(|| invalid("Expected List type"))?;
    let f = fields(ty)?;
    let count = length(ty, words)?;
    let node = f[2].ty.array_element_type().unwrap();
    let width = element.value_lanes().len();
    let node_width = node.value_lanes().len();
    let scratch = (width + node_width) * 8 + Shape::of(element)?.bytes() + Shape::of(node)?.bytes();
    let bytes = count
        .checked_mul(usize::from(preflight) * std::mem::size_of::<TaskValue>())
        .and_then(|n| n.checked_add(scratch))
        .ok_or_else(|| invalid("List result size overflow"))?;
    let _scratch = reserve_current(bytes)?;
    if arrays::length(words[f[1].offset])? != count {
        return Err(invalid("Invalid List owner count"));
    }
    let slots = arrays::length(words[f[2].offset])?;
    if slots < count {
        return Err(invalid("Invalid List slot count"));
    }
    let values = &*(words[f[0].offset] as *const Buffer);
    let owners = &*(words[f[1].offset] as *const Buffer);
    let nodes = &*(words[f[2].offset] as *const Buffer);
    let used = variant(node, "Used")?;
    let free = variant(node, "Free")?;
    let variants = &enumeration(node)?.variants;
    let links = fields(variants[used].payload.as_ref().unwrap())?;
    let mut current = option(&f[3].ty, &words[f[3].offset..])?;
    let tail = option(&f[4].ty, &words[f[4].offset..])?;
    let mut previous = None;
    let mut node_words = vec![0; node_width];
    let mut value_words = vec![0; width];
    for _ in 0..count {
        let index = current.ok_or_else(|| invalid("Short List chain"))?;
        node_words.fill(0);
        read_memory(node, nodes.load(index)?, &mut node_words)?;
        if node_words[0] != used as u64 {
            return Err(invalid("Invalid List live node"));
        }
        let link_words = &node_words[variants[used].offset..];
        if option(&links[0].ty, &link_words[links[0].offset..])? != previous {
            return Err(invalid("Invalid List previous link"));
        }
        let value_index = link_words[links[2].offset];
        let mut owner = [0];
        read_memory(&Type::UInt64, owners.load(value_index)?, &mut owner)?;
        if owner[0] != index {
            return Err(invalid("Invalid List payload owner"));
        }
        value_words.fill(0);
        read_memory(element, values.load(value_index)?, &mut value_words)?;
        visit(element, &value_words)?;
        previous = Some(index);
        current = option(&links[1].ty, &link_words[links[1].offset..])?;
    }
    if current.is_some() || previous != tail {
        return Err(invalid("Invalid List tail"));
    }
    current = option(&f[5].ty, &words[f[5].offset..])?;
    for _ in 0..slots - count {
        let index = current.ok_or_else(|| invalid("Short List free chain"))?;
        node_words.fill(0);
        read_memory(node, nodes.load(index)?, &mut node_words)?;
        if node_words[0] != free as u64 {
            return Err(invalid("Invalid List free node"));
        }
        current = option(
            variants[free].payload.as_ref().unwrap(),
            &node_words[variants[free].offset..],
        )?;
    }
    if current.is_some() {
        return Err(invalid("Invalid List free tail"));
    }
    Ok(())
}
