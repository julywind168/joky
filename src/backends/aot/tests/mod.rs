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
use scratch::ScratchDir;

mod aggregates;
mod arena;
mod arrays;
mod compilation;
mod control_flow;
mod function_values;
mod generics;
mod integers;
mod interop;
mod members;
mod ownership;
mod string_collections;
mod strings;
mod support;
mod suspension;

use support::check_arena_cps_fixture;
pub(crate) use support::{assert_sync_int32, assert_sync_results};
