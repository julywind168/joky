//! Retained C callbacks. The C ABI always receives userdata first and returns
//! a final value; a suspended Joky invocation is driven behind that boundary.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use super::abi::FunctionCallStatus;
use super::continuation::*;
use super::managed::{jk_alloc_native_handle, jk_drop};
use super::scope::{RuntimeScope, ScopeId, ScopeWork};

type Invoke = unsafe extern "C" fn(usize, usize, *const u64, *mut u64, *mut Continuation) -> u8;
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);
static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<Callback>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<usize, Arc<Callback>>> {
    REGISTRY.get_or_init(Mutex::default)
}

struct Callback {
    scope: Arc<RuntimeScope>,
    code: usize,
    entry: usize,
    invoke: Invoke,
    fallback: u64,
    result_size: usize,
    failed: AtomicBool,
    state: Mutex<State>,
    idle: Condvar,
}

struct State {
    environment: Option<usize>,
    closing: bool,
    closed: bool,
    invocations: HashMap<ScopeId, Arc<RuntimeScope>>,
    work: Option<ScopeWork>,
}

impl Callback {
    fn close(&self) {
        let scopes = {
            let mut state = self.state.lock().expect("callback state");
            if state.closing {
                while !state.closed {
                    state = self.idle.wait(state).expect("callback state");
                }
                return;
            }
            state.closing = true;
            state.invocations.values().cloned().collect::<Vec<_>>()
        };
        for scope in scopes {
            scope.close_and_wait();
        }
        let mut state = self.state.lock().expect("callback state");
        while !state.invocations.is_empty() {
            drop(state);
            std::thread::yield_now();
            state = self.state.lock().expect("callback state");
            if !state.invocations.is_empty() {
                state = self
                    .idle
                    .wait_timeout(state, Duration::from_millis(1))
                    .expect("callback state")
                    .0;
            }
        }
        let environment = state.environment.take();
        let work = state.work.take();
        drop(state);
        if let Some(environment) = environment {
            jk_drop(environment as *mut u8);
        }
        self.state.lock().expect("callback state").closed = true;
        self.idle.notify_all();
        drop(work);
    }
}

unsafe extern "C" fn drop_callback(payload: *mut u8) {
    let token = unsafe { payload.cast::<usize>().read() };
    let callback = registry()
        .lock()
        .expect("callback registry")
        .get(&token)
        .cloned();
    if let Some(callback) = callback {
        callback.close();
    }
}

/// The caller transfers one closure environment reference. Closed tokens keep
/// only rejection metadata until the owning JIT scope is retired.
pub(crate) unsafe extern "C" fn jk_callback_new(
    code: usize,
    environment: usize,
    entry: usize,
    invoke: usize,
    fallback: u64,
    result_size: usize,
) -> *mut u8 {
    let scope = super::scope::current_or_default();
    let callback = Arc::new(Callback {
        scope: Arc::clone(&scope),
        code,
        entry,
        invoke: unsafe { std::mem::transmute::<usize, Invoke>(invoke) },
        fallback,
        result_size,
        failed: AtomicBool::new(false),
        state: Mutex::new(State {
            environment: Some(environment),
            closing: false,
            closed: false,
            invocations: HashMap::new(),
            work: None,
        }),
        idle: Condvar::new(),
    });
    // Serialize registration with scope cancellation: close cannot observe a
    // registered callback before its code-lifetime work lease is installed.
    let mut state = callback.state.lock().expect("callback state");
    let weak = Arc::downgrade(&callback);
    let work = scope.begin_work(move || {
        if let Some(callback) = weak.upgrade() {
            callback.close();
        }
    });
    if work.is_none() {
        drop(state);
        callback.close();
        return std::ptr::null_mut();
    }
    state.work = work;
    drop(state);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    assert_ne!(token, 0, "callback token space exhausted");
    let payload = jk_alloc_native_handle(
        std::mem::size_of::<usize>(),
        std::mem::align_of::<usize>(),
        Some(drop_callback),
    );
    if payload.is_null() {
        callback.close();
        return payload;
    }
    unsafe { payload.cast::<usize>().write(token) };
    registry()
        .lock()
        .expect("callback registry")
        .insert(token, callback);
    payload
}

pub(crate) unsafe extern "C" fn jk_callback_context(payload: *mut u8) -> usize {
    if payload.is_null() {
        0
    } else {
        unsafe { payload.cast::<usize>().read() }
    }
}

pub(crate) unsafe extern "C" fn jk_callback_function(payload: *mut u8) -> usize {
    let token = unsafe { jk_callback_context(payload) };
    registry()
        .lock()
        .expect("callback registry")
        .get(&token)
        .map_or(0, |callback| callback.entry)
}

pub(crate) unsafe extern "C" fn jk_callback_failed(payload: *mut u8) -> u8 {
    let token = unsafe { jk_callback_context(payload) };
    u8::from(
        registry()
            .lock()
            .expect("callback registry")
            .get(&token)
            .is_none_or(|callback| callback.failed.load(Ordering::Acquire)),
    )
}

#[derive(Default)]
struct Completion {
    result: Mutex<Option<(u8, u64)>>,
    wake: Condvar,
}

unsafe extern "C" fn complete(handle: *mut Continuation) {
    let completion =
        unsafe { &**jk_continuation_frame_pointer(handle).cast::<*const Completion>() };
    let mut value = 0_u64;
    let size = unsafe { jk_continuation_suspend_result_size(handle) };
    let status = unsafe {
        jk_continuation_take_function_pending_result(handle, (&mut value as *mut u64).cast(), size)
    };
    *completion.result.lock().expect("callback completion") = Some((status, value));
    unsafe { jk_continuation_complete(handle) };
    completion.wake.notify_all();
}

struct Invocation {
    callback: Arc<Callback>,
    scope: Arc<RuntimeScope>,
    root: *mut Continuation,
}

impl Drop for Invocation {
    fn drop(&mut self) {
        unsafe { jk_continuation_free(self.root) };
        self.scope.close_and_wait();
        self.callback
            .state
            .lock()
            .expect("callback state")
            .invocations
            .remove(&self.scope.id());
        self.callback.idle.notify_all();
    }
}

/// C-stack pointers remain valid because this entry does not return until all
/// Joky work for this invocation has completed or drained after cancellation.
pub(crate) unsafe extern "C" fn jk_callback_invoke(
    token: usize,
    arguments: *const u64,
    result: *mut u64,
) {
    let callback = registry()
        .lock()
        .expect("callback registry")
        .get(&token)
        .cloned();
    let Some(callback) = callback else {
        unsafe { result.write(0) };
        return;
    };
    unsafe { result.write(callback.fallback) };
    let scope = RuntimeScope::for_callback(&callback.scope);
    let environment = {
        let mut state = callback.state.lock().expect("callback state");
        if state.closing {
            callback.failed.store(true, Ordering::Release);
            return;
        }
        state.invocations.insert(scope.id(), Arc::clone(&scope));
        state.environment.expect("open callback environment")
    };
    let _scope = scope.enter();
    // No borrowed task or handler pointer from the registering thread escapes.
    unsafe {
        super::task::with_task_context(std::ptr::null_mut(), || {
            let completion = Box::<Completion>::default();
            let root = jk_continuation_new(0);
            let invocation = Invocation {
                callback: Arc::clone(&callback),
                scope: Arc::clone(&scope),
                root,
            };
            let frame = jk_continuation_alloc_frame(root, std::mem::size_of::<usize>());
            frame.cast::<*const Completion>().write(&*completion);
            jk_continuation_alloc_suspend_result(root, callback.result_size);
            jk_continuation_set_resume_callback(root, complete as *mut c_void);
            let status = with_callback_pending_boundary(|| {
                if jk_continuation_begin_function_pending(root) == 0 {
                    return FunctionCallStatus::Cancelled;
                }
                let mut inline = 0_u64;
                let status =
                    (callback.invoke)(callback.code, environment, arguments, &mut inline, root);
                match status {
                    0 => {
                        jk_continuation_complete_function_pending(
                            root,
                            (&inline as *const u64).cast(),
                            callback.result_size,
                        );
                    }
                    1 => {}
                    3 => return FunctionCallStatus::Cancelled,
                    _ => return FunctionCallStatus::Failed,
                }
                match jk_continuation_poll_function_pending(root) {
                    0 => FunctionCallStatus::Ready,
                    1 => FunctionCallStatus::Pending,
                    3 => FunctionCallStatus::Cancelled,
                    _ => FunctionCallStatus::Failed,
                }
            });
            let mut final_status = status as u8;
            if status == FunctionCallStatus::Ready {
                final_status = jk_continuation_take_function_pending_result(
                    root,
                    result.cast(),
                    callback.result_size,
                );
            } else if status == FunctionCallStatus::Pending {
                loop {
                    if let Some((status, value)) =
                        *completion.result.lock().expect("callback completion")
                    {
                        final_status = status;
                        if status == 0 {
                            result.write(value);
                        }
                        break;
                    }
                    if jk_continuation_state(root) == ContinuationState::Cancelled as u8
                        || scope.function_failure() != 0
                    {
                        final_status = FunctionCallStatus::Cancelled as u8;
                        break;
                    }
                    if !scope.help_callback() {
                        let guard = completion.result.lock().expect("callback completion");
                        if guard.is_none() {
                            drop(
                                completion
                                    .wake
                                    .wait_timeout(guard, Duration::from_millis(1))
                                    .expect("callback completion"),
                            );
                        }
                    }
                }
            }
            if final_status != 0 {
                callback.failed.store(true, Ordering::Release);
                result.write(callback.fallback);
            }
            drop(invocation);
            drop(completion);
        })
    };
}

pub(crate) fn retire_scope(scope: ScopeId) {
    registry()
        .lock()
        .expect("callback registry")
        .retain(|_, callback| callback.scope.code_scope_id() != scope);
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn registered_count() -> usize {
    registry().lock().expect("callback registry").len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Barrier};

    struct Probe {
        drops: AtomicUsize,
        entered: AtomicBool,
        release: AtomicBool,
    }

    unsafe extern "C" fn drop_environment(payload: *mut u8) {
        let probe = unsafe { payload.cast::<Arc<Probe>>().read() };
        probe.drops.fetch_add(1, Ordering::SeqCst);
    }

    fn environment(probe: &Arc<Probe>) -> usize {
        let payload = jk_alloc_native_handle(
            std::mem::size_of::<Arc<Probe>>(),
            std::mem::align_of::<Arc<Probe>>(),
            Some(drop_environment),
        );
        unsafe { payload.cast::<Arc<Probe>>().write(Arc::clone(probe)) };
        payload as usize
    }

    unsafe extern "C" fn gated_invoke(
        _: usize,
        environment: usize,
        _: *const u64,
        result: *mut u64,
        _: *mut Continuation,
    ) -> u8 {
        let probe = unsafe { &*(environment as *const Arc<Probe>) };
        probe.entered.store(true, Ordering::Release);
        while !probe.release.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        unsafe { result.write(42) };
        0
    }

    fn probe() -> Arc<Probe> {
        Arc::new(Probe {
            drops: AtomicUsize::new(0),
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
        })
    }

    #[test]
    fn close_and_scope_shutdown_drain_native_entry_before_dropping_environment_once() {
        let scope = RuntimeScope::new();
        let _scope = scope.enter();
        let probe = probe();
        let payload = unsafe {
            jk_callback_new(
                0,
                environment(&probe),
                1,
                gated_invoke as *const () as usize,
                99,
                8,
            )
        };
        let token = unsafe { jk_callback_context(payload) };
        let callback = registry().lock().unwrap()[&token].clone();
        let caller = std::thread::spawn(move || {
            let mut result = 0;
            unsafe { jk_callback_invoke(token, std::ptr::null(), &mut result) };
            result
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !probe.entered.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let (done, completed) = mpsc::channel();
        let owner = Arc::clone(&scope);
        let shutdown = std::thread::spawn(move || {
            owner.close_and_wait();
            done.send(()).unwrap();
        });
        let close = std::thread::spawn(move || callback.close());
        assert!(completed.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
        probe.release.store(true, Ordering::Release);
        completed.recv_timeout(Duration::from_secs(5)).unwrap();
        shutdown.join().unwrap();
        close.join().unwrap();
        assert_eq!(caller.join().unwrap(), 99);
        assert_eq!(unsafe { jk_callback_failed(payload) }, 1);
        jk_drop(payload);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        retire_scope(scope.id());
        assert!(!registry().lock().unwrap().contains_key(&token));
    }

    #[test]
    fn registration_racing_shutdown_releases_environment_and_scope_work() {
        for _ in 0..100 {
            let scope = RuntimeScope::new();
            let _scope = scope.enter();
            let probe = probe();
            let barrier = Arc::new(Barrier::new(2));
            let other = Arc::clone(&barrier);
            let owner = Arc::clone(&scope);
            let (done, completed) = mpsc::channel();
            let shutdown = std::thread::spawn(move || {
                other.wait();
                owner.close_and_wait();
                done.send(()).unwrap();
            });
            barrier.wait();
            let payload = unsafe {
                jk_callback_new(
                    0,
                    environment(&probe),
                    1,
                    gated_invoke as *const () as usize,
                    99,
                    8,
                )
            };
            completed.recv_timeout(Duration::from_secs(5)).unwrap();
            shutdown.join().unwrap();
            jk_drop(payload);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
            retire_scope(scope.id());
        }
    }
}
