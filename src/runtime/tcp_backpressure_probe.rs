//! Thread-scoped configuration and observation for real socket backpressure tests.
use std::cell::Cell;

#[path = "../../tests/support/socket_buffer.rs"]
mod socket_buffer;

thread_local! {
    static BLOCKED: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(crate) struct Probe;
impl Probe {
    pub(crate) fn new() -> Self {
        BLOCKED.with(|state| {
            assert!(state.get().is_none(), "nested TCP backpressure probe");
            state.set(Some(0));
        });
        Self
    }
    pub(crate) fn blocked_writes(&self) -> usize {
        BLOCKED.with(|state| state.get().unwrap())
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        BLOCKED.with(|state| state.set(None));
    }
}
pub(super) fn configure(stream: &mut mio::net::TcpStream) {
    if BLOCKED.with(|state| state.get().is_some()) {
        socket_buffer::set(stream, libc::SO_SNDBUF, 32768);
    }
}
pub(super) fn blocked() {
    BLOCKED.with(|state| {
        if let Some(count) = state.get() {
            state.set(Some(count + 1));
        }
    });
}
