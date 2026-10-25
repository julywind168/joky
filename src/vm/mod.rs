//! Public VM embedding interface.
mod engine;
mod instances;
pub(crate) mod state_domains;
pub(crate) use state_domains::queue_current_i32_call;
pub use state_domains::{
    queue_current_domain_call, DomainHandle, DomainLimits, DomainOutcome, DomainOwner,
    DomainRequest, PeriodicStatus,
};
mod limits;
pub(crate) mod parallel;
pub use crate::core::instance::InstanceValue;
pub use engine::{VmEngine, VmMode};
pub use instances::{
    call_current_instance, current_instance_id, InstanceCleanupReport, InstanceId, InstanceLimits,
    InstanceStatus,
};
pub use limits::InterpreterLimits;
pub use parallel::{
    ParallelExecution, ParallelOutput, ParallelTask, PreparedParallelTask, TaskMap, TaskSet,
    TaskValue,
};

#[cfg(test)]
pub(crate) mod tests;
