//! Per-program runtime ownership and completion tracking.
//!
//! Executors remain process-wide, but every asynchronous continuation belongs
//! to exactly one scope. A program therefore waits only for work it started.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

pub(crate) type ScopeId = u64;
type CallbackJob = Box<dyn FnOnce() + Send>;

static NEXT_SCOPE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WORK_ID: AtomicU64 = AtomicU64::new(1);
static DEFAULT_SCOPE: OnceLock<Arc<RuntimeScope>> = OnceLock::new();
static DEFAULT_ARGS: OnceLock<Vec<OsString>> = OnceLock::new();

thread_local! {
    static CURRENT_SCOPE: RefCell<Option<Arc<RuntimeScope>>> = const { RefCell::new(None) };
}

pub(crate) struct RuntimeScope {
    pub(crate) region: Arc<super::region::Region>,
    pub(crate) env_snapshot: Arc<EnvSnapshot>,
    pub(crate) resources: Arc<super::resources::Counters>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) machine_resumptions: AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) managed_objects: Arc<AtomicUsize>,
    id: ScopeId,
    code_scope: Option<ScopeId>,
    callback_queue: Option<Mutex<VecDeque<CallbackJob>>>,
    closing: AtomicBool,
    function_failure: AtomicU8,
    main_error: Mutex<Option<String>>,
    pub(crate) root_failure: Mutex<Option<crate::runtime::task::TaskFailure>>,
    state: Mutex<ScopeState>,
    idle: Condvar,
}

/// Inputs captured once for one embedding/run. Callback scopes share this
/// object so nested execution cannot observe a different process snapshot.
pub(crate) struct EnvSnapshot {
    pub(crate) vars: Vec<(OsString, OsString)>,
    pub(crate) args: Vec<OsString>,
}

fn capture_env() -> Arc<EnvSnapshot> {
    Arc::new(EnvSnapshot {
        vars: std::env::vars_os().collect(),
        args: DEFAULT_ARGS
            .get()
            .cloned()
            .unwrap_or_else(|| std::env::args_os().skip(1).collect()),
    })
}

pub(crate) fn set_default_args(args: Vec<OsString>) -> bool {
    DEFAULT_ARGS.set(args).is_ok()
}

struct ScopeState {
    work: HashMap<u64, Arc<dyn Fn() + Send + Sync>>,
}

pub(crate) struct ScopeGuard {
    _region: super::region::Guard,
    previous: Option<Arc<RuntimeScope>>,
}

pub(crate) struct ScopeWork {
    scope: Arc<RuntimeScope>,
    id: u64,
}

impl RuntimeScope {
    #[cfg(test)]
    pub(crate) fn new() -> Arc<Self> {
        Self::new_with_args(Vec::new())
    }

    pub(crate) fn new_with_args(args: Vec<OsString>) -> Arc<Self> {
        Self::with_environment(Arc::new(EnvSnapshot {
            vars: std::env::vars_os().collect(),
            args,
        }))
    }

    fn with_environment(env_snapshot: Arc<EnvSnapshot>) -> Arc<Self> {
        Arc::new(Self {
            region: super::region::Region::root(),
            env_snapshot,
            #[cfg(any(test, feature = "test-support"))]
            machine_resumptions: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            managed_objects: Arc::new(AtomicUsize::new(0)),
            resources: Arc::default(),
            id: NEXT_SCOPE_ID.fetch_add(1, Ordering::Relaxed),
            code_scope: None,
            callback_queue: None,
            closing: AtomicBool::new(false),
            function_failure: AtomicU8::new(0),
            main_error: Mutex::new(None),
            root_failure: Mutex::new(None),
            state: Mutex::new(ScopeState {
                work: HashMap::new(),
            }),
            idle: Condvar::new(),
        })
    }

    pub(crate) fn id(&self) -> ScopeId {
        self.id
    }

    pub(crate) fn code_scope_id(&self) -> ScopeId {
        self.code_scope.unwrap_or(self.id)
    }

    pub(crate) fn for_callback(parent: &Arc<Self>) -> Arc<Self> {
        let mut scope = Self::with_environment(Arc::clone(&parent.env_snapshot));
        let child = Arc::get_mut(&mut scope).expect("new callback scope");
        child.code_scope = Some(parent.code_scope_id());
        child.callback_queue = Some(Mutex::default());
        scope
    }

    /// Foreign callers drive their own continuation queue while waiting. This
    /// remains live even if every ordinary worker is blocked inside C.
    pub(crate) fn enqueue_resume(&self, callback: CallbackJob) {
        if let Some(queue) = &self.callback_queue {
            queue.lock().expect("callback queue").push_back(callback);
            self.idle.notify_all();
        } else {
            super::task::enqueue_scheduler_callback(callback);
        }
    }

    pub(crate) fn help_callback(&self) -> bool {
        let job = self
            .callback_queue
            .as_ref()
            .and_then(|queue| queue.lock().expect("callback queue").pop_front());
        if let Some(job) = job {
            job();
            true
        } else {
            false
        }
    }

    pub(crate) fn default() -> Arc<Self> {
        DEFAULT_SCOPE
            .get_or_init(|| {
                Arc::new(Self {
                    region: super::region::Region::root(),
                    env_snapshot: capture_env(),
                    #[cfg(any(test, feature = "test-support"))]
                    machine_resumptions: AtomicU64::new(0),
                    #[cfg(any(test, feature = "test-support"))]
                    managed_objects: Arc::new(AtomicUsize::new(0)),
                    resources: Arc::default(),
                    id: 0,
                    code_scope: None,
                    callback_queue: None,
                    closing: AtomicBool::new(false),
                    function_failure: AtomicU8::new(0),
                    main_error: Mutex::new(None),
                    root_failure: Mutex::new(None),
                    state: Mutex::new(ScopeState {
                        work: HashMap::new(),
                    }),
                    idle: Condvar::new(),
                })
            })
            .clone()
    }

    pub(crate) fn enter(self: &Arc<Self>) -> ScopeGuard {
        let previous = CURRENT_SCOPE.with(|current| current.replace(Some(Arc::clone(self))));
        ScopeGuard {
            previous,
            _region: super::region::install(self.region.clone()),
        }
    }

    /// Keep this scope alive until the continuation reaches a terminal state.
    /// Closing scopes reject new work, which prevents late provider callbacks
    /// from reviving a run after its JIT code has been released.
    pub(crate) fn begin_work<F>(self: &Arc<Self>, cancel: F) -> Option<ScopeWork>
    where
        F: Fn() + Send + Sync + 'static,
    {
        if self.closing.load(Ordering::Acquire) {
            return None;
        }
        let id = NEXT_WORK_ID.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().expect("runtime scope mutex");
        if self.closing.load(Ordering::Acquire) {
            return None;
        }
        state.work.insert(id, Arc::new(cancel));
        Some(ScopeWork {
            scope: Arc::clone(self),
            id,
        })
    }

    /// Retain native work through scope shutdown. This cannot resume user code;
    /// an admitted job may hand its lease to finalizers after cancellation.
    pub(crate) fn begin_cleanup(self: &Arc<Self>) -> ScopeWork {
        let id = NEXT_WORK_ID.fetch_add(1, Ordering::Relaxed);
        self.state
            .lock()
            .expect("runtime scope mutex")
            .work
            .insert(id, Arc::new(|| {}));
        ScopeWork {
            scope: Arc::clone(self),
            id,
        }
    }

    pub(crate) fn wait_for_idle(&self) {
        let mut state = self.state.lock().expect("runtime scope mutex");
        if self.callback_queue.is_some() {
            loop {
                drop(state);
                let progressed = self.help_callback();
                state = self.state.lock().expect("runtime scope mutex");
                if state.work.is_empty() && !progressed {
                    return;
                }
                if !progressed {
                    state = self
                        .idle
                        .wait_timeout(state, std::time::Duration::from_millis(1))
                        .expect("runtime scope mutex")
                        .0;
                }
            }
        }
        while !state.work.is_empty() {
            state = self.idle.wait(state).expect("runtime scope mutex");
        }
    }

    pub(crate) fn record_function_failure(&self, status: u8) {
        let _ =
            self.function_failure
                .compare_exchange(0, status, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn function_failure(&self) -> u8 {
        self.function_failure.load(Ordering::Acquire)
    }

    pub(crate) fn take_main_error(&self) -> Option<String> {
        self.main_error.lock().expect("main result mutex").take()
    }

    /// Prevent new work, cancel every live continuation, then wait until its
    /// callbacks have relinquished their references to this scope.
    pub(crate) fn close_and_wait(&self) {
        self.closing.store(true, Ordering::Release);
        let callbacks = {
            let state = self.state.lock().expect("runtime scope mutex");
            state.work.values().cloned().collect::<Vec<_>>()
        };
        for cancel in callbacks {
            cancel();
        }
        self.wait_for_idle();
        self.region.close();
    }
}

/// Consume the error String returned by the generated program entry. The
/// host reads the copied message after synchronous and pending work drains.
pub(crate) unsafe extern "C" fn jk_main_result(tag: u32, pointer: *mut u8, length: usize) {
    if tag == 0 {
        return;
    }
    let message = if pointer.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, length) }).into_owned()
    };
    let scope = current_or_default();
    *scope.main_error.lock().expect("main result mutex") = Some(message);
    super::managed::jk_drop(pointer);
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        CURRENT_SCOPE.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

impl Drop for ScopeWork {
    fn drop(&mut self) {
        let mut state = self.scope.state.lock().expect("runtime scope mutex");
        if state.work.remove(&self.id).is_some() && state.work.is_empty() {
            self.scope.idle.notify_all();
        }
    }
}

pub(crate) fn current() -> Option<Arc<RuntimeScope>> {
    CURRENT_SCOPE.with(|current| current.borrow().clone())
}

pub(crate) fn current_or_default() -> Arc<RuntimeScope> {
    current().unwrap_or_else(RuntimeScope::default)
}

pub(crate) fn current_id() -> ScopeId {
    current_or_default().id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn scopes_track_only_their_own_work() {
        let first = RuntimeScope::new();
        let second = RuntimeScope::new();
        let cancelled = Arc::new(AtomicUsize::new(0));
        let first_cancelled = Arc::clone(&cancelled);
        let first_work = first
            .begin_work(move || {
                first_cancelled.fetch_add(1, Ordering::AcqRel);
            })
            .expect("open scope accepts work");
        let second_work = second.begin_work(|| {}).expect("open scope accepts work");
        drop(first_work);
        first.wait_for_idle();
        assert_eq!(cancelled.load(Ordering::Acquire), 0);
        drop(second_work);
        second.wait_for_idle();
    }

    #[test]
    fn callback_scopes_inherit_explicit_environment_snapshot() {
        let parent = RuntimeScope::new_with_args(vec![OsString::from("parent")]);
        let child = RuntimeScope::for_callback(&parent);
        assert_eq!(parent.env_snapshot.args, child.env_snapshot.args);
        assert!(Arc::ptr_eq(&parent.env_snapshot, &child.env_snapshot));
    }
}
