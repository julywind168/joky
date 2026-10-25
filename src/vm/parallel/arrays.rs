//! Safe host Array values use the same typed storage and exclusive owner packets.
use super::{invalid, TaskValue};
use crate::{
    runtime::{
        arena::Arena,
        buffer::{Buffer, ElementCleanup},
        cleanup::Cleanups,
        parallel::{packet::Shape, reserve_current},
        value_storage::{read_memory, write_scalar},
    },
    Error, Type,
};
use std::ptr::NonNull;

pub(super) fn input(
    ty: &Type,
    elements: &[TaskValue],
    arena: &mut Arena,
) -> Result<NonNull<Buffer>, Error> {
    let element = ty
        .array_element_type()
        .ok_or_else(|| invalid("Expected Array type"))?;
    let _scratch = reserve_current(
        Shape::of(element)?.bytes()
            + element.scalar_types().len() * std::mem::size_of::<(usize, Type)>(),
    )?;
    let layout = element
        .storage_layout()
        .map_err(|_| invalid("Invalid Array element layout"))?;
    let locations = if element.is_place() {
        vec![]
    } else {
        element
            .storage_scalars()
            .map_err(|_| invalid("Invalid Array element lanes"))?
    };
    let first_owner = element.scalar_types().iter().position(Type::is_owner_lane);
    let mut owner = Buffer::allocate(arena, elements.len() as u64, layout.size, layout.align)?;
    let built = (|| {
        unsafe {
            if first_owner.is_some() {
                owner.as_mut().configure(ElementCleanup {
                    owners: locations
                        .iter()
                        .filter(|(_, t)| t.is_owner_lane())
                        .map(|(offset, _)| *offset)
                        .collect(),
                    hooks: vec![],
                })?;
            }
            for value in elements {
                let dst = owner.as_ref().initialize_next()?;
                if element.is_place() {
                    super::places::write(value, element, dst)?;
                    owner.as_mut().commit_next();
                    continue;
                }
                let packet = value.packet(element, arena)?;
                let mut cleanup = Cleanups::default();
                let words = match packet.install(arena, &mut cleanup, 0, None) {
                    Ok(words) => words,
                    Err(error) => {
                        let _ = cleanup.finish(arena);
                        return Err(error);
                    }
                };
                // No fallible work between storing children and committing their
                // parent. The array now owns precisely this initialized prefix.
                for ((offset, scalar), bits) in locations.iter().zip(words) {
                    write_scalar(dst.add(*offset), scalar, bits);
                }
                owner.as_mut().commit_next();
                if let Some(key) = first_owner {
                    cleanup.take(key);
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = built {
        unsafe { Buffer::release(arena, owner.as_ptr()) }.expect("hook-free array cleanup");
        return Err(error);
    }
    Ok(owner)
}

/// Read the initialized element count.
/// # Safety
/// The handle is a live Array buffer exclusively retained by a checked packet.
pub(super) unsafe fn length(word: u64) -> Result<usize, Error> {
    let owner = NonNull::new(word as *mut Buffer).ok_or_else(|| invalid("Null Array result"))?;
    usize::try_from(owner.as_ref().len()).map_err(|_| invalid("Array length overflow"))
}

/// Read only initialized, active fields; `ty` is the packet's exact Array type.
/// Preflight host element metadata before walking even zero-sized huge arrays.
/// # Safety
/// `word` is a live Array buffer with the exact `ty`, exclusively retained by a
/// checked packet. The visitor must not mutate, release or retain its raw lanes.
pub(super) unsafe fn visit(
    ty: &Type,
    word: u64,
    preflight: bool,
    mut visit: impl FnMut(&Type, &[u64]) -> Result<(), Error>,
) -> Result<(), Error> {
    let element = ty
        .array_element_type()
        .ok_or_else(|| invalid("Expected Array type"))?;
    let count = length(word)?;
    let width = element.value_lanes().len();
    let bytes = count
        .checked_mul(usize::from(preflight) * std::mem::size_of::<TaskValue>())
        .and_then(|n| n.checked_add(width * 8 + Shape::of(element).ok()?.bytes()))
        .ok_or_else(|| invalid("Array result size overflow"))?;
    let _scratch = reserve_current(bytes)?;
    let owner = &*(word as *const Buffer);
    let mut words = vec![0; width];
    for index in 0..count {
        words.fill(0);
        if element.is_place() {
            words[0] = owner.load(index as u64)? as u64;
        } else {
            read_memory(element, owner.load(index as u64)?, &mut words)?;
        }
        visit(element, &words)?;
    }
    Ok(())
}
