//! Versioned external resource ownership, independent of language containers.
use super::{arena::Arena, buffer::Buffer};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{ffi::c_void, ptr::NonNull};

pub type ResourceCreateV1 = unsafe extern "C" fn(u64, *mut *mut c_void) -> i32;
pub type ResourceReleaseV1 = unsafe extern "C" fn(*mut c_void);

/// Synchronous factory for a registered resource type (UInt64 -> Resource).
/// Callbacks run on the task thread and must not unwind. Every nonnull output
/// transfers exactly one release obligation, including on failure. Successful
/// calls return zero and a nonnull token. release must be infallible and must not
/// access Joky storage. The descriptor and callback code outlive all resources.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResourceFactoryV1 {
    pub abi_version: u32,
    pub create: Option<ResourceCreateV1>,
    pub release: Option<ResourceReleaseV1>,
}
impl ResourceFactoryV1 {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.abi_version != 1 || self.create.is_none() || self.release.is_none() {
            return Err(error("Invalid resource factory v1 descriptor"));
        }
        Ok(())
    }
}
pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.starts_with("__joky_")
        || name.is_empty()
        || !name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    {
        return Err(error("Invalid resource factory name"));
    }
    Ok(())
}
fn error(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
pub(crate) struct OwnedResource {
    token: NonNull<c_void>,
    release: ResourceReleaseV1,
    event: ResourceEvent,
}
impl Drop for OwnedResource {
    fn drop(&mut self) {
        self.event.invalidate();
        // SAFETY: Adoption owns exactly one callback obligation; no copies escape.
        unsafe {
            (self.release)(self.token.as_ptr());
        }
    }
}
pub(crate) fn symbol(name: &str) -> String {
    format!("__joky_resource_factory_v1_{name}")
}

/// # Safety
/// Factory callbacks obey ResourceFactoryV1 and arena is exclusively live.
pub(crate) unsafe fn invoke(
    arena: &mut Arena,
    factory: &ResourceFactoryV1,
    input: u64,
) -> Result<NonNull<Buffer>, Error> {
    factory.validate()?;
    if super::parallel::pool::active() && super::parallel::control::vm_factory(factory) {
        return super::parallel::control::create_vm(arena, input);
    }
    let event = ResourceEvent::new()?;
    let mut token = std::ptr::null_mut();
    let result = super::error::checked(|| factory.create.unwrap()(input, &mut token));
    let resource = NonNull::new(token).map(|token| OwnedResource {
        token,
        release: factory.release.unwrap(),
        event,
    });
    if result? != 0 {
        return Err(error("Resource factory failed"));
    }
    let resource = resource.ok_or_else(|| error("Resource factory returned a null owner"))?;
    Buffer::external(arena, resource)
}

#[export_name = "__joky_resource_create_v1"]
unsafe extern "C" fn create(
    arena: *mut Arena,
    factory: *const ResourceFactoryV1,
    input: u64,
) -> *mut Buffer {
    match invoke(&mut *arena, &*factory, input) {
        Ok(owner) => owner.as_ptr(),
        Err(error) => {
            super::error::record_error(error);
            std::ptr::null_mut()
        }
    }
}
pub(crate) fn symbols() -> [(&'static str, *const u8); 2] {
    [
        ("__joky_resource_create_v1", create as *const u8),
        ("__joky_resource_poll_v1", poll_native as *const u8),
    ]
}

/// A creation operation that may wait. Poll uses the input in frame slot zero.
/// Pending (1) must leave output null and retain any acquired resource in its
/// operation context. Completed (0) transfers a nonnull token; every nonnull
/// output transfers one release obligation even when status or runtime errors
/// indicate failure. Stop revokes all access before the resource is adopted.
/// Callbacks must not unwind, retain frame addresses, or reenter the operation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResourceFactoryCpsV1 {
    pub abi_version: u32,
    pub poll: Option<unsafe extern "C" fn(*mut super::cps::CpsFrame, *mut *mut c_void) -> i32>,
    pub stop: Option<super::borrowed_host::BorrowingHostStop>,
    pub release: Option<ResourceReleaseV1>,
}
impl ResourceFactoryCpsV1 {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.abi_version != 1
            || self.poll.is_none()
            || self.stop.is_none()
            || self.release.is_none()
        {
            return Err(error("Invalid asynchronous resource factory v1 descriptor"));
        }
        Ok(())
    }
}
pub(crate) fn cps_symbol(name: &str) -> String {
    format!("__joky_resource_factory_cps_v1_{name}")
}
/// No instruction-budget or executor cancellation point occurs inside this call.
/// # Safety
/// Frame and arena are exclusive, callbacks obey ResourceFactoryCpsV1, and the
/// same descriptor is used until this operation terminates or is cancelled.
pub(crate) unsafe fn poll_factory(
    arena: &mut Arena,
    frame: &mut super::cps::CpsFrame,
    factory: &ResourceFactoryCpsV1,
) -> Result<Option<NonNull<Buffer>>, Error> {
    use super::{
        borrowed_host::{STOP_COMPLETED, STOP_FAILED},
        cps::{DONE, FAILED},
    };
    if matches!(frame.state, DONE | FAILED) {
        return Err(error("Resource operation has already terminated"));
    }
    let outcome = (|| {
        factory.validate()?;
        // Arm before partial startup so cancellation/failure always calls stop.
        if frame.host_stop.is_none() {
            frame.host_stop = factory.stop;
        }
        let event = ResourceEvent::new()?;
        let mut token = std::ptr::null_mut();
        let status = super::error::checked(|| factory.poll.unwrap()(frame, &mut token));
        let resource = NonNull::new(token).map(|token| OwnedResource {
            token,
            release: factory.release.unwrap(),
            event,
        });
        if matches!(status, Ok(1)) && resource.is_none() && !matches!(frame.state, DONE | FAILED) {
            return Ok(None);
        }
        // Stop must precede release even for malformed or failed output: workers
        // may still be relinquishing their final access to the transferred token.
        frame.stop_host(if matches!(status, Ok(0)) && resource.is_some() {
            STOP_COMPLETED
        } else {
            STOP_FAILED
        });
        match status? {
            0 => {
                let resource = resource
                    .ok_or_else(|| error("Resource operation completed without an owner"))?;
                Buffer::external(arena, resource).map(Some)
            }
            _ => Err(error(
                "Resource operation failed or violated its output protocol",
            )),
        }
    })();
    match &outcome {
        Ok(Some(_)) => {
            frame.state = DONE;
        }
        Ok(None) => {}
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.state = FAILED;
        }
    }
    outcome
}
#[export_name = "__joky_resource_poll_v1"]
unsafe extern "C" fn poll_native(
    frame: *mut super::cps::CpsFrame,
    factory: *const ResourceFactoryCpsV1,
) -> i32 {
    let arena = super::cps::arena(frame);
    match poll_factory(&mut *arena, &mut *frame, &*factory) {
        Ok(Some(owner)) => {
            (*frame).result = owner.as_ptr() as usize as u64;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}

/// A host event capability. Copies can test liveness but never own or recover a
/// resource. Identifiers never wrap or reuse, including across VM instances.
#[derive(Clone)]
pub struct ResourceEvent(std::sync::Arc<EventState>);
struct EventState {
    id: u64,
    active: std::sync::atomic::AtomicBool,
    busy: std::sync::atomic::AtomicBool,
}
static NEXT_EVENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
impl ResourceEvent {
    pub(crate) fn new() -> Result<Self, Error> {
        Self::with_counter(&NEXT_EVENT)
    }
    fn with_counter(counter: &std::sync::atomic::AtomicU64) -> Result<Self, Error> {
        use std::sync::atomic::Ordering;
        let id = counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| error("Resource event identities exhausted"))?;
        Ok(Self(std::sync::Arc::new(EventState {
            id,
            active: std::sync::atomic::AtomicBool::new(true),
            busy: std::sync::atomic::AtomicBool::new(false),
        })))
    }
    pub fn identity(&self) -> u64 {
        self.0.id
    }
    pub fn is_active(&self) -> bool {
        self.0.active.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn invalidate(&self) {
        self.0
            .active
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Unique operation permission. Its lifetime includes all Pending polls and stop.
pub(crate) struct ResourceLease {
    token: NonNull<c_void>,
    event: ResourceEvent,
}
impl ResourceLease {
    pub(crate) fn token(&self) -> *mut c_void {
        self.token.as_ptr()
    }
}
impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.event
            .0
            .busy
            .store(false, std::sync::atomic::Ordering::Release);
    }
}
impl OwnedResource {
    pub(crate) fn from_token(
        token: NonNull<c_void>,
        release: ResourceReleaseV1,
    ) -> Result<Self, Error> {
        Ok(Self {
            token,
            release,
            event: ResourceEvent::new()?,
        })
    }
    pub(crate) fn control_token(&self, release: ResourceReleaseV1) -> Result<*mut c_void, Error> {
        if !self.event.is_active() || !std::ptr::fn_addr_eq(self.release, release) {
            return Err(error("Resource is not a live control proxy"));
        }
        Ok(self.token.as_ptr())
    }
    pub(crate) fn parallel_safe(&self) -> bool {
        self.event.is_active()
            && !self.event.0.busy.load(std::sync::atomic::Ordering::Acquire)
            // The private built-in callback proves the token layout before it
            // is inspected. Generic user factories never acquire this evidence.
            && unsafe { super::tcp::transferable(self.token, self.release)
                || super::parallel::control::transferable(self.token, self.release) }
    }
    pub(crate) fn lease(&self) -> Result<ResourceLease, Error> {
        use std::sync::atomic::Ordering;
        if !self.event.is_active()
            || self
                .event
                .0
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(error(
                "Resource already has an active operation or has closed",
            ));
        }
        Ok(ResourceLease {
            token: self.token,
            event: self.event.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    #[test]
    fn resource_replacement_releases_old_owner_and_transfers_new_identity() {
        use std::{cell::Cell, rc::Rc};
        unsafe extern "C" fn count_release(token: *mut c_void) {
            let count = Box::from_raw(token.cast::<Rc<Cell<usize>>>());
            count.set(count.get() + 1);
        }
        let count = Rc::new(Cell::new(0));
        let old_event = ResourceEvent::new().unwrap();
        let new_event = ResourceEvent::new().unwrap();
        let mut arena = Arena::default();
        let mut make = |event| {
            Buffer::external(
                &mut arena,
                OwnedResource {
                    token: NonNull::new(Box::into_raw(Box::new(count.clone())).cast()).unwrap(),
                    release: count_release,
                    event,
                },
            )
            .unwrap()
        };
        let old = make(old_event.clone());
        let next = make(new_event.clone());
        unsafe {
            Buffer::replace(&mut arena, old.as_ptr(), next.as_ptr()).unwrap();
            assert_eq!(count.get(), 1);
            assert!(!old_event.is_active());
            assert!(new_event.is_active());
            Buffer::release(&mut arena, old.as_ptr()).unwrap();
        }
        assert_eq!(count.get(), 2);
        assert!(!new_event.is_active());
        assert_eq!(arena.bytes(), 0);
    }
    thread_local! { static LAST: std::cell::RefCell<Option<ResourceEvent>> = const { std::cell::RefCell::new(None) }; }
    unsafe extern "C" fn release(pointer: *mut c_void) {
        LAST.with(|last| assert!(!last.borrow().as_ref().unwrap().is_active()));
        drop(Box::from_raw(pointer.cast::<u8>()));
    }
    #[test]
    fn resource_event_is_invalid_before_close_and_identity_never_wraps() {
        let counter = AtomicU64::new(u64::MAX - 1);
        let last = ResourceEvent::with_counter(&counter).unwrap();
        assert_eq!(last.identity(), u64::MAX - 1);
        assert!(ResourceEvent::with_counter(&counter).is_err());
        assert!(ResourceEvent::with_counter(&counter).is_err());
        let event = ResourceEvent::new().unwrap();
        LAST.with(|last| *last.borrow_mut() = Some(event.clone()));
        let resource = OwnedResource {
            token: NonNull::new(Box::into_raw(Box::new(1u8)).cast()).unwrap(),
            release,
            event: event.clone(),
        };
        assert!(event.is_active());
        drop(resource);
        assert!(!event.is_active());
        let next = ResourceEvent::new().unwrap();
        assert_ne!(next.identity(), event.identity());
        assert!(!event.is_active());
    }
}
