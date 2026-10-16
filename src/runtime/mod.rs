//! Single-threaded continuation runtime.
pub(crate) mod abi;
pub(crate) mod bindings;
pub mod borrowed_host;
pub mod cps;
pub mod dns;
pub mod driver;
pub mod error;
pub mod executor;
pub(crate) mod float_env;
pub mod reactor;
pub mod scheduler;
pub mod suspension;

pub(crate) mod owned_text;
pub mod string;
pub mod text_ffi;

pub(crate) mod arena;
pub(crate) mod buffer;

pub(crate) mod cleanup;

pub mod byte_request;
pub mod resource;
pub(crate) mod tcp;

#[cfg(test)]
mod state_domain_probe;

pub(crate) mod domain;

pub mod fs;
