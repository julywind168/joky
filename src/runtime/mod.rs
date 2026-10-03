//! Single-threaded continuation runtime.
pub(crate) mod abi;
pub(crate) mod bindings;
pub mod borrowed_host;
pub mod cps;
pub mod driver;
pub mod error;
pub mod scheduler;
pub mod suspension;

pub(crate) mod owned_text;
pub mod string;
pub mod text_ffi;

pub(crate) mod arena;
pub(crate) mod buffer;

pub(crate) mod cleanup;
