//! Stable contracts shared by generated code and the Joky runtime.
//!
//! These descriptors intentionally contain sizes, offsets, and status values
//! only. Keeping them in a dependency-free crate establishes the boundary for
//! the independent runtime archive.

pub mod symbols;

/// Versioned symbol that every compatible AOT runtime archive must export.
/// Bump both values when generated objects can no longer use an older runtime.
pub const AOT_RUNTIME_ABI_VERSION: u32 = 28;
pub const AOT_RUNTIME_ABI_SYMBOL: &str = "jk_aot_runtime_abi_v28";

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TaskAbiDescriptor {
    pub version: u32,
    pub context_size: u32,
    pub captures_offset: i32,
    pub result_offset: i32,
    pub cancellation_offset: i32,
    pub continuation_offset: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ContinuationAbiDescriptor {
    pub version: u32,
    pub state_ready: u8,
    pub state_sleeping: u8,
    pub state_resuming: u8,
    pub state_cancelled: u8,
    pub state_completed: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FunctionPendingAbiDescriptor {
    pub version: u32,
    pub state_ready: u8,
    pub state_pending: u8,
    pub state_failed: u8,
    pub state_cancelled: u8,
    pub result_storage: u8,
    pub failure_storage: u8,
    pub status_size: u8,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Status of a function call activation.
pub enum FunctionCallStatus {
    Ready = 0,
    Pending = 1,
    Failed = 2,
    Cancelled = 3,
}

// Stable cleanup storage tags used by generated continuation code.
pub const CONTINUATION_FRAME_STORAGE: u8 = 0;
pub const CONTINUATION_SPILL_STORAGE: u8 = 1;
pub const CONTINUATION_RESULT_STORAGE: u8 = 2;
pub const CONTINUATION_SUSPEND_RESULT_STORAGE: u8 = 3;
pub const CONTINUATION_SUSPEND_ARGUMENT_STORAGE: u8 = 4;
pub const CONTINUATION_FAILURE_STORAGE: u8 = 5;

// Stable 64-bit TaskContext layout consumed by generated task thunks.
pub const TASK_CONTEXT_CAPTURES_OFFSET: i32 = 0;
pub const TASK_CONTEXT_RESULT_OFFSET: i32 = 8;
pub const TASK_CONTEXT_RESULT_SIZE_OFFSET: i32 = 16;
pub const TASK_CONTEXT_DROP_RESULT_OFFSET: i32 = 24;
pub const TASK_CONTEXT_CANCELLED_RESULT_OFFSET: i32 = 32;
pub const TASK_CONTEXT_RESULT_INITIALIZED_OFFSET: i32 = 33;
pub const TASK_CONTEXT_CANCELLATION_OFFSET: i32 = 40;
pub const TASK_CONTEXT_CONTINUATION_OFFSET: i32 = 96;
pub const TASK_CONTEXT_SIZE: u32 = 104;

/// Ownership for one pointer-sized slot in a handler environment.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerEnvOwnership {
    Borrowed = 0,
    Owned = 1,
    Shared = 2,
}

// Stable 64-bit HandlerCall and HandlerEnvSlot layouts.
pub const HANDLER_CALL_REQUEST_OFFSET: i32 = 8;
pub const HANDLER_CALL_CAPTURES_OFFSET: i32 = 24;
pub const HANDLER_CALL_RESULT_OFFSET: i32 = 40;
pub const HANDLER_CALL_RESULT_SIZE_OFFSET: i32 = 56;
pub const HANDLER_ENV_SLOT_SIZE: i32 = 24;
pub const HANDLER_ENV_SLOT_OFFSET_OFFSET: i32 = 0;
pub const HANDLER_ENV_SLOT_OWNERSHIP_OFFSET: i32 = 8;
pub const HANDLER_ENV_SLOT_DROP_CALLBACK_OFFSET: i32 = 16;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HandlerAbiDescriptor {
    pub version: u32,
    pub call_size: u32,
    pub env_slot_size: u32,
    pub request_offset: i32,
    pub captures_offset: i32,
    pub result_offset: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ManagedAbiDescriptor {
    pub version: u32,
    pub header_size: u32,
    pub cown_state_size: u32,
    pub cown_id_offset: i32,
    pub cown_payload_offset: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EffectAbiDescriptor {
    pub version: u32,
    pub operation_word_size: u32,
    pub payload_alignment: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FailureAbiDescriptor {
    pub version: u32,
    pub operation_word_size: u32,
    pub payload_alignment: u32,
    pub drop_callback_size: u32,
    pub state_empty: u8,
    pub state_pending: u8,
    pub state_claimed: u8,
    pub state_cleaned: u8,
}
