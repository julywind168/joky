//! Expression lowering modules for typed MIR.

mod builtins;
mod call_dispatch;
mod calls;
mod closures;
mod collections;
mod comparison;
mod cowns;
mod debug;
mod debug_aggregates;
mod debug_default;
mod debug_list;
mod dynamic;
mod effect_calls;
mod effects;
mod equality;
mod handlers;
mod hashing;
mod ordering;
mod ordering_aggregates;
mod resumable;
mod value;

use super::*;
