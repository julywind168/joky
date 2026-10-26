//! Bounded host byte storage with an exclusive resource lease across Pending.
use super::{
    arena::{Arena, ExternalCharge},
    buffer::Buffer,
    cps::{CpsFrame, DONE, FAILED},
    resource::{ResourceEvent, ResourceLease},
};
use crate::{Error, ErrorCode, ErrorPhase};
use std::{ffi::c_void, ptr::NonNull};

/// Maximum bytes in one request. Task and instance peak budgets also apply.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ByteIoKind {
    Read = 0,
    Write = 1,
}
/// Poll may access only request-owned bytes and the exclusively leased token.
/// Pending is 1 with count zero, completed is 0 with a bounded byte count.
/// Stop runs once after the first poll and must synchronously revoke all access,
/// join workers and dispose of frame.host_context. It precedes buffer release
/// and receiver reuse. Neither callback may unwind or reenter Joky. Descriptor,
/// callbacks and token implementation must remain live for all retaining tasks.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ResourceIoV1 {
    pub abi_version: u32,
    pub poll: Option<unsafe extern "C" fn(*mut CpsFrame, *mut ByteRequest, *mut usize) -> i32>,
    pub stop: Option<unsafe extern "C" fn(*mut CpsFrame, *mut ByteRequest, u32)>,
}
impl ResourceIoV1 {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.abi_version != 1 || self.poll.is_none() || self.stop.is_none() {
            return Err(invalid("Invalid resource I/O v1 descriptor"));
        }
        Ok(())
    }
}
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Abi, ErrorCode::AbiError, message)
}
pub(crate) fn symbol(name: &str) -> String {
    format!("__joky_resource_io_v1_{name}")
}

/// Opaque in C. Stable host-owned storage, valid until synchronous stop returns.
/// The language buffer and its private control block never cross this ABI.
pub struct ByteRequest {
    bytes: Vec<u8>,
    _charge: ExternalCharge,
    lease: ResourceLease,
    event: ResourceEvent,
    kind: ByteIoKind,
    host: ResourceIoV1,
}
impl ByteRequest {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
    pub fn resource(&self) -> *mut c_void {
        self.lease.token()
    }
    pub fn event(&self) -> ResourceEvent {
        self.event.clone()
    }
    pub fn kind(&self) -> ByteIoKind {
        self.kind
    }
    unsafe fn begin(
        arena: &mut Arena,
        receiver: *mut Buffer,
        input: u64,
        kind: ByteIoKind,
        host: ResourceIoV1,
    ) -> Result<Box<Self>, Error> {
        host.validate()?;
        let receiver = receiver
            .as_mut()
            .ok_or_else(|| invalid("Null resource receiver"))?;
        let source = if kind == ByteIoKind::Write {
            Some(
                (input as usize as *const Buffer)
                    .as_ref()
                    .ok_or_else(|| invalid("Null byte input"))?
                    .bytes()?,
            )
        } else {
            None
        };
        let length = match source {
            Some(bytes) => bytes.len(),
            None => usize::try_from(input).map_err(|_| invalid("Byte request length overflow"))?,
        };
        if length > MAX_REQUEST_BYTES || (kind == ByteIoKind::Read && length == 0) {
            return Err(invalid(
                "Byte request requires a positive read limit and at most 1 MiB",
            ));
        }
        let lease = receiver.resource_mut()?.lease()?;
        let charge = arena.reserve_external(
            length
                .checked_add(std::mem::size_of::<Self>())
                .ok_or_else(|| invalid("Byte request size overflow"))?,
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| invalid("Byte request allocation failed"))?;
        match source {
            Some(source) => bytes.extend_from_slice(source),
            None => bytes.resize(length, 0),
        }
        Ok(Box::new(Self {
            bytes,
            _charge: charge,
            lease,
            event: ResourceEvent::new()?,
            kind,
            host,
        }))
    }
}
impl Drop for ByteRequest {
    fn drop(&mut self) {
        self.event.invalidate();
    }
}

unsafe fn stop_request(frame: *mut CpsFrame, reason: u32) -> Option<Box<ByteRequest>> {
    (*frame).host_stop = None;
    let mut request = (*frame).byte_request.take()?;
    request.event.invalidate();
    // Keep bytes and resource permissions until all external access is revoked.
    let _ = super::error::checked(|| request.host.stop.unwrap()(frame, &mut *request, reason));
    (*frame).host_context = std::ptr::null_mut();
    Some(request)
}
unsafe extern "C" fn stop(frame: *mut CpsFrame, reason: u32) {
    drop(stop_request(frame, reason));
}
pub(crate) enum IoOutput {
    Read(NonNull<Buffer>),
    Write(u64),
}
/// # Safety
/// Compiler-checked arguments identify a live exclusively borrowed Resource and
/// either a bounded integer limit or a uniquely owned Array<UInt8> input.
pub(crate) unsafe fn poll_io(
    arena: &mut Arena,
    frame: &mut CpsFrame,
    host: &ResourceIoV1,
    kind: ByteIoKind,
) -> Result<Option<IoOutput>, Error> {
    use super::borrowed_host::{STOP_COMPLETED, STOP_FAILED};
    if matches!(frame.state, DONE | FAILED) {
        return Err(invalid("Byte request has already terminated"));
    }
    let outcome = (|| {
        if frame.byte_request.is_none() {
            frame.byte_request = Some(ByteRequest::begin(
                arena,
                frame.slots[0] as usize as *mut Buffer,
                frame.slots[1],
                kind,
                *host,
            )?);
        }
        let request = frame.byte_request.as_mut().unwrap();
        // Empty writes are a no-op and do not start a host operation.
        if kind == ByteIoKind::Write && request.bytes.is_empty() {
            frame.byte_request = None;
            return Ok(Some(IoOutput::Write(0)));
        }
        frame.host_stop = Some(stop);
        let request = &mut **request as *mut ByteRequest;
        let mut count = 0;
        let status = super::error::checked(|| host.poll.unwrap()(frame, request, &mut count));
        if matches!(status, Ok(1)) && count == 0 && !matches!(frame.state, DONE | FAILED) {
            return Ok(None);
        }
        let valid = matches!(status, Ok(0))
            && count <= (*request).bytes.len()
            && (kind == ByteIoKind::Read || count > 0);
        let request =
            stop_request(frame, if valid { STOP_COMPLETED } else { STOP_FAILED }).unwrap();
        status?;
        if !valid {
            return Err(invalid("Byte I/O failed or returned an invalid byte count"));
        }
        // The host snapshot remains charged while the language copy is allocated.
        Ok(Some(match kind {
            ByteIoKind::Read => IoOutput::Read(Buffer::copy_bytes(arena, &request.bytes[..count])?),
            ByteIoKind::Write => IoOutput::Write(count as u64),
        }))
    })();
    match &outcome {
        Ok(Some(_)) => frame.state = DONE,
        Ok(None) => {}
        Err(_) => {
            frame.stop_host(STOP_FAILED);
            frame.byte_request = None;
            frame.state = FAILED;
        }
    }
    outcome
}
#[export_name = "__joky_resource_io_poll_v1"]
unsafe extern "C" fn poll_native(
    frame: *mut CpsFrame,
    host: *const ResourceIoV1,
    kind: u32,
) -> i32 {
    let kind = match kind {
        0 => ByteIoKind::Read,
        1 => ByteIoKind::Write,
        _ => {
            super::error::record_error(invalid("Invalid byte I/O kind"));
            return super::cps::RUNTIME_ERROR;
        }
    };
    match poll_io(&mut *super::cps::arena(frame), &mut *frame, &*host, kind) {
        Ok(Some(IoOutput::Read(owner))) => {
            (&mut (*frame).slots)[2] = owner.as_ptr() as usize as u64;
            0
        }
        Ok(Some(IoOutput::Write(count))) => {
            (*frame).result = count;
            0
        }
        Ok(None) => 1,
        Err(error) => {
            super::error::record_error(error);
            super::cps::RUNTIME_ERROR
        }
    }
}
/// # Safety
/// request is a live ByteRequest supplied to this operation's poll or stop.
#[no_mangle]
pub unsafe extern "C" fn joky_byte_request_data_v1(request: *mut ByteRequest) -> *mut u8 {
    (*request).bytes.as_mut_ptr()
}
/// # Safety
/// request is live for this operation.
#[no_mangle]
pub unsafe extern "C" fn joky_byte_request_length_v1(request: *const ByteRequest) -> u64 {
    (*request).bytes.len() as u64
}
/// # Safety
/// request is live; the token may only be used under its exclusive permission.
#[no_mangle]
pub unsafe extern "C" fn joky_byte_request_resource_v1(request: *const ByteRequest) -> *mut c_void {
    (*request).resource()
}
pub(crate) fn symbols() -> [(&'static str, *const u8); 1] {
    [("__joky_resource_io_poll_v1", poll_native as *const u8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::resource::{invoke, ResourceFactoryV1};
    use std::cell::Cell;
    thread_local! {static STOPS:Cell<u32>=const {Cell::new(0)};}
    unsafe extern "C" fn create(_: u64, output: *mut *mut c_void) -> i32 {
        *output = Box::into_raw(Box::new(42u8)).cast();
        0
    }
    unsafe extern "C" fn release(token: *mut c_void) {
        assert_eq!(*Box::from_raw(token.cast::<u8>()), 42);
    }
    unsafe extern "C" fn pending(
        frame: *mut CpsFrame,
        request: *mut ByteRequest,
        count: *mut usize,
    ) -> i32 {
        assert_eq!(*(*request).resource().cast::<u8>(), 42);
        if (*frame).state == 0 {
            (*frame).state = 1;
            return 1;
        }
        (*request).bytes_mut().fill(255);
        *count = (*request).bytes().len();
        0
    }
    unsafe extern "C" fn stop(_: *mut CpsFrame, request: *mut ByteRequest, _: u32) {
        assert!(!(*request).event().is_active());
        STOPS.with(|n| n.set(n.get() + 1));
    }
    fn factory() -> ResourceFactoryV1 {
        ResourceFactoryV1 {
            abi_version: 1,
            create: Some(create),
            release: Some(release),
        }
    }
    #[test]
    fn resource_bytes_peak_budget_refunds_and_runtime_lease_rejects_overlap() {
        STOPS.with(|n| n.set(0));
        let budget = std::sync::Arc::new(super::super::arena::Budget {
            limit: (4096).into(),
            ..Default::default()
        });
        let mut arena = Arena::with_budget(budget.clone(), None);
        let owner = unsafe { invoke(&mut arena, &factory(), 0) }.unwrap();
        let base = arena.bytes();
        let host = ResourceIoV1 {
            abi_version: 1,
            poll: Some(pending),
            stop: Some(stop),
        };
        let mut first = CpsFrame::new(2);
        first.slots = [owner.as_ptr() as usize as u64, 1024].to_vec();
        assert!(
            unsafe { poll_io(&mut arena, &mut first, &host, ByteIoKind::Read) }
                .unwrap()
                .is_none()
        );
        let peak = arena.bytes();
        assert!(peak >= base + 1024);
        assert_eq!(budget.bytes.get(), peak);
        let mut second = CpsFrame::new(2);
        second.slots = first.slots.clone();
        assert!(unsafe { poll_io(&mut arena, &mut second, &host, ByteIoKind::Read) }.is_err());
        assert_eq!(arena.bytes(), peak);
        // Copy-out must charge the host snapshot and language output together.
        budget.limit.set(peak);
        assert!(unsafe { poll_io(&mut arena, &mut first, &host, ByteIoKind::Read) }.is_err());
        assert_eq!(arena.bytes(), base);
        assert_eq!(budget.bytes.get(), base);
        STOPS.with(|n| assert_eq!(n.get(), 1));
        budget.limit.set(4096);
        let mut retry = CpsFrame::new(2);
        retry.slots = first.slots.clone();
        assert!(
            unsafe { poll_io(&mut arena, &mut retry, &host, ByteIoKind::Read) }
                .unwrap()
                .is_none()
        );
        drop(retry);
        assert_eq!(arena.bytes(), base);
        unsafe {
            Buffer::release(&mut arena, owner.as_ptr()).unwrap();
        }
        assert_eq!(budget.bytes.get(), 0);
        assert_eq!(arena.bytes(), 0);
        STOPS.with(|n| assert_eq!(n.get(), 2));
    }
}
