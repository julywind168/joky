//! Public VM embedding interface.
mod engine;
#[cfg(test)]
pub(crate) mod instance_container_tests;
#[cfg(test)]
mod instance_tests;
mod instances;
pub(crate) mod state_domains;
pub(crate) use state_domains::queue_current_i32_call;
pub use state_domains::{
    queue_current_domain_call, DomainHandle, DomainLimits, DomainOutcome, DomainOwner,
    DomainRequest,
};
mod limits;
#[cfg(test)]
mod show_tests;
#[cfg(test)]
pub(crate) mod state_domain_tests;
pub use crate::core::instance::InstanceValue;
pub use engine::{VmEngine, VmMode};
pub use instances::{
    call_current_instance, current_instance_id, InstanceCleanupReport, InstanceId, InstanceLimits,
    InstanceStatus,
};
pub use limits::InterpreterLimits;

#[cfg(test)]
mod aliases_tests;
#[cfg(test)]
mod array_borrow_tests;
#[cfg(test)]
mod array_tests;
#[cfg(all(test, unix))]
mod async_borrowing_tests;
#[cfg(test)]
mod async_owner_borrow_tests;
#[cfg(test)]
mod builtin_calls_tests;
#[cfg(test)]
mod conditionals_tests;
#[cfg(test)]
mod division_tests;
#[cfg(test)]
mod engine_api_tests;
#[cfg(test)]
mod enums_tests;
#[cfg(test)]
mod expressions_tests;
#[cfg(test)]
mod interpreter_tests;
#[cfg(test)]
mod jumps_tests;
#[cfg(test)]
mod loops_tests;
#[cfg(test)]
mod module_lifecycle_tests;
#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod race_tests;
#[cfg(test)]
mod record_patterns_tests;
#[cfg(test)]
mod records_tests;
#[cfg(test)]
mod scheduler_tests;
#[cfg(test)]
mod sequences_tests;
#[cfg(test)]
mod string_builder_tests;
#[cfg(test)]
mod string_collections_tests;
#[cfg(test)]
mod string_queries_tests;
#[cfg(test)]
mod string_storage_tests;
#[cfg(test)]
mod strings_tests;
#[cfg(test)]
mod support;
#[cfg(test)]
mod task_scope_tests;
#[cfg(test)]
mod tuples_tests;

#[cfg(test)]
mod float_math_tests;
#[cfg(test)]
mod float_tests;
#[cfg(test)]
mod int64_tests;
#[cfg(test)]
mod narrow_tests;

#[cfg(test)]
mod uint8_tests;
#[cfg(test)]
mod unsigned_tests;

#[cfg(test)]
mod bitwise_tests;

#[cfg(test)]
mod conversion_tests;

#[cfg(test)]
mod generic_tests;

#[cfg(test)]
mod generic_enum_tests;

#[cfg(test)]
mod generic_alias_tests;
#[cfg(test)]
mod generic_impl_tests;
#[cfg(test)]
mod generic_method_tests;
#[cfg(test)]
mod generic_record_tests;

#[cfg(test)]
mod method_tests;

#[cfg(test)]
mod import_tests;

#[cfg(test)]
mod stdlib_tests;

#[cfg(test)]
mod arena_scope_tests;
#[cfg(test)]
mod arena_suspension_tests;
#[cfg(test)]
mod arena_tests;
#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod local_borrow_tests;
#[cfg(test)]
mod local_field_tests;

#[cfg(test)]
pub(crate) mod ffi_tests;
#[cfg(test)]
mod ptr_tests;

#[cfg(test)]
mod array_release_tests;

#[cfg(test)]
mod array_management_tests;
#[cfg(test)]
mod array_replace_tests;
#[cfg(test)]
mod borrowed_match_tests;
#[cfg(test)]
mod borrowed_method_tests;
#[cfg(test)]
mod checked_failure_tests;
#[cfg(test)]
mod generic_map_tests;
#[cfg(test)]
mod int_map_tests;
#[cfg(test)]
mod key_protocol_tests;
#[cfg(test)]
mod map_borrowing_tests;
#[cfg(test)]
mod map_iteration_tests;
#[cfg(test)]
mod map_stress_tests;
#[cfg(test)]
mod memory_replace_tests;
#[cfg(test)]
mod mutable_borrow_tests;
#[cfg(test)]
mod ordered_array_tests;
#[cfg(test)]
mod owned_map_tests;
#[cfg(test)]
mod owning_slices_tests;
#[cfg(test)]
mod swap_remove_tests;

#[cfg(test)]
mod borrowed_dyn_tests;
#[cfg(test)]
mod function_value_tests;
#[cfg(test)]
mod suspending_function_value_tests;
#[cfg(test)]
mod trait_tests;

#[cfg(test)]
mod drop_tests;
#[cfg(test)]
mod json_tcp_tests;
#[cfg(test)]
mod json_tests;
#[cfg(test)]
mod json_typed_tests;
#[cfg(test)]
mod multi_owner_tests;
#[cfg(test)]
mod owner_field_tests;
#[cfg(test)]
mod owning_element_tests;
#[cfg(test)]
mod recursive_owner_tests;

#[cfg(test)]
mod text_host_tests;

#[cfg(test)]
mod noescape_tests;
#[cfg(test)]
mod string_map_tests;

#[cfg(all(test, unix))]
mod text_ffi_tests;

#[cfg(test)]
mod object_host_tests;

#[cfg(test)]
mod executor_tests;

#[cfg(test)]
mod closure_tests;

#[cfg(test)]
pub(crate) mod list_tests;

#[cfg(test)]
pub(crate) mod set_tests;

#[cfg(test)]
mod net0_tests;

#[cfg(test)]
mod suspending_owner_tests;

#[cfg(test)]
mod resource_tests;

#[cfg(test)]
mod resource_byte_tests;

#[cfg(test)]
mod reactor_tests;

#[cfg(test)]
mod tcp_split_tests;
#[cfg(test)]
mod tcp_tests;

#[cfg(test)]
mod buffered_tcp_split_tests;
#[cfg(test)]
mod buffered_tcp_tests;

#[cfg(test)]
mod json_derive_tests;

#[cfg(test)]
mod arithmetic_assignment_tests;

#[cfg(test)]
mod conditional_patterns_tests;
