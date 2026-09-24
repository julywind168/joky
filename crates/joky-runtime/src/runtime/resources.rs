//! Production resource accounting for one runtime scope.
//!
//! The counters intentionally track runtime ownership only.  Test-only
//! process allocation metrics live in the compiler crate and must not install
//! a global allocator in the standalone runtime archive.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Runtime-owned resource categories.  Provider-specific categories remain
/// here so provider adapters can use the same scope accounting without making
/// the standalone runtime depend on the compiler or semantic analyzer.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Continuations,
    TaskGroups,
    HandlerFrames,
    FileRequests,
    FileArgumentBytes,
    FileHandles,
}

const COUNT: usize = 6;

/// Per-scope counters shared by all leases owned by that scope.
#[derive(Default)]
pub(crate) struct Counters {
    values: [AtomicUsize; COUNT],
}

impl Counters {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn snapshot(&self) -> [usize; COUNT] {
        std::array::from_fn(|index| self.values[index].load(Ordering::SeqCst))
    }
}

/// A scope-owned count of one resource category.
pub(crate) struct Lease {
    counters: Arc<Counters>,
    kind: Kind,
    amount: usize,
}

impl Lease {
    pub(crate) fn new(scope: &super::scope::RuntimeScope, kind: Kind, amount: usize) -> Self {
        let counters = Arc::clone(&scope.resources);
        counters.values[kind as usize].fetch_add(amount, Ordering::SeqCst);
        Self {
            counters,
            kind,
            amount,
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.counters.values[self.kind as usize].fetch_sub(self.amount, Ordering::SeqCst);
    }
}
