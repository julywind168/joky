//! Pointer-free domain packets with recursive String and Array payloads.
//! Owner lanes contain word offsets; every allocation has an independent lifetime.
use super::{
    arena::Arena,
    buffer::Buffer,
    value_storage::{read_memory, write_scalar},
};
use crate::{Error, ErrorCode, ErrorPhase, Type};

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidCall, message)
}
fn limited() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Domain value packet allocation failed",
    )
}
pub(crate) fn validate_when_handles(packet: &Buffer) -> Result<(), Error> {
    if !(1..=64).contains(&packet.len()) {
        return Err(invalid("Invalid when access count"));
    }
    Ok(())
}

pub(crate) fn has_owners(ty: &Type) -> bool {
    ty.scalar_types().iter().any(Type::is_owner_lane)
}

// Walk only active values; inactive enum lanes must remain canonical zeroes.
fn visit(
    ty: &Type,
    words: &mut [u64],
    text: &mut impl FnMut(&Type, u64) -> Result<u64, Error>,
) -> Result<(), Error> {
    if words.len() != ty.scalar_types().len() {
        return Err(invalid("Domain value lane mismatch"));
    }
    match ty {
        Type::String | Type::Buffer(_) => words[0] = text(ty, words[0])?,
        Type::Record(record) => {
            for field in &record.fields {
                visit(
                    &field.ty,
                    &mut words[field.offset..field.offset + field.ty.scalar_types().len()],
                    text,
                )?;
            }
        }
        Type::Tuple(tuple) => {
            let mut offset = 0;
            for element in &tuple.elements {
                let end = offset + element.scalar_types().len();
                visit(element, &mut words[offset..end], text)?;
                offset = end;
            }
        }
        Type::Enum(enumeration) => {
            let variant = usize::try_from(words[0])
                .ok()
                .and_then(|tag| enumeration.variants.get(tag))
                .ok_or_else(|| invalid("Invalid domain value enum tag"))?;
            let start = variant.offset;
            let end = start
                + variant
                    .payload
                    .as_ref()
                    .map_or(0, |t| t.scalar_types().len());
            if words[1..start].iter().chain(&words[end..]).any(|n| *n != 0) {
                return Err(invalid("Noncanonical inactive domain value lanes"));
            }
            if let Some(payload) = &variant.payload {
                visit(payload, &mut words[start..end], text)?;
            }
        }
        Type::Unit => {}
        _ if words.len() == 1
            && crate::core::instance::InstanceValue::from_bits(ty, words[0]).is_some() => {}
        _ => return Err(invalid("Invalid domain scalar value")),
    }
    Ok(())
}
fn lanes(types: &[Type]) -> usize {
    types.iter().map(|ty| ty.scalar_types().len()).sum()
}
fn header(words: &[u64], offset: usize) -> Result<(usize, usize), Error> {
    let length = words
        .get(offset)
        .copied()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid("Invalid domain String offset"))?;
    let end = offset
        .checked_add(1)
        .and_then(|n| n.checked_add(length.div_ceil(8)))
        .filter(|end| *end <= words.len())
        .ok_or_else(|| invalid("Truncated domain String packet"))?;
    Ok((length, end))
}
fn string_bytes(words: &[u64], offset: usize) -> Result<Vec<u8>, Error> {
    let (length, end) = header(words, offset)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| limited())?;
    for word in &words[offset + 1..end] {
        let count = (length - bytes.len()).min(8);
        let part = word.to_le_bytes();
        if part[count..].iter().any(|n| *n != 0) {
            return Err(invalid("Noncanonical domain String padding"));
        }
        bytes.extend_from_slice(&part[..count]);
    }
    std::str::from_utf8(&bytes).map_err(|_| invalid("Invalid domain String UTF-8"))?;
    Ok(bytes)
}
// Validate UTF-8 across wire-word boundaries without allocating a second string.
fn validate_text(words: &[u64], offset: usize) -> Result<usize, Error> {
    let (length, end) = header(words, offset)?;
    let mut remaining = length;
    let mut pending = [0u8; 12];
    let mut kept = 0;
    for word in &words[offset + 1..end] {
        let count = remaining.min(8);
        remaining -= count;
        let bytes = word.to_le_bytes();
        if bytes[count..].iter().any(|byte| *byte != 0) {
            return Err(invalid("Noncanonical domain String padding"));
        }
        pending[kept..kept + count].copy_from_slice(&bytes[..count]);
        let used = kept + count;
        match std::str::from_utf8(&pending[..used]) {
            Ok(_) => kept = 0,
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                pending.copy_within(valid..used, 0);
                kept = used - valid;
            }
            Err(_) => return Err(invalid("Invalid domain String UTF-8")),
        }
    }
    if kept != 0 {
        return Err(invalid("Truncated domain String UTF-8"));
    }
    Ok(end)
}
fn array_header(words: &[u64], offset: usize, element: &Type) -> Result<(usize, usize), Error> {
    let count = words
        .get(offset)
        .copied()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid("Invalid domain array count"))?;
    let end = count
        .checked_mul(element.scalar_types().len())
        .and_then(|n| offset.checked_add(1)?.checked_add(n))
        .filter(|end| *end <= words.len())
        .ok_or_else(|| invalid("Truncated domain array packet"))?;
    Ok((count, end))
}
fn validate_values(
    ty: &Type,
    values: &[u64],
    words: &[u64],
    cursor: &mut usize,
) -> Result<(), Error> {
    let mut raw = [0; crate::core::script::MAX_LANES];
    if values.len() > raw.len() {
        return Err(invalid("Domain element exceeds lane limit"));
    }
    raw[..values.len()].copy_from_slice(values);
    visit(ty, &mut raw[..values.len()], &mut |ty, offset| {
        if usize::try_from(offset).ok() != Some(*cursor) {
            return Err(invalid(&format!("Noncanonical domain owner offset: {ty}, got {offset}, expected {}, packet {} words", *cursor, words.len())));
        }
        match ty {
            Type::String => *cursor = validate_text(words, *cursor)?,
            Type::Buffer(element) => {
                let start = *cursor + 1;
                let (_, end) = array_header(words, *cursor, element)?;
                *cursor = end;
                let width = element.scalar_types().len();
                if width != 0 {
                    for value in words[start..end].chunks_exact(width) {
                        validate_values(element, value, words, cursor)?;
                    }
                }
            }
            _ => unreachable!(),
        }
        Ok(offset)
    })
}
pub(crate) fn validate(types: &[Type], words: &[u64]) -> Result<(), Error> {
    let count = lanes(types);
    if count > crate::core::script::MAX_LANES || words.len() < count {
        return Err(invalid("Domain packet lane count is out of bounds"));
    }
    let mut cursor = count;
    let mut start = 0;
    for ty in types {
        let end = start + ty.scalar_types().len();
        validate_values(ty, &words[start..end], words, &mut cursor)?;
        start = end;
    }
    if cursor != words.len() {
        return Err(invalid("Trailing domain value packet data"));
    }
    Ok(())
}
fn layout_error(_: crate::memory_layout::MemoryLayoutError) -> Error {
    invalid("Invalid domain element layout")
}
fn release_values(types: &[Type], raw: &[u64], arena: &mut Arena) {
    for (ty, pointer) in types.iter().flat_map(Type::scalar_types).zip(raw) {
        if ty.is_owner_lane() && *pointer != 0 {
            unsafe {
                let _ = Buffer::release(arena, *pointer as *mut Buffer);
            }
        }
    }
}
fn decode_values(
    ty: &Type,
    raw: &mut [u64],
    words: &[u64],
    arena: &mut Arena,
) -> Result<(), Error> {
    let mut owners = Vec::new();
    owners.try_reserve_exact(raw.len()).map_err(|_| limited())?;
    let result = visit(ty, raw, &mut |ty, offset| {
        let pointer = decode_owner(ty, words, offset as usize, arena)?;
        owners.push(pointer);
        Ok(pointer as u64)
    });
    if result.is_err() {
        for pointer in owners {
            unsafe {
                let _ = Buffer::release(arena, pointer);
            }
        }
    }
    result
}
fn decode_owner(
    ty: &Type,
    words: &[u64],
    offset: usize,
    arena: &mut Arena,
) -> Result<*mut Buffer, Error> {
    if *ty == Type::String {
        let (length, _) = header(words, offset)?;
        let _scratch = arena.reserve_external(length)?;
        return Ok(Buffer::copy_bytes(arena, &string_bytes(words, offset)?)?.as_ptr());
    }
    let Type::Buffer(element) = ty else {
        return Err(invalid("Invalid domain owner type"));
    };
    let (count, end) = array_header(words, offset, element)?;
    let layout = element.storage_layout().map_err(layout_error)?;
    let locations = element.storage_scalars().map_err(layout_error)?;
    let mut owner = Buffer::allocate(arena, count as u64, layout.size, layout.align)?;
    let result = (|| {
        unsafe {
            if element.is_owner() {
                owner.as_mut().configure(super::buffer::ElementCleanup {
                    owners: locations
                        .iter()
                        .filter(|(_, ty)| ty.is_owner_lane())
                        .map(|(offset, _)| *offset)
                        .collect(),
                })?;
            }
            let width = element.scalar_types().len();
            if width == 0 {
                owner.as_mut().initialize_empty(count as u64)?;
            } else {
                for value in words[offset + 1..end].chunks_exact(width) {
                    // Check destination before creating the next element's children.
                    let dst = owner.as_ref().initialize_next()?;
                    let mut raw = value.to_vec();
                    decode_values(element, &mut raw, words, arena)?;
                    for ((offset, scalar), bits) in locations.iter().zip(raw) {
                        write_scalar(dst.add(*offset), scalar, bits);
                    }
                    owner.as_mut().commit_next();
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        unsafe {
            let _ = Buffer::release(arena, owner.as_ptr());
        }
        return Err(error);
    }
    Ok(owner.as_ptr())
}
/// Recreate independent owners; partial decoding releases every completed subtree.
pub(crate) fn decode(types: &[Type], words: &[u64], arena: &mut Arena) -> Result<Vec<u64>, Error> {
    validate(types, words)?;
    let mut raw = words[..lanes(types)].to_vec();
    let mut start = 0;
    for ty in types {
        let end = start + ty.scalar_types().len();
        if let Err(error) = decode_values(ty, &mut raw[start..end], words, arena) {
            // The failing subtree already released its partial owners.
            release_values(types, &raw[..start], arena);
            return Err(error);
        }
        start = end;
    }
    Ok(raw)
}
unsafe fn owner_size(ty: &Type, owner: *const Buffer) -> Result<usize, Error> {
    if *ty == Type::String {
        return Ok(1 + super::owned_text::borrow(owner)?.len().div_ceil(8));
    }
    let Type::Buffer(element) = ty else {
        return Err(invalid("Invalid domain owner type"));
    };
    let width = element.scalar_types().len();
    let mut size = ((*owner).len() as usize)
        .checked_mul(width)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(limited)?;
    if has_owners(element) {
        for index in 0..(*owner).len() {
            let mut raw = [0; crate::core::script::MAX_LANES];
            read_memory(element, (*owner).load(index)?, &mut raw[..width])?;
            visit(element, &mut raw[..width], &mut |ty, pointer| {
                size = size
                    .checked_add(owner_size(ty, pointer as *const Buffer)?)
                    .ok_or_else(limited)?;
                Ok(pointer)
            })?;
        }
    }
    Ok(size)
}
unsafe fn encode_owner(
    ty: &Type,
    owner: *const Buffer,
    words: &mut Vec<u64>,
) -> Result<usize, Error> {
    let offset = words.len();
    if *ty == Type::String {
        let text = super::owned_text::borrow(owner)?;
        words.push(text.len() as u64);
        for bytes in text.as_bytes().chunks(8) {
            let mut word = [0; 8];
            word[..bytes.len()].copy_from_slice(bytes);
            words.push(u64::from_le_bytes(word));
        }
    } else if let Type::Buffer(element) = ty {
        let width = element.scalar_types().len();
        let count = (*owner).len();
        words.push(count);
        words.resize(words.len() + count as usize * width, 0);
        if width != 0 {
            for index in 0..count {
                let mut raw = [0; crate::core::script::MAX_LANES];
                read_memory(element, (*owner).load(index)?, &mut raw[..width])?;
                visit(element, &mut raw[..width], &mut |ty, pointer| {
                    Ok(encode_owner(ty, pointer as *const Buffer, words)? as u64)
                })?;
                let start = offset + 1 + index as usize * width;
                words[start..start + width].copy_from_slice(&raw[..width]);
            }
        }
    } else {
        return Err(invalid("Invalid domain owner type"));
    }
    Ok(offset)
}
/// # Safety
/// Active owner lanes are live unique buffers. All returned ownership is consumed,
/// including on allocation failure; nested buffers carry normal element cleanup.
pub(crate) unsafe fn encode_result(
    ty: &Type,
    raw: &[u64],
    arena: &mut Arena,
) -> Result<Vec<u64>, Error> {
    let result = (|| {
        let mut values = raw.to_vec();
        let mut total = raw.len();
        visit(ty, &mut values, &mut |ty, pointer| {
            total = total
                .checked_add(owner_size(ty, pointer as *const Buffer)?)
                .ok_or_else(limited)?;
            Ok(pointer)
        })?;
        let _charge = arena.reserve_external(total.checked_mul(8).ok_or_else(limited)?)?;
        let mut words = Vec::new();
        words.try_reserve_exact(total).map_err(|_| limited())?;
        words.extend_from_slice(raw);
        visit(ty, &mut values, &mut |ty, pointer| {
            Ok(encode_owner(ty, pointer as *const Buffer, &mut words)? as u64)
        })?;
        words[..raw.len()].copy_from_slice(&values);
        Ok(words)
    })();
    release_values(std::slice::from_ref(ty), raw, arena);
    result
}

/// Grow and overwrite a private UInt64 packet. Its owner address stays stable.
/// # Safety
/// The packet is a live exclusive UInt64 Buffer in `arena`.
pub(crate) unsafe fn write(
    arena: &mut Arena,
    packet: *mut Buffer,
    words: &[u64],
) -> Result<(), Error> {
    if (*packet).capacity() < words.len() as u64 {
        let replacement = Buffer::allocate(arena, words.len() as u64, 8, 8)?;
        Buffer::replace(arena, packet, replacement.as_ptr())?;
    }
    (*packet).truncate_owned(arena, 0)?;
    for word in words {
        (*packet).initialize_next()?.cast::<u64>().write(*word);
        (*packet).commit_next();
    }
    Ok(())
}
/// # Safety
/// `packet` is an exclusive UInt64 buffer; a nonzero owner matches the type.
/// Zero represents an inactive enum lane, never an active owner.
pub(crate) unsafe fn pack(
    arena: &mut Arena,
    packet: *mut Buffer,
    index: usize,
    owner: *const Buffer,
    ty: &Type,
) -> Result<(), Error> {
    if owner.is_null() {
        return Ok(());
    }
    let count = (*packet).len() as usize;
    let total = count
        .checked_add(owner_size(ty, owner)?)
        .ok_or_else(limited)?;
    let _charge = arena.reserve_external(total.checked_mul(8).ok_or_else(limited)?)?;
    let mut words = Vec::new();
    words.try_reserve_exact(total).map_err(|_| limited())?;
    for i in 0..count {
        words.push((*packet).load(i as u64)?.cast::<u64>().read());
    }
    words[index] = encode_owner(ty, owner, &mut words)? as u64;
    write(arena, packet, &words)
}
/// # Safety
/// The packet and owner type have passed complete domain contract validation.
pub(crate) unsafe fn unpack(
    arena: &mut Arena,
    packet: *const Buffer,
    index: usize,
    ty: &Type,
) -> Result<u64, Error> {
    let offset = (*packet).load(index as u64)?.cast::<u64>().read();
    if offset == 0 {
        return Ok(0);
    }
    let count = (*packet).len() as usize;
    let _charge = arena.reserve_external(count.checked_mul(8).ok_or_else(limited)?)?;
    let mut words = Vec::new();
    words.try_reserve_exact(count).map_err(|_| limited())?;
    for i in 0..count {
        words.push((*packet).load(i as u64)?.cast::<u64>().read());
    }
    Ok(decode_owner(ty, &words, offset as usize, arena)? as u64)
}
/// # Safety
/// Pointers, indices and immutable type descriptor are compiler-emitted and live.
#[export_name = "__joky_domain_owner_v1"]
pub(crate) unsafe extern "C" fn transfer(
    arena: *mut Arena,
    packet: *mut Buffer,
    index: usize,
    value: u64,
    decode: i32,
    descriptor: *const u8,
    length: usize,
) -> u64 {
    let result =
        super::domain::metadata::decode_value_type(std::slice::from_raw_parts(descriptor, length))
            .and_then(|ty| {
                if decode != 0 {
                    unpack(&mut *arena, packet, index, &ty)
                } else {
                    pack(&mut *arena, packet, index, value as *const Buffer, &ty).map(|()| 0)
                }
            });
    match result {
        Ok(value) => value,
        Err(error) => {
            super::error::record_error(error);
            0
        }
    }
}

/// Initial owner registration also protects requests cancelled before their first poll.
pub(crate) fn input_cleanup(ty: &Type, words: &[u64]) -> Option<(usize, super::cleanup::Entry)> {
    let scalars = ty.scalar_types();
    let key = scalars.iter().position(Type::is_owner_lane)?;
    let owners = scalars
        .iter()
        .zip(words)
        .filter(|(ty, _)| ty.is_owner_lane())
        .map(|(_, word)| {
            std::ptr::NonNull::new(*word as *mut Buffer).map(std::ptr::NonNull::as_ptr)
        })
        .collect();
    Some((
        key,
        super::cleanup::Entry {
            keep_alive: None,
            storage_size: 0,
            storage: None,
            owners,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn domain_text_packets_validate_offsets_lengths_padding_and_utf8() {
        let ty = [Type::String];
        assert!(validate(&ty, &[1, 0]).is_ok());
        assert!(validate(&ty, &[1, 3, 0x00636261]).is_ok());
        for words in [
            vec![0],
            vec![2, 0],
            vec![1, 9, 0],
            vec![1, 1, 0xff],
            vec![1, 1, 0x10061],
            vec![1, 0, 0],
        ] {
            assert!(validate(&ty, &words).is_err(), "{words:?}");
        }
        let mut arena = Arena::default();
        let raw = decode(&ty, &[1, 3, 0x00636261], &mut arena).unwrap();
        let wire = unsafe { encode_result(&Type::String, &raw, &mut arena) }.unwrap();
        assert_eq!(wire, [1, 3, 0x00636261]);
    }
    #[test]
    fn owned_array_domain_packets_validate_complete_nested_payloads() {
        let ty = Type::Buffer(std::sync::Arc::new(Type::String));
        let valid = [1, 2, 4, 6, 1, 0x61, 0];
        assert!(validate(std::slice::from_ref(&ty), &valid).is_ok());
        for malformed in [
            vec![1, u64::MAX],
            vec![1, 2, 4],
            vec![1, 2, 4, 4, 1, 0x61, 0],
            vec![1, 2, 4, 6, 1, 0xff, 0],
            vec![1, 0, 0],
        ] {
            assert!(
                validate(std::slice::from_ref(&ty), &malformed).is_err(),
                "{malformed:?}"
            );
        }
        let mut arena = Arena::default();
        let raw = decode(std::slice::from_ref(&ty), &valid, &mut arena).unwrap();
        assert_eq!(
            unsafe { encode_result(&ty, &raw, &mut arena) }.unwrap(),
            valid
        );
        let empty = Type::Buffer(std::sync::Arc::new(Type::Unit));
        let packet = [1, 1_000_000];
        let raw = decode(std::slice::from_ref(&empty), &packet, &mut arena).unwrap();
        assert_eq!(
            unsafe { encode_result(&empty, &raw, &mut arena) }.unwrap(),
            packet
        );
    }
}
