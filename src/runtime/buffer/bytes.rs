//! Bounded byte operations over compiler-checked owners and borrowed slices.
use super::{bounds_error, located, Buffer, Error, BYTE_CHUNK};
use crate::runtime::parallel::pool::check_cancelled;

fn range(buffer: &Buffer, start: u64, end: u64) -> Result<std::ops::Range<usize>, Error> {
    if start > end || end > buffer.len() {
        return Err(bounds_error("Byte range is outside the initialized prefix"));
    }
    Ok(start as usize..end as usize)
}

impl Buffer {
    /// Append an initialized byte range to pre-reserved capacity without retaining a view.
    ///
    /// # Safety
    /// Both headers and their payloads remain live on this task. The destination is
    /// exclusively borrowed, owns plain UInt8 storage, and does not alias the source.
    /// No callback or suspension may run while the payload pointers are in use.
    pub(crate) unsafe fn append_bytes(
        destination: *mut Self,
        source: *const Self,
        start: u64,
        end: u64,
    ) -> Result<(), Error> {
        if std::ptr::eq(destination, source) {
            return Err(bounds_error("Byte append requires distinct storage"));
        }
        let source = &*source;
        let selected = range(source, start, end)?;
        let input = &source.bytes()?[selected];
        let destination = &mut *destination;
        destination.bytes()?;
        if input.len() > destination.capacity - destination.len {
            return Err(bounds_error("Byte append exceeds reserved capacity"));
        }
        // Publish the new prefix only after all bounded copies succeed. A cancelled
        // copy leaves bytes outside the old initialized prefix, with no new owners.
        let output = destination.data.as_ptr().add(destination.len);
        for (index, chunk) in input.chunks(BYTE_CHUNK).enumerate() {
            check_cancelled()?;
            std::ptr::copy_nonoverlapping(
                chunk.as_ptr(),
                output.add(index * BYTE_CHUNK),
                chunk.len(),
            );
        }
        destination.len += input.len();
        Ok(())
    }

    /// Return an absolute offset, or `end` if the byte is absent from the range.
    pub(crate) fn find_byte(&self, byte: u8, start: u64, end: u64) -> Result<u64, Error> {
        let selected = range(self, start, end)?;
        let input = &self.bytes()?[selected];
        for (index, chunk) in input.chunks(BYTE_CHUNK).enumerate() {
            check_cancelled()?;
            if let Some(offset) = chunk.iter().position(|&value| value == byte) {
                return Ok(start + (index * BYTE_CHUNK + offset) as u64);
            }
        }
        Ok(end)
    }

    /// Return the longest valid UTF-8 prefix. An incomplete scalar at a chunk
    /// boundary is retried with its continuation bytes, not reported as an error.
    pub(crate) fn utf8_prefix(&self) -> Result<u64, Error> {
        let input = self.bytes()?;
        let mut offset = 0;
        while offset < input.len() {
            check_cancelled()?;
            let end = input.len().min(offset + BYTE_CHUNK);
            match std::str::from_utf8(&input[offset..end]) {
                Ok(_) => offset = end,
                Err(error) => {
                    offset += error.valid_up_to();
                    if error.error_len().is_some() || end == input.len() {
                        return Ok(offset as u64);
                    }
                }
            }
        }
        Ok(offset as u64)
    }
}

// Compiler-owned signatures establish the header layouts, exclusive destination,
// source lifetime and disjoint payloads. The helpers never retain borrowed pointers.
#[export_name = "__joky_buffer_append_bytes"]
pub(super) unsafe extern "C" fn append(
    destination: *mut Buffer,
    input: *const Buffer,
    from: u64,
    to: u64,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        Buffer::append_bytes(destination, input, from, to),
        source,
        start,
        end,
    );
}
#[export_name = "__joky_buffer_find_byte"]
pub(super) unsafe extern "C" fn find(
    input: *const Buffer,
    byte: u8,
    from: u64,
    to: u64,
    source: u64,
    start: usize,
    end: usize,
) -> u64 {
    located((*input).find_byte(byte, from, to), source, start, end)
}
#[export_name = "__joky_buffer_utf8_prefix"]
pub(super) unsafe extern "C" fn utf8_prefix(
    input: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> u64 {
    located((*input).utf8_prefix(), source, start, end)
}
