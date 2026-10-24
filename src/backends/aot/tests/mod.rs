use super::*;
use crate::parser::test_support::{with_suspends, Suspends};
use crate::{FunctionSignature, Type};
use std::process::Command;

#[cfg(unix)]
#[path = "../../../../tests/support/native.rs"]
mod native;
#[cfg(unix)]
#[path = "../../../../tests/support/runtime.rs"]
mod runtime;
#[path = "../../../../tests/support/scratch.rs"]
mod scratch;

#[cfg(unix)]
use native::{c_compiler, link, link_runtime};
pub(crate) use scratch::ScratchDir;

mod aggregates;
mod arena;
mod arrays;
mod borrowed_dyn;
mod closures;
mod compilation;
mod control_flow;
#[cfg(unix)]
mod fixed_array_ffi;
#[cfg(unix)]
mod fixed_array_model;
mod floats;
#[cfg(unix)]
mod for_indexed;
mod function_values;
mod generics;
mod instances;
mod integers;
mod interop;
mod members;
mod narrow;
mod ownership;
mod string_collections;
mod strings;
mod support;
mod suspension;

#[cfg(unix)]
mod task_parallel_probe;

use support::check_arena_cps_fixture;
pub(crate) use support::{assert_sync_int32, assert_sync_results};

mod text_ffi;

#[cfg(unix)]
mod lists;

#[cfg(unix)]
mod collections;

mod explicit_arena;
#[cfg(unix)]
mod sets;

#[cfg(unix)]
mod task_parallel_storage;
