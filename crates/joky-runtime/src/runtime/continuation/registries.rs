use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use super::{Continuation, ScopeId, SuspendProvider};

/// Machine entries are published after JIT finalization or by the AOT launcher
/// before main. Keys are scoped and stored in continuation headers.
static MACHINE_ENTRIES: OnceLock<Mutex<HashMap<(ScopeId, usize), usize>>> = OnceLock::new();
static SUSPEND_PROVIDERS: OnceLock<Mutex<HashMap<(ScopeId, u64), SuspendProvider>>> =
    OnceLock::new();
/// Suspended tasks are cancelled concurrently with completion. Keep an Arc
/// keyed by task context so cancellation never has to dereference the raw ABI
/// continuation handle after its public handle has been retired.
static TASK_CONTINUATIONS: OnceLock<Mutex<HashMap<usize, Continuation>>> = OnceLock::new();
/// Handles are nonzero, never-reused integer tokens carried in a pointer-sized
/// ABI word. They are NOT allocation addresses and must never be dereferenced.
/// Only active entries consume memory; the monotonically increasing serial is
/// constant-sized. Exhaustion fails closed instead of wrapping to an old token.
static HANDLES: OnceLock<Mutex<HandleRegistry>> = OnceLock::new();

#[derive(Default)]
struct HandleRegistry {
    last: usize,
    entries: HashMap<usize, Continuation>,
}

impl HandleRegistry {
    fn insert(&mut self, continuation: Continuation) -> Option<usize> {
        let token = self.last.checked_add(1)?;
        self.last = token;
        continuation
            .inner
            .owner_handle
            .store(token, Ordering::Release);
        self.entries.insert(token, continuation);
        Some(token)
    }
}

fn handles() -> &'static Mutex<HandleRegistry> {
    HANDLES.get_or_init(|| Mutex::new(HandleRegistry::default()))
}

pub(super) fn suspend_providers() -> &'static Mutex<HashMap<(ScopeId, u64), SuspendProvider>> {
    SUSPEND_PROVIDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn machine_entries() -> &'static Mutex<HashMap<(ScopeId, usize), usize>> {
    MACHINE_ENTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn task_continuations() -> &'static Mutex<HashMap<usize, Continuation>> {
    TASK_CONTINUATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn register_handle(continuation: Continuation) -> *mut Continuation {
    let token = handles()
        .lock()
        .expect("continuation handle mutex")
        .insert(continuation)
        .unwrap_or_else(|| {
            eprintln!("Joky continuation handle namespace exhausted");
            std::process::abort();
        });
    std::ptr::without_provenance_mut(token)
}

// Internal ownership traversal may retain a parent whose public free is
// pending until its children drain; do not admit external callbacks this way.
pub(super) fn retain_owner(token: usize) -> Option<Continuation> {
    handles()
        .lock()
        .expect("continuation handle mutex")
        .entries
        .get(&token)
        .cloned()
}

pub(super) fn unregister_handle(handle: *mut Continuation) {
    let removed = handles()
        .lock()
        .expect("continuation handle mutex")
        .entries
        .remove(&(handle as usize));
    // Continuation destructors must not run while the registry is locked.
    drop(removed);
}

/// Clone the runtime state for an ABI callback without dereferencing the
/// opaque token.
pub(super) unsafe fn retain_handle(handle: *mut Continuation) -> Option<Continuation> {
    if handle.is_null() {
        return None;
    }
    let continuation = handles()
        .lock()
        .expect("continuation handle mutex")
        .entries
        .get(&(handle as usize))
        .cloned()?;
    let _gate = continuation
        .inner
        .lifecycle_gate
        .lock()
        .expect("continuation lifecycle mutex");
    if continuation.inner.owner_handle.load(Ordering::Acquire) != handle as usize
        || continuation.inner.free_requested.load(Ordering::Acquire)
        || continuation.inner.handle_released.load(Ordering::Acquire)
    {
        return None;
    }
    drop(_gate);
    Some(continuation)
}

pub(super) unsafe fn retain_const_handle(handle: *const Continuation) -> Option<Continuation> {
    // SAFETY: the pointer is only converted for the same registry lookup.
    unsafe { retain_handle(handle.cast_mut()) }
}

pub(crate) fn registered_machine_entry(scope_id: ScopeId, key: usize) -> Option<*mut c_void> {
    machine_entries()
        .lock()
        .expect("continuation machine entry mutex")
        .get(&(scope_id, key))
        .copied()
        .map(|entry| entry as *mut c_void)
}

pub(super) fn register_machine_entry(scope_id: ScopeId, key: usize, entry: *mut c_void) -> u8 {
    if key == 0 || entry.is_null() {
        return 0;
    }
    let mut entries = machine_entries()
        .lock()
        .expect("continuation machine entry mutex");
    match entries.entry((scope_id, key)) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(entry as usize);
            1
        }
        std::collections::hash_map::Entry::Occupied(slot) => {
            u8::from(*slot.get() == entry as usize)
        }
    }
}

pub(super) fn unregister_machine_entry(scope_id: ScopeId, key: usize) {
    machine_entries()
        .lock()
        .expect("continuation machine entry mutex")
        .remove(&(scope_id, key));
}

pub(super) fn unregister_machine_entries_for_scope(scope_id: ScopeId) {
    machine_entries()
        .lock()
        .expect("continuation machine entry mutex")
        .retain(|(entry_scope, _), _| *entry_scope != scope_id);
}

pub(super) unsafe fn register_suspend_provider(
    scope_id: ScopeId,
    operation: u64,
    start: *mut c_void,
    cancel: *mut c_void,
) -> u8 {
    if start.is_null() {
        return 0;
    }
    suspend_providers()
        .lock()
        .expect("suspend provider mutex")
        .entry((scope_id, operation))
        .and_modify(|provider| {
            if provider.start == start as usize && provider.cancel == cancel as usize {
                provider.registrations += 1;
            } else {
                *provider = SuspendProvider {
                    start: start as usize,
                    cancel: cancel as usize,
                    registrations: 1,
                };
            }
        })
        .or_insert(SuspendProvider {
            start: start as usize,
            cancel: cancel as usize,
            registrations: 1,
        });
    1
}

pub(super) unsafe fn unregister_suspend_provider(scope_id: ScopeId, operation: u64) {
    let mut providers = suspend_providers().lock().expect("suspend provider mutex");
    if let Some(provider) = providers.get_mut(&(scope_id, operation)) {
        if provider.registrations > 1 {
            provider.registrations -= 1;
        } else {
            providers.remove(&(scope_id, operation));
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceSnapshot {
    pub handle_capacity: usize,
    pub scope_handles: usize,
    pub task_links: usize,
    pub machine_entries: usize,
    pub providers: usize,
    pub registry_capacity: usize,
    pub suspend_argument_bytes: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn resource_snapshot(scope: Option<ScopeId>) -> ResourceSnapshot {
    let matches = |value: &Continuation| scope.is_none_or(|id| value.scope_id() == id);
    let (capacity, selected, bytes) = {
        let handles = handles().lock().unwrap();
        (
            handles.entries.capacity(),
            handles
                .entries
                .values()
                .filter(|value| matches(value))
                .count(),
            handles
                .entries
                .values()
                .filter(|value| matches(value))
                .map(|value| value.inner.suspend_argument_size.load(Ordering::Acquire))
                .sum(),
        )
    };
    let tasks = task_continuations().lock().unwrap();
    let entries = machine_entries().lock().unwrap();
    let providers = suspend_providers().lock().unwrap();
    ResourceSnapshot {
        handle_capacity: capacity,
        scope_handles: selected,
        task_links: tasks.values().filter(|value| matches(value)).count(),
        machine_entries: entries
            .keys()
            .filter(|(id, _)| scope.is_none_or(|s| *id == s))
            .count(),
        providers: providers
            .keys()
            .filter(|(id, _)| scope.is_none_or(|s| *id == s))
            .count(),
        registry_capacity: tasks.capacity() + entries.capacity() + providers.capacity(),
        suspend_argument_bytes: bytes,
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn describe_scope(scope: Option<ScopeId>) -> Vec<String> {
    let entries: Vec<_> = handles()
        .lock()
        .unwrap()
        .entries
        .iter()
        .filter(|(_, value)| scope.is_none_or(|id| value.scope_id() == id))
        .map(|(id, value)| (*id, value.clone()))
        .collect();
    entries.into_iter().map(|(id, value)| {
        let inner = &value.inner;
        format!("token={id} state={:?} free={} callbacks={} native={} timers={} children={} wait={} call={:?} parent={} pc={} refs={}",
            value.state(), inner.free_requested.load(Ordering::Acquire),
            inner.callbacks_in_flight.load(Ordering::Acquire), inner.native_operation_active.load(Ordering::Acquire),
            inner.timer_active.load(Ordering::Acquire), inner.function_children.lock().unwrap().len(),
            inner.task_wait_group.lock().unwrap().is_some(), inner.function_call.lock().unwrap(),
            inner.function_parent.load(Ordering::Acquire), inner.program_counter.load(Ordering::Acquire), std::sync::Arc::strong_count(inner))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_table_reuses_capacity_without_reusing_tokens() {
        let mut registry = HandleRegistry::default();
        let mut last = 0;
        for _ in 0..256 {
            for _ in 0..32 {
                let token = registry.insert(Continuation::new(1)).unwrap();
                assert!(token > last);
                last = token;
            }
            registry.entries.clear();
            assert!(registry.entries.capacity() <= 128);
        }
        assert_eq!(last, 8192);
    }

    #[test]
    fn handle_namespace_exhaustion_never_wraps() {
        let mut registry = HandleRegistry {
            last: usize::MAX - 1,
            ..Default::default()
        };
        assert_eq!(registry.insert(Continuation::new(1)), Some(usize::MAX));
        assert_eq!(registry.insert(Continuation::new(1)), None);
        assert_eq!(registry.entries.len(), 1);
    }
}
