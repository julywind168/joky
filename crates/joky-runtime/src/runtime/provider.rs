//! Scope-owned provider registration for the standalone runtime.
//!
//! Providers are installed by name: the compiler side derives `(name,
//! operation_id)` entries from the checked effect declarations and the host
//! dispatches them to the provider that owns the effect. This layer owns
//! registration lifetime and shutdown ordering without depending on semantic
//! types or compiler data structures.

use std::ffi::c_void;
use std::sync::Arc;

use super::continuation::{
    register_suspend_provider_for_scope, unregister_suspend_provider_for_scope, Continuation,
};
use super::scope::{RuntimeScope, ScopeId};

type Shutdown = Box<dyn FnOnce() + Send + 'static>;

/// Start hook shared by every provider operation continuation.
/// Owned managed arguments (including native handles) belong to the continuation;
/// borrowed arguments remain with the caller. Providers must not drop either
/// input share. Pending work retains its own shared values or native resources.
pub(crate) type ProviderStart = unsafe extern "C" fn(
    handle: *mut Continuation,
    operation: u64,
    arguments: *const u8,
    size: usize,
    environment: *mut u8,
    result_size: usize,
) -> u8;

/// Cancel hook shared by every provider operation continuation.
pub(crate) type ProviderCancel =
    unsafe extern "C" fn(handle: *mut Continuation, operation: u64) -> u8;

/// One operation the compiler asks a provider to serve. `operation` is the
/// encoded `(effect_id << 32) | operation_index` identity of the call site.
#[derive(Clone, Copy)]
pub struct ProviderOperationEntry<'a> {
    /// Effect the operation belongs to.
    pub effect: &'a str,
    /// Operation name within the effect.
    pub name: &'a str,
    /// Encoded `(effect_id << 32) | operation_index` call-site identity.
    pub operation: u64,
}

/// Register every entry whose name appears in `hooks`; unknown names stay
/// unregistered and fail at their call sites if reached. Returns `None` when
/// nothing matched, mirroring the old empty-mask behaviour.
pub(crate) fn register_named(
    hooks: &[(&str, ProviderStart)],
    cancel: ProviderCancel,
    entries: &[ProviderOperationEntry],
) -> Option<ProviderScope> {
    let scope = super::scope::current_or_default();
    let mut registration = ProviderScope::new(scope);
    for entry in entries {
        if let Some(&(_, start)) = hooks.iter().find(|(name, _)| *name == entry.name) {
            if !registration.register_operation(
                entry.operation,
                start as *mut c_void,
                cancel as *mut c_void,
            ) {
                return None;
            }
        }
    }
    (!registration.is_empty()).then_some(registration)
}

/// Owns provider registrations for one runtime scope.
pub struct ProviderScope {
    scope: Arc<RuntimeScope>,
    scope_id: ScopeId,
    operations: Vec<u64>,
    shutdown: Vec<Shutdown>,
}

impl ProviderScope {
    pub(crate) fn new(scope: Arc<RuntimeScope>) -> Self {
        let scope_id = scope.id();
        Self {
            scope,
            scope_id,
            operations: Vec::new(),
            shutdown: Vec::new(),
        }
    }

    /// Register a native hook for an operation in this scope.
    pub(crate) fn register_operation(
        &mut self,
        operation: u64,
        start: *mut c_void,
        cancel: *mut c_void,
    ) -> bool {
        let accepted =
            unsafe { register_suspend_provider_for_scope(self.scope_id, operation, start, cancel) }
                != 0;
        if accepted {
            self.operations.push(operation);
        }
        accepted
    }

    /// Add provider-specific cleanup that must run before hooks are removed.
    pub(crate) fn on_shutdown(&mut self, cleanup: impl FnOnce() + Send + 'static) {
        self.shutdown.push(Box::new(cleanup));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    pub(crate) fn scope_id(&self) -> ScopeId {
        self.scope_id
    }
}

impl Drop for ProviderScope {
    fn drop(&mut self) {
        self.scope.close_and_wait();
        for cleanup in self.shutdown.drain(..) {
            cleanup();
        }
        for operation in self.operations.drain(..) {
            unsafe {
                unregister_suspend_provider_for_scope(self.scope_id, operation);
            }
        }
    }
}

/// Honor a dynamic local handler before a native provider operation. Input
/// descriptors name shared pointer slots, whose shares remain borrowed by the
/// typed thunk. The resumption token owns the result until completion accepts.
pub(crate) unsafe fn dispatch_handler(
    h: *mut Continuation,
    op: u64,
    arguments: *const u8,
    size: usize,
    result_size: usize,
    shared_offsets: &[usize],
) -> Option<u8> {
    use super::handler::*;
    use super::managed::{jk_drop, jk_dup};
    nearest_operation(op)?;
    let Some(continuation) = Continuation::retain_registered(h) else {
        return Some(0);
    };
    let mut request = if size == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(arguments, size).to_vec()
    };
    let mut slots = Vec::new();
    for &offset in shared_offsets {
        assert!(offset + std::mem::size_of::<usize>() <= size);
        let field = request.as_mut_ptr().add(offset).cast::<*mut u8>();
        field.write_unaligned(jk_dup(field.read_unaligned()));
        slots.push(HandlerEnvSlot {
            offset,
            ownership: HandlerEnvOwnership::Shared as u8,
            drop_callback: jk_drop as *const () as *mut c_void,
        });
    }
    let token = jk_handler_frame_begin_resumption_handle_with_payload_env(
        op,
        request.as_ptr(),
        size,
        slots.as_ptr(),
        slots.len(),
    );
    let mut result = vec![0u8; result_size];
    let succeeded = !token.is_null()
        && jk_handler_frame_dispatch_resumption_handle(token) != 0
        && jk_handler_frame_resumption_payload_size(token) == result_size
        && jk_handler_frame_resumption_payload_copy(token, result.as_mut_ptr(), result_size)
            == result_size;
    if succeeded && continuation.complete_suspend_with_payload(h, op, result.as_ptr(), result_size)
    {
        jk_handler_frame_resumption_payload_consume(token);
    }
    jk_handler_frame_free_resumption_handle(token);
    Some(u8::from(succeeded))
}
