//! PHC validation precedes work admission and every variable-size allocation.
use super::{
    allocation_error, failure, Error, Packet, INVALID_HASH, MAX_ENCODED, PARAMETERS,
    RANDOM_UNAVAILABLE, UNSUPPORTED,
};
use argon2::{
    password_hash::phc::{Output, ParamsString, PasswordHash, Salt},
    Algorithm, Argon2, Block, Params, Version,
};
use std::fmt::Write;
use zeroize::Zeroizing;
const MEMORY_KIB: u32 = 19456;
const ITERATIONS: u32 = 2;
const SALT_BYTES: usize = 16;
const HASH_BYTES: usize = 32;

pub(super) struct Plan {
    params: Params,
    salt: Salt,
    expected: Option<Output>,
}
impl Plan {
    pub(super) fn generate<E>(fill: impl FnOnce(&mut [u8]) -> Result<(), E>) -> Result<Self, u64> {
        let mut salt = [0u8; SALT_BYTES];
        fill(&mut salt).map_err(|_| RANDOM_UNAVAILABLE)?;
        Ok(Self {
            params: Params::new(MEMORY_KIB, ITERATIONS, 1, Some(HASH_BYTES))
                .expect("fixed password policy"),
            salt: Salt::new(&salt).expect("fixed salt size"),
            expected: None,
        })
    }
    pub(super) fn parse(encoded: &[u8]) -> Result<Self, u64> {
        if encoded.len() > MAX_ENCODED {
            return Err(INVALID_HASH);
        }
        let text = std::str::from_utf8(encoded).map_err(|_| INVALID_HASH)?;
        let hash = PasswordHash::new(text).map_err(|_| INVALID_HASH)?;
        if hash.algorithm.as_str() != "argon2id" || hash.version.is_some_and(|v| v != 19) {
            return Err(UNSUPPORTED);
        }
        if hash.version.is_none()
            || hash.params.iter().count() != 3
            || ["m", "t", "p"]
                .iter()
                .any(|key| hash.params.get_decimal(*key).is_none())
        {
            return Err(INVALID_HASH);
        }
        let params = Params::try_from(&hash).map_err(|_| PARAMETERS)?;
        let salt = hash.salt.ok_or(INVALID_HASH)?;
        let expected = hash.hash.ok_or(INVALID_HASH)?;
        if params.m_cost() > 65536
            || params.t_cost() > 4
            || params.p_cost() > 4
            || salt.len() < 8
            || salt.len() > 64
            || expected.len() < 16
            || expected.len() > 64
        {
            return Err(PARAMETERS);
        }
        Ok(Self {
            params,
            salt,
            expected: Some(expected),
        })
    }
    pub(super) fn needs_rehash(&self) -> bool {
        self.params.m_cost() < MEMORY_KIB
            || self.params.t_cost() < ITERATIONS
            || self.salt.len() < SALT_BYTES
            || self.expected.as_ref().is_none_or(|h| h.len() < HASH_BYTES)
    }
    pub(super) fn memory_bytes(&self) -> usize {
        self.params.block_count() * std::mem::size_of::<Block>()
    }
    pub(super) fn compute(self, password: &[u8]) -> Result<Packet, Error> {
        let mut blocks = Zeroizing::new(Vec::new());
        blocks
            .try_reserve_exact(self.params.block_count())
            .map_err(|_| allocation_error())?;
        blocks.resize(self.params.block_count(), Block::default());
        let mut bytes = Zeroizing::new([0u8; 64]);
        let len = self.params.output_len().unwrap_or(HASH_BYTES);
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone());
        argon
            .hash_password_into_with_memory(
                password,
                &self.salt,
                &mut bytes[..len],
                blocks.as_mut_slice(),
            )
            .map_err(|_| failure("Argon2id computation failed"))?;
        let actual = Output::new(&bytes[..len]).map_err(|_| failure("Invalid Argon2id output"))?;
        if let Some(expected) = self.expected {
            // PHC Output's PartialEq delegates to its constant-time comparison.
            return Ok(Packet::bytes(&[u8::from(actual == expected)]));
        }
        let hash = PasswordHash {
            algorithm: Algorithm::Argon2id.ident(),
            version: Some(19),
            params: ParamsString::try_from(&self.params)
                .map_err(|_| failure("Invalid Argon2id parameters"))?,
            salt: Some(self.salt),
            hash: Some(actual),
        };
        let mut packet = Packet::error(0);
        write!(&mut packet, "{hash}")
            .map_err(|_| failure("Password hash encoding exceeded its limit"))?;
        Ok(packet)
    }
}
