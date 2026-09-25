//! Runtime implementation shared by AOT programs.

pub(crate) mod abi;
pub(crate) mod aot_exports;
pub(crate) mod batch;
pub(crate) mod blocking;
pub(crate) mod bytes;
pub(crate) mod debug;
pub(crate) use joky_runtime_core::c_memory;
pub(crate) mod callback;
pub(crate) mod continuation;
pub(crate) mod cown;
pub(crate) use joky_runtime_core::destruction;
pub(crate) mod env;
pub(crate) mod file;
pub(crate) mod handler;
pub(crate) mod path;
pub(crate) use joky_runtime_core::io;
pub(crate) mod hashing;
pub(crate) mod list;
pub(crate) mod managed;
pub(crate) mod map;
pub(crate) mod mut_list;
pub(crate) mod mut_map;
pub(crate) mod process;
pub(crate) mod provider;
pub(crate) mod random;
pub(crate) mod reactor;
pub(crate) mod region;
pub(crate) mod resources;
pub(crate) mod scope;
pub(crate) mod socket;
pub(crate) mod sqlite;
pub(crate) mod string;
pub(crate) mod task;

#[cfg(test)]
pub(crate) fn effect_abi_descriptor() -> abi::EffectAbiDescriptor {
    abi::EffectAbiDescriptor {
        version: 1,
        operation_word_size: std::mem::size_of::<u64>() as u32,
        payload_alignment: std::mem::align_of::<u64>() as u32,
    }
}
