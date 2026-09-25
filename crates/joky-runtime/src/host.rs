//! Opaque host API used to run JIT code in the standalone runtime.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use crate::runtime;

pub use joky_runtime_abi::FunctionCallStatus;
pub use runtime::provider::ProviderOperationEntry;

mod symbols;
pub use symbols::{
    visit_batch_symbols, visit_control_symbols, visit_handler_symbols, visit_jit_symbols,
    visit_task_symbols, visit_value_symbols,
};

#[derive(Clone)]
pub struct RuntimeScope {
    state: Arc<RuntimeScopeState>,
}

struct RuntimeScopeState {
    inner: Arc<runtime::scope::RuntimeScope>,
}

pub struct RuntimeScopeGuard {
    _guard: runtime::scope::ScopeGuard,
    _not_send: PhantomData<Rc<()>>,
}

pub struct ProviderRegistration {
    _registration: runtime::provider::ProviderScope,
}

impl RuntimeScope {
    pub fn new() -> Self {
        Self::new_with_args(Vec::new())
    }

    pub fn new_with_args(args: Vec<std::ffi::OsString>) -> Self {
        Self {
            state: Arc::new(RuntimeScopeState {
                inner: runtime::scope::RuntimeScope::new_with_args(args),
            }),
        }
    }

    pub fn enter(&self) -> RuntimeScopeGuard {
        RuntimeScopeGuard {
            _guard: self.state.inner.enter(),
            _not_send: PhantomData,
        }
    }

    pub fn wait_for_idle(&self) {
        self.state.inner.wait_for_idle();
    }

    pub fn close_and_wait(&self) {
        self.state.close_and_wait();
    }

    /// Zero keys and null addresses are rejected. Re-registering the same
    /// address is idempotent; a conflicting address leaves the entry unchanged.
    ///
    /// # Safety
    /// A non-null entry must implement the continuation machine-entry ABI for
    /// this key and remain executable until this scope has been closed.
    pub unsafe fn register_machine_entry(&self, key: usize, entry: *mut c_void) -> bool {
        runtime::continuation::register_machine_entry_for_scope(self.state.inner.id(), key, entry)
            != 0
    }

    pub fn unregister_machine_entry(&self, key: usize) {
        runtime::continuation::unregister_machine_entry_for_scope(self.state.inner.id(), key);
    }

    /// Invoke a generated Pending-ABI root while the standalone runtime owns
    /// the native pending boundary.
    ///
    /// # Safety
    /// The entry must be a valid `extern "C" fn() -> u8` implementing the root
    /// Pending ABI. Its code and any continuation entries it uses must remain
    /// executable until this scope has been closed and its work has drained.
    pub unsafe fn invoke_pending_entry(&self, entry: *const u8) -> FunctionCallStatus {
        let _guard = self.enter();
        let entry: extern "C" fn() -> u8 = unsafe { std::mem::transmute(entry) };
        runtime::continuation::with_function_pending_boundary(|| match entry() {
            0 => FunctionCallStatus::Ready,
            1 => FunctionCallStatus::Pending,
            3 => FunctionCallStatus::Cancelled,
            _ => FunctionCallStatus::Failed,
        })
    }

    pub fn reset_root_failure(&self) {
        let _guard = self.enter();
        runtime::task::reset_root_failure();
    }

    pub fn take_root_failure_operation(&self) -> Option<u64> {
        let _guard = self.enter();
        runtime::task::take_root_failure_operation()
    }

    pub fn discard_root_failure(&self) {
        let _guard = self.enter();
        runtime::task::reset_root_failure();
    }

    pub fn take_main_error(&self) -> Option<String> {
        self.state.inner.take_main_error()
    }

    pub fn function_failure(&self) -> u8 {
        self.state.inner.function_failure()
    }

    /// Install a native provider by name. This dispatch table is the single
    /// registration point for providers built into the runtime; adding a
    /// provider is one row here plus its implementation.
    pub fn register_provider(
        &self,
        provider: &str,
        operations: &[runtime::provider::ProviderOperationEntry],
    ) -> Option<ProviderRegistration> {
        let _guard = self.enter();
        dispatch_provider(provider, operations).map(ProviderRegistration::new)
    }
}

/// The registry of providers built into this runtime.
fn dispatch_provider(
    provider: &str,
    operations: &[runtime::provider::ProviderOperationEntry],
) -> Option<runtime::provider::ProviderScope> {
    match provider {
        "file" => runtime::file::register_operations(operations),
        "env" => runtime::env::register_operations(operations),
        "random" => runtime::random::register_operations(operations),
        "process" => runtime::process::register_operations(operations),
        "socket" => runtime::socket::register_operations(operations),
        "sqlite" => runtime::sqlite::register_operations(operations),
        _ => None,
    }
}

/// One operation registration entry as it crosses the C ABI.
#[repr(C)]
pub struct ProviderFFIOperation {
    /// Effect name the operation belongs to, NUL-terminated UTF-8.
    pub effect: *const std::ffi::c_char,
    /// Operation name within the effect, NUL-terminated UTF-8.
    pub name: *const std::ffi::c_char,
    /// Encoded `(effect_id << 32) | operation_index` call-site identity.
    pub operation: u64,
}

/// C entry point for AOT launchers: install a built-in provider by name.
/// The returned handle must be released with [`jk_provider_unregister`].
#[no_mangle]
pub unsafe extern "C" fn jk_provider_register(
    provider: *const std::ffi::c_char,
    operations: *const ProviderFFIOperation,
    count: usize,
) -> *mut runtime::provider::ProviderScope {
    if provider.is_null() || (count != 0 && operations.is_null()) {
        return std::ptr::null_mut();
    }
    let Ok(provider) = (unsafe { std::ffi::CStr::from_ptr(provider) }).to_str() else {
        return std::ptr::null_mut();
    };
    let entries = unsafe { std::slice::from_raw_parts(operations, count) };
    let mut converted = Vec::with_capacity(count);
    for entry in entries {
        if entry.effect.is_null() || entry.name.is_null() {
            return std::ptr::null_mut();
        }
        let (effect, name) = unsafe {
            (
                std::ffi::CStr::from_ptr(entry.effect).to_str(),
                std::ffi::CStr::from_ptr(entry.name).to_str(),
            )
        };
        let (Ok(effect), Ok(name)) = (effect, name) else {
            return std::ptr::null_mut();
        };
        converted.push(runtime::provider::ProviderOperationEntry {
            effect,
            name,
            operation: entry.operation,
        });
    }
    // AOT launchers own the root scope; current_or_default resolves to it.
    dispatch_provider(provider, &converted)
        .map(Box::new)
        .map_or(std::ptr::null_mut(), Box::into_raw)
}

/// Release a handle returned by [`jk_provider_register`].
#[no_mangle]
pub unsafe extern "C" fn jk_provider_unregister(
    registration: *mut runtime::provider::ProviderScope,
) {
    if !registration.is_null() {
        drop(unsafe { Box::from_raw(registration) });
    }
}

impl Default for RuntimeScope {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeScopeState {
    fn close_and_wait(&self) {
        self.inner.close_and_wait();
        let scope_id = self.inner.id();
        runtime::continuation::unregister_machine_entries_for_scope(scope_id);
        runtime::callback::retire_scope(scope_id);
    }
}

impl Drop for RuntimeScopeState {
    fn drop(&mut self) {
        self.close_and_wait();
        self.inner
            .root_failure
            .lock()
            .expect("root failure mutex")
            .take();
    }
}

impl ProviderRegistration {
    fn new(registration: runtime::provider::ProviderScope) -> Self {
        Self {
            _registration: registration,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use std::ffi::c_void;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{runtime, FunctionCallStatus, RuntimeScope};

    pub fn report_resources(scope: &RuntimeScope, phase: &str) {
        report_inner(&scope.state.inner, phase);
    }

    /// Describe every live managed payload, for leak triage in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn describe_live_managed_objects() -> Vec<String> {
        runtime::managed::live_payload_report()
    }

    fn report_inner(scope: &runtime::scope::RuntimeScope, phase: &str) {
        if std::env::var_os("JOKY_TEST_RESOURCE_REPORT").is_none() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // CLI parity runs own the process, including callback invocation scopes.
            let managed = runtime::managed::live_object_count_all_threads();
            let resources = scope.resources.snapshot();
            let continuation = runtime::continuation::resource_snapshot(None);
            let blocking = runtime::blocking::resource_snapshot(None);
            let reactor = runtime::reactor::resource_snapshot(None);
            let sockets = runtime::socket::resource_snapshot(None);
            let counts = [
                managed,
                resources[0],
                resources[1],
                resources[2],
                resources[3],
                resources[4],
                resources[5],
                continuation.scope_handles,
                continuation.task_links,
                continuation.machine_entries,
                continuation.providers,
                continuation.suspend_argument_bytes,
                blocking.scope_ready,
                blocking.scope_waiting,
                reactor.scope_requests,
                sockets.0,
                sockets.1,
                runtime::callback::registered_count(),
            ];
            if phase == "startup" || counts == [0; 18] || Instant::now() >= deadline {
                eprintln!("[resources] {phase}: {counts:?}");
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[no_mangle]
    pub extern "C" fn jk_test_aot_resources(finish: u8) {
        let scope = runtime::scope::current_or_default();
        report_inner(&scope, if finish == 0 { "startup" } else { "drained" });
    }

    /// Opaque handle for exercising the continuation ABI from host tests.
    pub struct ContinuationHandle(*mut runtime::continuation::Continuation);

    impl ContinuationHandle {
        pub fn new(scope: &RuntimeScope, generation: u64) -> Self {
            let _guard = scope.enter();
            Self(runtime::continuation::jk_continuation_new(generation))
        }

        /// Borrow a continuation handle supplied to a runtime callback.
        ///
        /// # Safety
        ///
        /// `handle` must remain registered for the lifetime of this value.
        pub unsafe fn from_raw(handle: *mut c_void) -> Self {
            Self(handle.cast())
        }

        pub fn as_raw(&self) -> *mut c_void {
            self.0.cast()
        }

        pub fn allocate_frame(&self, size: usize) -> *mut u8 {
            unsafe { runtime::continuation::jk_continuation_alloc_frame(self.0, size) }
        }

        pub fn frame_pointer(&self) -> *mut u8 {
            unsafe { runtime::continuation::jk_continuation_frame_pointer(self.0) }
        }

        pub fn allocate_result(&self, size: usize) -> *mut u8 {
            unsafe { runtime::continuation::jk_continuation_alloc_result(self.0, size) }
        }

        pub fn allocate_suspend_result(&self, size: usize) -> *mut u8 {
            unsafe { runtime::continuation::jk_continuation_alloc_suspend_result(self.0, size) }
        }

        /// Install a callback using the continuation resume ABI.
        ///
        /// # Safety
        ///
        /// `callback` must remain executable until the continuation terminates.
        pub unsafe fn set_resume_callback(&self, callback: *mut c_void) -> bool {
            unsafe {
                runtime::continuation::jk_continuation_set_resume_callback(self.0, callback) != 0
            }
        }

        pub fn begin_function_pending(&self) -> bool {
            unsafe { runtime::continuation::jk_continuation_begin_function_pending(self.0) != 0 }
        }

        pub fn poll_function_pending(&self) -> FunctionCallStatus {
            match unsafe { runtime::continuation::jk_continuation_poll_function_pending(self.0) } {
                0 => FunctionCallStatus::Ready,
                1 => FunctionCallStatus::Pending,
                3 => FunctionCallStatus::Cancelled,
                _ => FunctionCallStatus::Failed,
            }
        }

        /// Move a ready function result into `output`.
        ///
        /// # Safety
        ///
        /// `output` must address `size` writable bytes for the function result.
        pub unsafe fn take_function_pending_result(
            &self,
            output: *mut u8,
            size: usize,
        ) -> FunctionCallStatus {
            match unsafe {
                runtime::continuation::jk_continuation_take_function_pending_result(
                    self.0, output, size,
                )
            } {
                0 => FunctionCallStatus::Ready,
                1 => FunctionCallStatus::Pending,
                3 => FunctionCallStatus::Cancelled,
                _ => FunctionCallStatus::Failed,
            }
        }

        pub fn complete(&self) -> bool {
            unsafe { runtime::continuation::jk_continuation_complete(self.0) != 0 }
        }

        pub fn complete_suspend(&self, operation: u64) -> bool {
            unsafe {
                runtime::continuation::jk_continuation_complete_suspend(self.0, operation) != 0
            }
        }

        pub fn free(self) {
            unsafe { runtime::continuation::jk_continuation_free(self.0) };
        }
    }

    /// Keeps one raw provider hook installed until its runtime scope is drained.
    pub struct SuspendProviderRegistration {
        scope: RuntimeScope,
        operation: u64,
    }

    impl Drop for SuspendProviderRegistration {
        fn drop(&mut self) {
            self.scope.close_and_wait();
            unsafe {
                runtime::continuation::unregister_suspend_provider_for_scope(
                    self.scope.state.inner.id(),
                    self.operation,
                );
            }
        }
    }

    /// Return the number of generated machine entries dispatched in this scope.
    pub fn machine_resumptions(scope: &RuntimeScope) -> u64 {
        scope
            .state
            .inner
            .machine_resumptions
            .load(Ordering::Acquire)
    }

    /// Return the number of managed objects owned by this runtime scope.
    pub fn managed_objects(scope: &RuntimeScope) -> usize {
        scope.state.inner.managed_objects.load(Ordering::Acquire)
    }

    /// Return managed objects across the process for isolated runtime tests.
    pub fn process_managed_objects() -> usize {
        runtime::managed::live_object_count_all_threads()
    }

    /// Return scope-owned runtime resources by stable internal category.
    pub fn resource_counts(scope: &RuntimeScope) -> [usize; 6] {
        scope.state.inner.resources.snapshot()
    }

    /// Return the number of host handles retaining this scope state.
    pub fn scope_strong_count(scope: &RuntimeScope) -> usize {
        Arc::strong_count(&scope.state)
    }

    /// Reset process-wide heap task metrics for an isolated compiler stress test.
    pub fn reset_heap_task_metrics() {
        runtime::task::reset_heap_task_metrics();
    }

    /// Return `(spawned, peak retained, currently retained)` heap task storage.
    pub fn heap_task_metrics() -> (usize, usize, usize) {
        runtime::task::heap_task_metrics()
    }

    /// Exercise reusable continuation and reactor registries in this scope.
    pub fn churn_runtime_metadata(scope: &RuntimeScope) {
        let _guard = scope.enter();
        let scope_id = scope.state.inner.id();
        for _ in 0..32 {
            let mut handles = Vec::with_capacity(32);
            let mut timers = Vec::with_capacity(32);
            for _ in 0..32 {
                let handle = runtime::continuation::jk_continuation_new(1);
                assert!(!unsafe {
                    runtime::continuation::jk_continuation_alloc_frame(handle, 128)
                }
                .is_null());
                handles.push(handle);
                timers.push(runtime::reactor::register_timer_with_cancel(
                    Instant::now() + Duration::from_secs(3600),
                    Box::new(|| {}),
                    Some(Box::new(|| {})),
                ));
            }
            assert_eq!(
                runtime::continuation::resource_snapshot(Some(scope_id)).scope_handles,
                32
            );
            for handle in handles {
                unsafe { runtime::continuation::jk_continuation_free(handle) };
                assert_eq!(
                    unsafe { runtime::continuation::jk_continuation_cancel(handle) },
                    0
                );
            }
            for id in timers {
                runtime::reactor::cancel_timer(id);
                runtime::reactor::cancel_timer(id);
                runtime::reactor::cancel_io(id);
            }
            wait_for_runtime_metadata(scope);
        }
    }

    /// Return retained capacity across process-wide standalone registries.
    pub fn runtime_metadata_capacity() -> usize {
        let continuation = runtime::continuation::resource_snapshot(None);
        continuation.handle_capacity
            + continuation.registry_capacity
            + runtime::reactor::resource_snapshot(None).request_capacity
            + runtime::blocking::resource_snapshot(None).capacity
            + runtime::socket::resource_snapshot(None).2
    }

    /// Describe live continuations owned by this scope for failure diagnostics.
    pub fn describe_continuations(scope: &RuntimeScope) -> Vec<String> {
        runtime::continuation::describe_scope(Some(scope.state.inner.id()))
    }

    fn wait_for_runtime_metadata(scope: &RuntimeScope) {
        let scope_id = scope.state.inner.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let continuation = runtime::continuation::resource_snapshot(Some(scope_id));
            let blocking = runtime::blocking::resource_snapshot(Some(scope_id));
            let reactor = runtime::reactor::resource_snapshot(Some(scope_id));
            let sockets = runtime::socket::resource_snapshot(Some(scope_id));
            let clean = resource_counts(scope) == [0; 6]
                && managed_objects(scope) == 0
                && continuation.scope_handles == 0
                && continuation.task_links == 0
                && continuation.machine_entries == 0
                && continuation.providers == 0
                && continuation.suspend_argument_bytes == 0
                && blocking.scope_ready == 0
                && blocking.scope_waiting == 0
                && reactor.scope_requests == 0
                && sockets.0 == 0
                && sockets.1 == 0;
            if clean {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "standalone metadata did not drain: {continuation:?}, {blocking:?}, {reactor:?}, {sockets:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Record an unclaimed root failure for backend lifetime tests.
    ///
    /// # Safety
    ///
    /// `payload` and `drop_payload` must satisfy the task abort payload ABI.
    pub unsafe fn record_root_failure(
        scope: &RuntimeScope,
        operation: u64,
        payload: *const c_void,
        payload_size: usize,
        drop_payload: *const c_void,
    ) {
        let _guard = scope.enter();
        unsafe {
            runtime::task::jk_task_abort_payload(operation, payload, payload_size, drop_payload)
        };
    }

    pub fn has_root_failure(scope: &RuntimeScope) -> bool {
        scope
            .state
            .inner
            .root_failure
            .lock()
            .expect("root failure mutex")
            .is_some()
    }

    /// Publish pending activations only after the supplied native entry returns.
    pub fn with_function_pending_boundary(
        scope: &RuntimeScope,
        invoke: impl FnOnce() -> FunctionCallStatus,
    ) -> FunctionCallStatus {
        let _guard = scope.enter();
        runtime::continuation::with_function_pending_boundary(invoke)
    }

    /// Register a raw suspending provider hook for a host runtime test.
    ///
    /// # Safety
    ///
    /// `start` and `cancel` must use the runtime provider ABI and remain valid
    /// until the returned registration is dropped.
    pub unsafe fn register_suspend_provider(
        scope: &RuntimeScope,
        operation: u64,
        start: *mut c_void,
        cancel: *mut c_void,
    ) -> Option<SuspendProviderRegistration> {
        let accepted = unsafe {
            runtime::continuation::register_suspend_provider_for_scope(
                scope.state.inner.id(),
                operation,
                start,
                cancel,
            )
        } != 0;
        accepted.then(|| SuspendProviderRegistration {
            scope: scope.clone(),
            operation,
        })
    }

    /// Complete a raw provider request and copy its result payload.
    ///
    /// # Safety
    ///
    /// `continuation` must be a live handle supplied to a registered provider,
    /// and `payload` must address `payload_size` readable bytes.
    pub unsafe fn complete_suspend_with_payload(
        continuation: *mut c_void,
        operation: u64,
        payload: *const u8,
        payload_size: usize,
    ) -> u8 {
        unsafe {
            runtime::continuation::jk_continuation_complete_suspend_with_payload(
                continuation.cast(),
                operation,
                payload,
                payload_size,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn machine_entry() {}

    #[test]
    fn scope_owns_current_context_and_machine_entries() {
        let scope = RuntimeScope::new();
        let id = scope.state.inner.id();
        {
            let _guard = scope.enter();
            assert_eq!(runtime::scope::current_id(), id);
        }
        assert_ne!(runtime::scope::current_id(), id);

        let entry = machine_entry as *const () as *mut c_void;
        assert!(unsafe { scope.register_machine_entry(7, entry) });
        assert_eq!(
            runtime::continuation::registries::registered_machine_entry(id, 7),
            Some(entry)
        );
        scope.close_and_wait();
        assert_eq!(
            runtime::continuation::registries::registered_machine_entry(id, 7),
            None
        );
    }

    #[test]
    fn cloned_scope_keeps_host_lifecycle_alive() {
        let scope = RuntimeScope::new();
        let clone = scope.clone();
        let id = scope.state.inner.id();
        drop(scope);
        let _guard = clone.enter();
        assert_eq!(runtime::scope::current_id(), id);
    }

    #[test]
    fn scope_tracks_managed_object_ownership() {
        let scope = RuntimeScope::new();
        let _guard = scope.enter();
        let object = runtime::managed::jk_alloc_object(
            runtime::managed::RuntimeValueKind::Class as u8,
            8,
            8,
        );
        assert!(!object.is_null());
        assert_eq!(testing::managed_objects(&scope), 1);
        runtime::managed::jk_drop(object);
        assert_eq!(testing::managed_objects(&scope), 0);
    }
}
