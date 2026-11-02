//! Scoped signal delivery, with default termination when no subscription exists.
use super::{lock, Reason, Shutdown, SignalError};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};

struct Registry {
    active: usize,
    slots: usize,
    inactive: Arc<AtomicBool>,
}
static REGISTRY: OnceLock<Result<Mutex<Registry>, SignalError>> = OnceLock::new();
fn registry() -> Result<&'static Mutex<Registry>, SignalError> {
    REGISTRY
        .get_or_init(|| {
            let inactive = Arc::new(AtomicBool::new(true));
            // signal-hook intentionally leaves its multiplexer installed after
            // unregister. Keep two bounded fallback actions so dropping the final
            // subscription does not silently swallow later termination signals.
            for signal in [SIGINT, SIGTERM] {
                signal_hook::flag::register_conditional_default(signal, inactive.clone())
                    .map_err(|_| SignalError::Unavailable)?;
            }
            Ok(Mutex::new(Registry {
                active: 0,
                slots: 0,
                inactive,
            }))
        })
        .as_ref()
        .map_err(|error| *error)
}
struct Active {
    delivering: bool,
}
impl Active {
    fn deactivate(&mut self) {
        if self.delivering {
            let mut state = lock(registry().expect("registered signal bridge"));
            state.active -= 1;
            if state.active == 0 {
                state.inactive.store(true, Ordering::SeqCst);
            }
            self.delivering = false;
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.deactivate();
        lock(registry().expect("registered signal bridge")).slots -= 1;
    }
}
/// Owns a bounded native signal thread. Signals target the whole process;
/// registration is explicit, and existing handlers may also run.
/// After the last subscription is dropped, SIGINT/SIGTERM have default
/// termination behavior. The signal-hook multiplexer is not uninstalled.
pub struct Subscription {
    handle: Handle,
    thread: Option<std::thread::JoinHandle<()>>,
    active: Option<Active>,
}
impl Subscription {
    pub(super) fn new(shutdown: Shutdown) -> Result<Self, SignalError> {
        let registry = registry()?;
        let mut state = lock(registry);
        if state.slots >= 8 {
            return Err(SignalError::Busy);
        }
        let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(|_| SignalError::Unavailable)?;
        let handle = signals.handle();
        // Publication is serialized with final unsubscription. Signals received
        // after this point are latched by Signals even before the thread starts.
        state.active += 1;
        state.slots += 1;
        state.inactive.store(false, Ordering::SeqCst);
        drop(state);
        let active = Active { delivering: true };
        let thread = std::thread::Builder::new()
            .name("joky-signals".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                for signal in &mut signals {
                    shutdown.request(if signal == SIGINT {
                        Reason::Interrupt
                    } else {
                        Reason::Terminate
                    });
                }
            })
            .map_err(|_| SignalError::Unavailable)?;
        Ok(Self {
            handle,
            thread: Some(thread),
            active: Some(active),
        })
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        // Establish default handling before removing the final delivery action.
        let mut active = self.active.take();
        if let Some(active) = &mut active {
            active.deactivate();
        }
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Retain the worker slot until join, even if another thread subscribes
        // while this subscription is being disposed.
        drop(active);
    }
}
