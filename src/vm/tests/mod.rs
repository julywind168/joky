//! Execution regressions grouped by behavior; shared helpers stay in this test target.
use super::*;
#[path = "containers/map_borrow_iter_tests.rs"]
mod map_borrow_iter_tests;
#[path = "language/range_pattern_tests.rs"]
mod range_pattern_tests;
#[path = "containers/range_slice_tests.rs"]
mod range_slice_tests;
#[path = "language/range_tests.rs"]
mod range_tests;
#[path = "containers/slice_borrow_iter_tests.rs"]
mod slice_borrow_iter_tests;

#[path = "containers/array_borrow_iter_tests.rs"]
mod array_borrow_iter_tests;

#[path = "containers/array_borrow_tests.rs"]
mod array_borrow_tests;
#[path = "containers/local_slice_tests.rs"]
mod local_slice_tests;

#[path = "containers/array_into_iter_tests.rs"]
mod array_into_iter_tests;

#[path = "containers/array_management_tests.rs"]
mod array_management_tests;

#[path = "containers/array_patterns_tests.rs"]
mod array_patterns_tests;

#[path = "containers/array_release_tests.rs"]
mod array_release_tests;

#[path = "containers/array_replace_tests.rs"]
mod array_replace_tests;

#[path = "containers/array_spread_tests.rs"]
mod array_spread_tests;

#[path = "containers/array_tests.rs"]
mod array_tests;

#[path = "containers/generic_map_tests.rs"]
mod generic_map_tests;

#[path = "containers/indexing_tests.rs"]
mod indexing_tests;

#[path = "containers/int_map_tests.rs"]
mod int_map_tests;

#[path = "containers/json_derive_tests.rs"]
mod json_derive_tests;

#[path = "containers/json_tests.rs"]
mod json_tests;

#[path = "containers/json_typed_tests.rs"]
mod json_typed_tests;

#[path = "containers/key_protocol_tests.rs"]
mod key_protocol_tests;

#[path = "containers/list_tests.rs"]
pub(crate) mod list_tests;

#[path = "containers/map_borrowing_tests.rs"]
mod map_borrowing_tests;

#[path = "containers/map_iteration_tests.rs"]
mod map_iteration_tests;

#[path = "containers/map_stress_tests.rs"]
mod map_stress_tests;

#[path = "containers/memory_replace_tests.rs"]
mod memory_replace_tests;

#[path = "containers/ordered_array_tests.rs"]
mod ordered_array_tests;

#[path = "containers/owned_map_tests.rs"]
mod owned_map_tests;

#[path = "containers/owning_element_tests.rs"]
mod owning_element_tests;

#[path = "containers/owning_slices_tests.rs"]
mod owning_slices_tests;

#[path = "containers/set_tests.rs"]
pub(crate) mod set_tests;

#[path = "containers/string_builder_tests.rs"]
mod string_builder_tests;

#[path = "containers/string_collections_tests.rs"]
mod string_collections_tests;

#[path = "containers/string_map_tests.rs"]
mod string_map_tests;

#[path = "containers/string_queries_tests.rs"]
mod string_queries_tests;

#[path = "containers/string_storage_tests.rs"]
mod string_storage_tests;

#[path = "containers/strings_tests.rs"]
mod strings_tests;

#[path = "containers/swap_remove_tests.rs"]
mod swap_remove_tests;

#[path = "embedding/borrowed_host_io_tests.rs"]
mod borrowed_host_io_tests;

#[path = "embedding/engine_api_tests.rs"]
mod engine_api_tests;

#[path = "embedding/ffi_tests.rs"]
pub(crate) mod ffi_tests;

#[path = "embedding/import_tests.rs"]
mod import_tests;

#[path = "embedding/instance_container_tests.rs"]
pub(crate) mod instance_container_tests;

#[path = "embedding/instance_tests.rs"]
mod instance_tests;

#[path = "embedding/interpreter_tests.rs"]
mod interpreter_tests;

#[path = "embedding/module_lifecycle_tests.rs"]
mod module_lifecycle_tests;

#[path = "embedding/object_host_tests.rs"]
mod object_host_tests;

#[path = "embedding/parity_tests.rs"]
mod parity_tests;

#[path = "embedding/ptr_tests.rs"]
mod ptr_tests;

#[path = "embedding/resource_byte_tests.rs"]
mod resource_byte_tests;

#[path = "embedding/resource_tests.rs"]
mod resource_tests;

#[path = "embedding/state_domain_tests.rs"]
pub(crate) mod state_domain_tests;

#[path = "embedding/stdlib_tests.rs"]
mod stdlib_tests;

#[cfg(unix)]
#[path = "embedding/text_ffi_tests.rs"]
mod text_ffi_tests;

#[path = "embedding/text_host_tests.rs"]
mod text_host_tests;

#[path = "language/aliases_tests.rs"]
mod aliases_tests;

#[path = "language/arena_scope_tests.rs"]
mod arena_scope_tests;

#[path = "language/arena_tests.rs"]
mod arena_tests;

#[path = "language/arithmetic_assignment_tests.rs"]
mod arithmetic_assignment_tests;

#[path = "language/bitwise_tests.rs"]
mod bitwise_tests;

#[path = "language/borrowed_dyn_tests.rs"]
mod borrowed_dyn_tests;

#[path = "language/borrowed_match_tests.rs"]
mod borrowed_match_tests;

#[path = "language/borrowed_method_tests.rs"]
mod borrowed_method_tests;

#[path = "language/builtin_calls_tests.rs"]
mod builtin_calls_tests;

#[path = "language/checked_failure_tests.rs"]
mod checked_failure_tests;

#[path = "language/closure_tests.rs"]
mod closure_tests;

#[path = "language/conditional_patterns_tests.rs"]
mod conditional_patterns_tests;
#[path = "language/for_indexed_tests.rs"]
mod for_indexed_tests;
#[path = "language/for_iterator_tests.rs"]
mod for_iterator_tests;

#[path = "language/conditionals_tests.rs"]
mod conditionals_tests;

#[path = "language/const_generic_tests.rs"]
mod const_generic_tests;

#[path = "language/conversion_tests.rs"]
mod conversion_tests;

#[path = "language/division_tests.rs"]
mod division_tests;

#[path = "language/drop_tests.rs"]
mod drop_tests;

#[path = "language/enums_tests.rs"]
mod enums_tests;

#[path = "language/explicit_arena_tests.rs"]
mod explicit_arena_tests;

#[path = "language/expressions_tests.rs"]
mod expressions_tests;

#[path = "language/fixed_array_tests.rs"]
mod fixed_array_tests;

#[path = "language/float_math_tests.rs"]
mod float_math_tests;

#[path = "language/float_tests.rs"]
mod float_tests;

#[path = "language/function_value_tests.rs"]
mod function_value_tests;

#[path = "language/generic_alias_tests.rs"]
mod generic_alias_tests;

#[path = "language/generic_enum_tests.rs"]
mod generic_enum_tests;

#[path = "language/generic_impl_tests.rs"]
mod generic_impl_tests;

#[path = "language/generic_method_tests.rs"]
mod generic_method_tests;

#[path = "language/generic_record_tests.rs"]
mod generic_record_tests;

#[path = "language/generic_tests.rs"]
mod generic_tests;

#[path = "language/graph_tests.rs"]
mod graph_tests;

#[path = "language/int64_tests.rs"]
mod int64_tests;

#[path = "language/jumps_tests.rs"]
mod jumps_tests;

#[path = "language/local_borrow_tests.rs"]
mod local_borrow_tests;

#[path = "language/local_field_tests.rs"]
mod local_field_tests;

#[path = "language/loops_tests.rs"]
mod loops_tests;

#[path = "language/method_tests.rs"]
mod method_tests;

#[path = "language/multi_owner_tests.rs"]
mod multi_owner_tests;

#[path = "language/mutable_borrow_tests.rs"]
mod mutable_borrow_tests;

#[path = "language/narrow_tests.rs"]
mod narrow_tests;

#[path = "language/noescape_tests.rs"]
mod noescape_tests;

#[path = "language/owner_field_tests.rs"]
mod owner_field_tests;

#[path = "language/record_patterns_tests.rs"]
mod record_patterns_tests;

#[path = "language/records_tests.rs"]
mod records_tests;

#[path = "language/recursive_owner_tests.rs"]
mod recursive_owner_tests;

#[path = "language/sequences_tests.rs"]
mod sequences_tests;

#[path = "language/show_tests.rs"]
mod show_tests;

#[path = "language/structured_value_tests.rs"]
mod structured_value_tests;

#[path = "language/trait_tests.rs"]
mod trait_tests;

#[path = "language/tuples_tests.rs"]
mod tuples_tests;

#[path = "language/uint8_tests.rs"]
mod uint8_tests;

#[path = "language/unsigned_tests.rs"]
mod unsigned_tests;

#[path = "support.rs"]
mod support;

#[path = "tasks/arena_suspension_tests.rs"]
mod arena_suspension_tests;

#[cfg(unix)]
#[path = "tasks/async_borrowing_tests.rs"]
mod async_borrowing_tests;

#[path = "tasks/async_owner_borrow_tests.rs"]
mod async_owner_borrow_tests;

#[path = "tasks/buffered_tcp_split_tests.rs"]
mod buffered_tcp_split_tests;

#[path = "tasks/buffered_tcp_tests.rs"]
mod buffered_tcp_tests;

#[path = "tasks/executor_tests.rs"]
mod executor_tests;

#[path = "tasks/json_tcp_tests.rs"]
mod json_tcp_tests;

#[path = "tasks/concurrent_block_tests.rs"]
mod concurrent_block_tests;
#[path = "tasks/race_tests.rs"]
mod race_tests;

#[path = "tasks/reactor_tests.rs"]
mod reactor_tests;

#[path = "tasks/scheduler_tests.rs"]
mod scheduler_tests;

#[path = "tasks/suspending_borrow_tests.rs"]
mod suspending_borrow_tests;

#[path = "tasks/suspending_closure_tests.rs"]
mod suspending_closure_tests;
#[path = "tasks/suspending_function_value_tests.rs"]
mod suspending_function_value_tests;

#[path = "tasks/suspending_owner_tests.rs"]
mod suspending_owner_tests;

#[path = "tasks/isolate_tests.rs"]
mod isolate_tests;
#[path = "tasks/task_scope_tests.rs"]
mod task_scope_tests;

#[path = "tasks/tcp_split_tests.rs"]
mod tcp_split_tests;

#[path = "tasks/tcp_tests.rs"]
mod tcp_tests;

#[path = "language/service_ergonomics_tests.rs"]
mod service_ergonomics_tests;

#[path = "language/mutable_parameters_tests.rs"]
mod mutable_parameters_tests;

#[path = "tasks/suspending_dyn_tests.rs"]
mod suspending_dyn_tests;

#[path = "tasks/duration_tests.rs"]
mod duration_tests;

#[path = "language/add_tests.rs"]
mod add_tests;

#[path = "tasks/suspending_arena_tests.rs"]
mod suspending_arena_tests;

#[path = "tasks/time_tests.rs"]
mod time_tests;
