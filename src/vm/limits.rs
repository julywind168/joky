//! Resource limits for one interpreter invocation, including its scope branches.

/// Interpreter limits are copied when a call starts. Existing continuations keep
/// their original limits after the VM is reconfigured or reloaded.
///
/// Budgets count bytecode instructions and native CPS polls, not elapsed time.
/// Native callbacks must still return promptly; they cannot be preempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterpreterLimits {
    /// Maximum number of simultaneously live user and native CPS frames.
    pub max_frames: usize,
    /// Total work allowed across the entire call, including every resume.
    /// `None` is unlimited; `Some(0)` rejects the first instruction.
    pub instruction_budget: Option<u64>,
    /// Work per asynchronous poll before yielding cooperatively. Must be positive
    /// when set. Synchronous calls use only the total instruction budget.
    pub poll_budget: Option<u64>,
}

impl Default for InterpreterLimits {
    fn default() -> Self {
        Self {
            max_frames: 1024,
            instruction_budget: None,
            poll_budget: None,
        }
    }
}
