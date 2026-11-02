//! Latched shutdown notification. No process signal is installed by construction.
#[cfg(unix)]
mod signals;
use super::{
    arena::{Arena, ExternalCharge},
    borrowed_host::{STOP_COMPLETED, STOP_FAILED},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    executor::{current_waker, TaskWaker},
    resource::OwnedResource,
};
use crate::{Error, ErrorCode, ErrorPhase, FunctionSignature, Type};
#[cfg(unix)]
pub use signals::Subscription as SignalSubscription;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex, MutexGuard},
};

pub(super) enum Wake {
    Local(TaskWaker),
    Parallel(super::parallel::pool::IoWaker),
}
impl Wake {
    pub(super) fn current() -> Option<Self> {
        super::parallel::pool::io_waker()
            .map(Self::Parallel)
            .or_else(|| current_waker().map(Self::Local))
    }
    pub(super) fn wake(&self) {
        match self {
            Self::Local(w) => w.wake(),
            Self::Parallel(w) => w.wake(),
        }
    }
}
enum Charge {
    Local { _charge: ExternalCharge },
    Shared { _charge: super::parallel::Lease },
}
impl Charge {
    fn new(arena: &mut Arena, bytes: usize) -> Result<Self, Error> {
        if let Some(root) = arena.parallel_root() {
            Ok(Self::Shared {
                _charge: root.accounts().reserve(bytes)?,
            })
        } else {
            Ok(Self::Local {
                _charge: arena.reserve_external(bytes)?,
            })
        }
    }
}
const MAX_WAITERS: usize = 1024;
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}
fn failure(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
/// First observed cause; later requests never replace it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Reason {
    Requested = 1,
    Interrupt = 2,
    Terminate = 3,
}
#[derive(Default)]
struct State {
    reason: Option<Reason>,
    next: u64,
    waiters: BTreeMap<u64, Option<Wake>>,
}
/// Thread-safe host notification. Clones share one latched state.
/// This object never cancels tasks or exits the process by itself.
#[derive(Clone, Default)]
pub struct Shutdown(Arc<Mutex<State>>);
impl Shutdown {
    /// Create an independent notification without installing any signal handlers.
    pub fn new() -> Self {
        Self::default()
    }
    /// Publish the first reason and wake every registered waiter. Returns true once.
    pub fn request(&self, reason: Reason) -> bool {
        let waiters = {
            let mut state = lock(&self.0);
            if state.reason.is_some() {
                return false;
            }
            state.reason = Some(reason);
            std::mem::take(&mut state.waiters)
        };
        for wake in waiters.into_values().flatten() {
            wake.wake();
        }
        true
    }
    /// Read the first reason without consuming it.
    pub fn reason(&self) -> Option<Reason> {
        lock(&self.0).reason
    }
    /// Explicitly subscribe to process-wide SIGINT/SIGTERM. At most eight native
    /// subscriptions coexist. Dropping the subscription stops and joins its thread.
    #[cfg(unix)]
    pub fn subscribe_unix(&self) -> Result<SignalSubscription, SignalError> {
        signals::Subscription::new(self.clone())
    }
}
/// Failure to install a native signal bridge; notification remains usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum SignalError {
    Unsupported = 1,
    Busy = 2,
    Unavailable = 3,
}

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(crate) enum Operation {
    Create,
    Share,
    Request,
    Reason,
    Wait,
    Subscribe,
}
impl Operation {
    pub(crate) fn lookup(name: &str) -> Option<Self> {
        Some(match name {
            "__shutdown_create" => Self::Create,
            "__shutdown_share" => Self::Share,
            "__shutdown_request" => Self::Request,
            "__shutdown_reason" => Self::Reason,
            "__shutdown_wait" => Self::Wait,
            "__shutdown_subscribe" => Self::Subscribe,
            _ => return None,
        })
    }
    pub(crate) fn returns_owner(self) -> bool {
        matches!(self, Self::Create | Self::Share | Self::Subscribe)
    }
    pub(crate) fn validate(
        self,
        signature: &FunctionSignature,
        suspends: bool,
    ) -> Result<(), Error> {
        let owner = Type::Resource("shutdown.ControllerOwner".into());
        let mut params = if matches!(self, Self::Create) {
            vec![]
        } else {
            vec![Type::reference(owner.clone(), false)]
        };
        if matches!(self, Self::Subscribe) {
            params.push(Type::reference(Type::UInt64, true));
        }
        let ret = match self {
            Self::Create | Self::Share => owner,
            Self::Subscribe => Type::Resource("shutdown.SignalsOwner".into()),
            _ => Type::UInt64,
        };
        if !suspends || *signature != FunctionSignature::new(params, ret) {
            return Err(Error::new_with_code(
                ErrorPhase::Abi,
                ErrorCode::AbiError,
                "Shutdown requires its exact compiler-owned signature",
            ));
        }
        Ok(())
    }
}

enum Value {
    Controller(Shutdown),
    #[cfg(unix)]
    Signals(Option<signals::Subscription>),
    #[cfg(not(unix))]
    Signals,
}
struct Owner {
    value: Value,
    _charge: Charge,
}
impl Drop for Owner {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Value::Signals(subscription) = &mut self.value {
            subscription.take();
        }
    }
}
pub(crate) fn transferable_type(name: &str) -> bool {
    name == "shutdown.ControllerOwner"
}
/// # Safety
/// Token is a live resource without an active lease; callback proves its layout.
pub(crate) unsafe fn transferable(
    token: NonNull<c_void>,
    callback: super::resource::ResourceReleaseV1,
) -> bool {
    if !std::ptr::fn_addr_eq(callback, release as super::resource::ResourceReleaseV1) {
        return false;
    }
    let owner = &*token.cast::<Owner>().as_ptr();
    matches!(owner.value, Value::Controller(_)) && matches!(owner._charge, Charge::Shared { .. })
}
unsafe extern "C" fn release(token: *mut c_void) {
    drop(Box::from_raw(token.cast::<Owner>()));
}
fn owner(arena: &mut Arena, value: Value, charge: Charge) -> Result<u64, Error> {
    let token = Box::into_raw(Box::new(Owner {
        value,
        _charge: charge,
    }));
    let resource = match OwnedResource::from_token(NonNull::new(token.cast()).unwrap(), release) {
        Ok(resource) => resource,
        Err(error) => {
            unsafe {
                release(token.cast());
            }
            return Err(error);
        }
    };
    Ok(Buffer::external(arena, resource)?.as_ptr() as usize as u64)
}
// Each retaining owner/request conservatively accounts for the shared state.
fn state_bytes() -> usize {
    std::mem::size_of::<Mutex<State>>() + 2 * std::mem::size_of::<usize>()
}
unsafe fn controller(bits: u64) -> Result<Shutdown, Error> {
    let buffer =
        NonNull::new(bits as usize as *mut Buffer).ok_or_else(|| failure("Null shutdown owner"))?;
    let token = buffer.as_ref().resource_ref()?.control_token(release)?;
    match &(*token.cast::<Owner>()).value {
        Value::Controller(value) => Ok(value.clone()),
        _ => Err(failure("Expected a shutdown controller")),
    }
}
struct Waiter {
    shutdown: Shutdown,
    id: u64,
    _charge: Charge,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        lock(&self.shutdown.0).waiters.remove(&self.id);
    }
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Waiter>()));
    }
}
unsafe fn wait(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    shutdown: Shutdown,
) -> Result<Option<u64>, Error> {
    let mut state = lock(&shutdown.0);
    if let Some(reason) = state.reason {
        return Ok(Some(reason as u64));
    }
    if frame.host_context.is_null() {
        if state.waiters.len() >= MAX_WAITERS {
            return Err(Error::new_with_code(
                ErrorPhase::Runtime,
                ErrorCode::ResourceLimit,
                "Shutdown waiter limit reached",
            ));
        }
        let charge = Charge::new(arena, state_bytes() + std::mem::size_of::<Waiter>() + 512)?;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| failure("Shutdown waiter identities exhausted"))?;
        let id = state.next;
        state.waiters.insert(id, Wake::current());
        frame.host_context = Box::into_raw(Box::new(Waiter {
            shutdown: shutdown.clone(),
            id,
            _charge: charge,
        }))
        .cast();
        frame.host_stop = Some(stop);
    } else {
        let waiter = &*frame.host_context.cast::<Waiter>();
        if let Some(wake) = Wake::current() {
            state.waiters.insert(waiter.id, Some(wake));
        }
    }
    Ok(None)
}
/// # Safety
/// Frame slots match the checked operation signature. Resource references remain
/// live until completion/cancellation, and polling is serialized on the owner thread.
pub(crate) unsafe fn poll(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    op: Operation,
) -> Result<Option<u64>, Error> {
    if matches!(frame.state, DONE | FAILED) {
        return Err(failure("Shutdown operation has terminated"));
    }
    let result = (|| {
        if matches!(op, Operation::Create) {
            let charge = Charge::new(
                arena,
                state_bytes() + std::mem::size_of::<Owner>() + OwnedResource::event_bytes(),
            )?;
            return owner(arena, Value::Controller(Shutdown::new()), charge).map(Some);
        }
        let shutdown = controller(frame.slots[0])?;
        Ok(Some(match op {
            Operation::Create => unreachable!(),
            Operation::Share => {
                let charge = Charge::new(
                    arena,
                    state_bytes() + std::mem::size_of::<Owner>() + OwnedResource::event_bytes(),
                )?;
                owner(arena, Value::Controller(shutdown), charge)?
            }
            Operation::Request => u64::from(shutdown.request(Reason::Requested)),
            Operation::Reason => shutdown.reason().map_or(0, |reason| reason as u64),
            Operation::Wait => return wait(arena, frame, shutdown),
            Operation::Subscribe => {
                // Reserve before installing process-global hooks or creating a worker.
                let charge = Charge::new(
                    arena,
                    state_bytes()
                        + std::mem::size_of::<Owner>()
                        + OwnedResource::event_bytes()
                        + 320 * 1024,
                )?;
                #[cfg(unix)]
                let (value, code) = match shutdown.subscribe_unix() {
                    Ok(subscription) => (Value::Signals(Some(subscription)), 0),
                    Err(error) => (Value::Signals(None), error as u64),
                };
                #[cfg(not(unix))]
                let (value, code) = (Value::Signals, SignalError::Unsupported as u64);
                *(frame.slots[1] as usize as *mut u64) = code;
                owner(arena, value, charge)?
            }
        }))
    })();
    match &result {
        Ok(None) => {}
        Ok(Some(bits)) => {
            frame.stop_host(STOP_COMPLETED);
            frame.result = *bits;
            frame.state = DONE;
        }
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.state = FAILED;
        }
    }
    result
}
/// # Safety
/// Same frame contract as `poll`; kind is a declared Operation discriminant.
#[export_name = "__joky_shutdown_poll"]
pub(crate) unsafe extern "C" fn poll_native(frame: *mut CpsFrame, kind: u32) -> i32 {
    let op = match kind {
        0 => Operation::Create,
        1 => Operation::Share,
        2 => Operation::Request,
        3 => Operation::Reason,
        4 => Operation::Wait,
        5 => Operation::Subscribe,
        _ => {
            super::error::record_error(failure("Invalid shutdown operation"));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll(&mut *super::cps::arena(frame), &mut *frame, op) {
        Ok(Some(_)) => 0,
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_waiters_cancel_independently_and_host_requests_are_latched() {
        let mut arena = Arena::default();
        let state = Shutdown::new();
        let mut first = CpsFrame::new(0);
        let mut second = CpsFrame::new(0);
        unsafe {
            assert!(wait(&mut arena, &mut first, state.clone())
                .unwrap()
                .is_none());
            assert!(wait(&mut arena, &mut second, state.clone())
                .unwrap()
                .is_none());
        }
        assert_eq!(lock(&state.0).waiters.len(), 2);
        first.release();
        assert_eq!(lock(&state.0).waiters.len(), 1);
        let external = state.clone();
        std::thread::spawn(move || {
            assert!(external.request(Reason::Terminate));
            assert!(!external.request(Reason::Interrupt));
        })
        .join()
        .unwrap();
        assert!(lock(&state.0).waiters.is_empty());
        unsafe {
            assert_eq!(
                wait(&mut arena, &mut second, state.clone()).unwrap(),
                Some(3)
            );
        }
        second.release();
        assert_eq!(state.reason(), Some(Reason::Terminate));
        assert_eq!(arena.bytes(), 0);
    }
    #[test]
    fn shutdown_pending_waiters_are_bounded_and_refund_after_drop() {
        let mut arena = Arena::default();
        let state = Shutdown::new();
        let mut frames = Vec::new();
        for _ in 0..MAX_WAITERS {
            let mut frame = CpsFrame::new(0);
            unsafe {
                assert!(wait(&mut arena, &mut frame, state.clone())
                    .unwrap()
                    .is_none());
            }
            frames.push(frame);
        }
        let mut overflow = CpsFrame::new(0);
        assert_eq!(
            unsafe { wait(&mut arena, &mut overflow, state.clone()) }
                .unwrap_err()
                .code(),
            ErrorCode::ResourceLimit
        );
        drop(frames);
        assert!(lock(&state.0).waiters.is_empty());
        assert_eq!(arena.bytes(), 0);
        unsafe {
            assert!(wait(&mut arena, &mut overflow, state.clone())
                .unwrap()
                .is_none());
        }
        drop(overflow);
        assert_eq!(arena.bytes(), 0);
    }
}
