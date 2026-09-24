//! Cranelift function compilation modules.

mod compile;
mod context;
mod continuation;
mod continuations;
mod ordinary;
mod ordinary_continuations;
mod pending;
mod runtime;
mod runtime_ids;
mod spills;
mod statements;
mod task_frame;
mod thunks;
pub(super) mod values;

pub(super) use context::CompileContext;
pub(super) use context::StringValue;
pub(super) use statements::duplicate_shared_value;
