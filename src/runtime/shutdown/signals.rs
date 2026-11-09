//! Scoped signal delivery, with default termination when no subscription exists.
use super::{lock, Reason, Shutdown, SignalError};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::{
        backend::{PollResult, SignalDelivery, SignalIterator},
        exfiltrator::SignalOnly,
        Handle,
    },
};
use std::{
    io::{self, Read},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

type Delivery = SignalDelivery<UnixStream, SignalOnly>;

struct SignalPipe {
    // Fields drop in declaration order: unregister and drain signal callbacks
    // before releasing the final read descriptor. Handle::close only sets the
    // closed flag and writes a wake byte; it does not unregister callbacks.
    handle: Handle,
    _read: UnixStream,
}
impl SignalPipe {
    fn new() -> io::Result<(Delivery, Self)> {
        let (read, write) = UnixStream::pair()?;
        let keep_read = read.try_clone()?;
        let delivery = SignalDelivery::with_pipe(read, write, SignalOnly, [SIGINT, SIGTERM])?;
        let handle = delivery.handle();
        Ok((
            delivery,
            Self {
                handle,
                _read: keep_read,
            },
        ))
    }
}

fn wait_readable(read: &mut UnixStream) -> io::Result<bool> {
    loop {
        match read.read(&mut [0]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result.map(|count| count != 0),
        }
    }
}

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
    pipe: SignalPipe,
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
        let (delivery, pipe) = SignalPipe::new().map_err(|_| SignalError::Unavailable)?;
        // Publication is serialized with final unsubscription. Signals received
        // after this point are latched by delivery even before the thread starts.
        state.active += 1;
        state.slots += 1;
        state.inactive.store(false, Ordering::SeqCst);
        drop(state);
        let active = Active { delivering: true };
        let thread = std::thread::Builder::new()
            .name("joky-signals".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut signals = SignalIterator::new(delivery);
                loop {
                    match signals.poll_signal(&mut wait_readable) {
                        PollResult::Signal(signal) => {
                            shutdown.request(if signal == SIGINT {
                                Reason::Interrupt
                            } else {
                                Reason::Terminate
                            });
                        }
                        PollResult::Closed => break,
                        PollResult::Pending => continue,
                        PollResult::Err(error) => panic!("Signal pipe read failed: {error}"),
                    }
                }
            })
            .map_err(|_| SignalError::Unavailable)?;
        Ok(Self {
            pipe,
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
        // The worker can observe closed before close() writes its wake byte.
        // Keep a second read descriptor alive across close, join and unregister.
        self.pipe.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Retain the worker slot until join, even if another thread subscribes
        // while this subscription is being disposed.
        drop(active);
    }
}

#[cfg(test)]
mod tests;
