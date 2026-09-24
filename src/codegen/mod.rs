//! Cranelift JIT code generation backend
//!
//! The backend only consumes `MirProgram`s that pass the MIR verifier and is
//! split by responsibility into functions, control flow, class layout,
//! constructors, operators, and ABI bindings, and maps `RuntimeIntrinsic`s to
//! symbols defined by `joky-runtime-abi` and provided by the standalone runtime

mod abi;
mod classes;
mod constructors;
mod cranelift;
pub(crate) mod debug;
mod environment;
mod functions;
mod hashing;
mod helpers;
mod object;
mod operators;
mod types;

pub(crate) use cranelift::CraneliftBackend;
pub(crate) use object::emit_mir_object_with_debug;
pub(crate) use object::emit_mir_object_with_entries;
