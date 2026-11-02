//! SHA-256 and HMAC-SHA256 using RustCrypto.
mod streaming;
use super::{bounds_error, located, Buffer, Error, BYTE_CHUNK};
use crate::runtime::parallel::pool::check_cancelled;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
pub(crate) use streaming::*;

type HmacSha256 = Hmac<Sha256>;
fn digest(input: &[u8]) -> Result<[u8; 32], Error> {
    let mut state = Sha256::new();
    for chunk in input.chunks(BYTE_CHUNK) {
        check_cancelled()?;
        Digest::update(&mut state, chunk);
    }
    check_cancelled()?;
    Ok(state.finalize().into())
}
fn mac(key: &[u8], input: &[u8]) -> Result<HmacSha256, Error> {
    check_cancelled()?;
    // HMAC hashes keys longer than its 64-byte block. Chunk that step too, so
    // cancellation is checked even while processing a very large key.
    let hashed;
    let key = if key.len() > 64 {
        hashed = digest(key)?;
        &hashed[..]
    } else {
        key
    };
    let mut state = HmacSha256::new_from_slice(key)
        .map_err(|_| bounds_error("Invalid HMAC-SHA256 key length"))?;
    for chunk in input.chunks(BYTE_CHUNK) {
        check_cancelled()?;
        Mac::update(&mut state, chunk);
    }
    check_cancelled()?;
    Ok(state)
}
impl Buffer {
    fn crypto_output(&self) -> Result<(), Error> {
        if self.bytes()?.len() != 32 {
            return Err(bounds_error(
                "Cryptographic output requires exactly 32 bytes",
            ));
        }
        Ok(())
    }
    // The destination has a live exclusive loan; sources have shared loans.
    // No payload pointer escapes, and only a completed result is written.
    pub(crate) fn sha256(&mut self, input: &Buffer) -> Result<(), Error> {
        self.crypto_output()?;
        let result = digest(input.bytes()?)?;
        unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), self.data.as_ptr(), 32) };
        Ok(())
    }
    pub(crate) fn hmac_sha256(&mut self, key: &Buffer, input: &Buffer) -> Result<(), Error> {
        self.crypto_output()?;
        let result = mac(key.bytes()?, input.bytes()?)?.finalize().into_bytes();
        unsafe { std::ptr::copy_nonoverlapping(result.as_ptr(), self.data.as_ptr(), 32) };
        Ok(())
    }
    pub(crate) fn verify_hmac_sha256(&self, input: &Buffer, tag: &Buffer) -> Result<bool, Error> {
        let key = self.bytes()?;
        let input = input.bytes()?;
        let tag = tag.bytes()?;
        check_cancelled()?;
        if tag.len() != 32 {
            return Ok(false);
        }
        Ok(mac(key, input)?.verify_slice(tag).is_ok())
    }
}

/// # Safety
/// All pointers name live initialized byte buffers/views on the current task.
/// Destination storage has an exclusive loan disjoint from every source; input
/// storage has a shared loan. No pointer is retained beyond this call.
#[export_name = "__joky_buffer_sha256"]
pub(super) unsafe extern "C" fn sha256(
    destination: *mut Buffer,
    input: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located((*destination).sha256(&*input), source, start, end)
}
/// # Safety
/// Same lifetime, layout and disjoint destination contract as `sha256`; both
/// key and input have shared loans and may overlap each other.
#[export_name = "__joky_buffer_hmac_sha256"]
pub(super) unsafe extern "C" fn hmac_sha256(
    destination: *mut Buffer,
    key: *const Buffer,
    input: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) {
    located(
        (*destination).hmac_sha256(&*key, &*input),
        source,
        start,
        end,
    )
}
/// # Safety
/// All pointers name live initialized byte buffers/views with shared loans on
/// the current task. Inputs may overlap; no pointer or reference is retained.
#[export_name = "__joky_buffer_verify_hmac_sha256"]
pub(super) unsafe extern "C" fn verify_hmac_sha256(
    key: *const Buffer,
    input: *const Buffer,
    tag: *const Buffer,
    source: u64,
    start: usize,
    end: usize,
) -> u8 {
    located(
        (*key).verify_hmac_sha256(&*input, &*tag).map(u8::from),
        source,
        start,
        end,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::arena::{Arena, LIVE};

    #[test]
    fn crypto_runtime_rejects_invalid_layouts_and_output_lengths_before_writing() {
        let mut arena = Arena::default();
        let input = Buffer::copy_bytes(&mut arena, b"abc").unwrap();
        let wrong = Buffer::allocate(&mut arena, 1, 8, 8).unwrap();
        let mut short = Buffer::copy_bytes(&mut arena, &[7; 31]).unwrap();
        let mut output = Buffer::copy_bytes(&mut arena, &[7; 32]).unwrap();
        unsafe {
            assert!(short.as_mut().sha256(input.as_ref()).is_err());
            assert!(short
                .as_mut()
                .hmac_sha256(input.as_ref(), input.as_ref())
                .is_err());
            assert_eq!(short.as_ref().bytes().unwrap(), &[7; 31]);
            assert!(output.as_mut().sha256(wrong.as_ref()).is_err());
            assert!(output
                .as_mut()
                .hmac_sha256(wrong.as_ref(), input.as_ref())
                .is_err());
            assert!(output
                .as_mut()
                .hmac_sha256(input.as_ref(), wrong.as_ref())
                .is_err());
            assert_eq!(output.as_ref().bytes().unwrap(), &[7; 32]);
            assert!(input
                .as_ref()
                .verify_hmac_sha256(input.as_ref(), wrong.as_ref())
                .is_err());
            assert!(input
                .as_ref()
                .verify_hmac_sha256(wrong.as_ref(), output.as_ref())
                .is_err());
            assert!(wrong
                .as_ref()
                .verify_hmac_sha256(input.as_ref(), output.as_ref())
                .is_err());
            assert!(!input
                .as_ref()
                .verify_hmac_sha256(input.as_ref(), short.as_ref())
                .unwrap());
        }
        drop(arena);
        LIVE.with(|n| assert_eq!(n.get(), 0));
    }
}
