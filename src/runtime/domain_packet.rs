//! Pointer-free domain packets: fixed scalar lanes followed by owned UTF-8 data.
//! String lanes contain word offsets, followed by a byte length and packed bytes.
use super::{arena::Arena, buffer::Buffer};
use crate::{Error, ErrorCode, ErrorPhase, Type};

fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidCall, message)
}
fn limited() -> Error {
    Error::new_with_code(
        ErrorPhase::Runtime,
        ErrorCode::ResourceLimit,
        "Domain text packet allocation failed",
    )
}
pub(crate) fn has_text(ty: &Type) -> bool {
    ty.scalar_types().contains(&Type::String)
}

// Walk only active values; inactive enum lanes must remain canonical zeroes.
fn visit(
    ty: &Type,
    words: &mut [u64],
    text: &mut impl FnMut(u64) -> Result<u64, Error>,
) -> Result<(), Error> {
    if words.len() != ty.scalar_types().len() {
        return Err(invalid("Domain value lane mismatch"));
    }
    match ty {
        Type::String => words[0] = text(words[0])?,
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
fn visit_types(
    types: &[Type],
    words: &mut [u64],
    text: &mut impl FnMut(u64) -> Result<u64, Error>,
) -> Result<(), Error> {
    let mut offset = 0;
    for ty in types {
        let end = offset + ty.scalar_types().len();
        visit(ty, &mut words[offset..end], text)?;
        offset = end;
    }
    Ok(())
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
pub(crate) fn validate(types: &[Type], words: &[u64]) -> Result<(), Error> {
    let count = lanes(types);
    if count > crate::core::script::MAX_LANES || words.len() < count {
        return Err(invalid("Domain packet lane count is out of bounds"));
    }
    let mut values = [0; crate::core::script::MAX_LANES];
    values[..count].copy_from_slice(&words[..count]);
    let mut offset = count;
    visit_types(types, &mut values[..count], &mut |value| {
        if usize::try_from(value).ok() != Some(offset) {
            return Err(invalid("Noncanonical domain String offset"));
        }
        offset = validate_text(words, offset)?;
        Ok(value)
    })?;
    if offset != words.len() {
        return Err(invalid("Trailing domain value packet data"));
    }
    Ok(())
}
/// Recreate independent String owners in the invocation arena. Inputs have no
/// pointers until this boundary; partial decoding releases every created owner.
pub(crate) fn decode(types: &[Type], words: &[u64], arena: &mut Arena) -> Result<Vec<u64>, Error> {
    validate(types, words)?;
    let mut values = words[..lanes(types)].to_vec();
    let mut owners = Vec::new();
    owners
        .try_reserve_exact(values.len())
        .map_err(|_| limited())?;
    let result = visit_types(types, &mut values, &mut |offset| {
        let (length, _) = header(words, offset as usize)?;
        let _scratch = arena.reserve_external(length)?;
        let bytes = string_bytes(words, offset as usize)?;
        let owner = Buffer::copy_bytes(arena, &bytes)?;
        owners.push(owner.as_ptr());
        Ok(owner.as_ptr() as u64)
    });
    if let Err(error) = result {
        for owner in owners {
            unsafe {
                let _ = Buffer::release(arena, owner);
            }
        }
        return Err(error);
    }
    Ok(values)
}
/// # Safety
/// Active String lanes are uniquely owned live buffers in `arena`. On success
/// or failure this consumes their ownership, after copying the result bytes.
pub(crate) unsafe fn encode_result(
    ty: &Type,
    raw: &[u64],
    arena: &mut Arena,
) -> Result<Vec<u64>, Error> {
    let scalars = ty.scalar_types();
    let count = scalars.len();
    let result = (|| {
        if raw.len() != count {
            return Err(invalid("Domain result lane mismatch"));
        }
        let mut total = count;
        for (ty, pointer) in scalars.iter().zip(raw) {
            if *ty == Type::String && *pointer != 0 {
                let text = super::owned_text::borrow(*pointer as *const Buffer)?;
                total = total
                    .checked_add(1 + text.len().div_ceil(8))
                    .ok_or_else(limited)?;
            }
        }
        let _charge = arena.reserve_external(total.checked_mul(8).ok_or_else(limited)?)?;
        let mut words = Vec::new();
        words.try_reserve_exact(total).map_err(|_| limited())?;
        words.extend_from_slice(raw);
        let mut values = [0; crate::core::script::MAX_LANES];
        values[..count].copy_from_slice(raw);
        visit(ty, &mut values[..count], &mut |pointer| {
            let text = super::owned_text::borrow(pointer as *const Buffer)?;
            let offset = words.len();
            words.push(text.len() as u64);
            for bytes in text.as_bytes().chunks(8) {
                let mut word = [0; 8];
                word[..bytes.len()].copy_from_slice(bytes);
                words.push(u64::from_le_bytes(word));
            }
            Ok(offset as u64)
        })?;
        words[..count].copy_from_slice(&values[..count]);
        Ok(words)
    })();
    // Even if allocation fails before the first copy, all returned owners end here.
    for (ty, pointer) in scalars.iter().zip(raw) {
        if *ty == Type::String && *pointer != 0 {
            let _ = Buffer::release(arena, *pointer as *mut Buffer);
        }
    }
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
/// `packet` is an exclusive UInt64 buffer; a nonzero owner is a live String.
/// Zero represents an inactive enum lane, never an active String value.
pub(crate) unsafe fn pack(
    arena: &mut Arena,
    packet: *mut Buffer,
    index: usize,
    owner: *const Buffer,
) -> Result<(), Error> {
    if owner.is_null() {
        return Ok(());
    }
    let text = super::owned_text::borrow(owner)?;
    let count = (*packet).len() as usize;
    let total = count
        .checked_add(1 + text.len().div_ceil(8))
        .ok_or_else(limited)?;
    let _charge = arena.reserve_external(total.checked_mul(8).ok_or_else(limited)?)?;
    let mut words = Vec::new();
    words.try_reserve_exact(total).map_err(|_| limited())?;
    for i in 0..count {
        words.push((*packet).load(i as u64)?.cast::<u64>().read());
    }
    words[index] = count as u64;
    words.push(text.len() as u64);
    for bytes in text.as_bytes().chunks(8) {
        let mut word = [0; 8];
        word[..bytes.len()].copy_from_slice(bytes);
        words.push(u64::from_le_bytes(word));
    }
    write(arena, packet, &words)
}
/// # Safety
/// `packet` is a checked UInt64 packet; zero offsets are inactive String lanes.
pub(crate) unsafe fn unpack(
    arena: &mut Arena,
    packet: *const Buffer,
    index: usize,
) -> Result<u64, Error> {
    let offset = (*packet).load(index as u64)?.cast::<u64>().read();
    if offset == 0 {
        return Ok(0);
    }
    let count = (*packet).len() as usize;
    let _charge = arena.reserve_external(count * 8)?;
    let mut words = Vec::new();
    words.try_reserve_exact(count).map_err(|_| limited())?;
    for i in 0..count {
        words.push((*packet).load(i as u64)?.cast::<u64>().read());
    }
    let (length, _) = header(&words, offset as usize)?;
    let _bytes_charge = arena.reserve_external(length)?;
    let bytes = string_bytes(&words, offset as usize)?;
    Ok(Buffer::copy_bytes(arena, &bytes)?.as_ptr() as u64)
}
/// # Safety
/// All pointers and lane indices obey `pack`/`unpack`; called only by generated code.
#[export_name = "__joky_domain_text_v1"]
pub(crate) unsafe extern "C" fn text(
    arena: *mut Arena,
    packet: *mut Buffer,
    index: usize,
    value: u64,
    decode: i32,
) -> u64 {
    let result = if decode != 0 {
        unpack(&mut *arena, packet, index)
    } else {
        pack(&mut *arena, packet, index, value as *const Buffer).map(|()| 0)
    };
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
    let key = scalars.iter().position(|ty| *ty == Type::String)?;
    let owners = scalars
        .iter()
        .zip(words)
        .filter(|(ty, _)| **ty == Type::String)
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
            hooks: Vec::new(),
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
}
