//! Frame-owned adapters for checked domain turns. No receiver crosses Pending.
use crate::{
    runtime::{
        cps::{CpsFrame, DONE, RUNTIME_ERROR},
        error, string,
    },
    vm::{queue_current_i32_call, DomainOutcome, DomainRequest, InstanceValue},
    Error, ErrorCode, ErrorPhase,
};

unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let frame = &mut *frame;
    let request = frame.host_context.cast::<DomainRequest>();
    frame.host_context = std::ptr::null_mut();
    frame.host_stop = None;
    if !request.is_null() {
        drop(Box::from_raw(request));
    }
}
fn invalid(message: &str) -> Error {
    Error::new_with_code(ErrorPhase::Runtime, ErrorCode::InvalidState, message)
}
unsafe fn poll(frame: *mut CpsFrame, write: bool) -> i32 {
    let result = (|| {
        let f = &mut *frame;
        if f.state == 0 {
            let method = string::borrow(f.slots[0] as *const u8)?;
            let args = if write {
                vec![InstanceValue::Int32(f.slots[1] as i32)]
            } else {
                Vec::new()
            };
            let request = queue_current_i32_call(method, &args, write)?;
            request.register_current_wait()?;
            f.host_context = Box::into_raw(Box::new(request)).cast();
            f.host_stop = Some(stop);
            f.state = 1;
            f.wake = None; // External TaskWaker; no polling of an unready request.
            return Ok(1);
        }
        if f.state != 1 || f.host_context.is_null() {
            return Err(invalid("Invalid domain wait state"));
        }
        let request = &mut *f.host_context.cast::<DomainRequest>();
        let Some(result) = request.take_result() else {
            request.register_current_wait()?;
            return Ok(1);
        };
        stop(frame, 0);
        match result {
            DomainOutcome::Value(InstanceValue::Int32(value)) => {
                (*frame).result = value as u64;
                (*frame).state = DONE;
                Ok(0)
            }
            DomainOutcome::Failed(error) => Err(error),
            DomainOutcome::Cancelled => Err(invalid("Domain operation was cancelled")),
            DomainOutcome::Closed => Err(invalid("Domain operation owner is closed")),
            _ => Err(invalid("Domain operation did not return Int32")),
        }
    })();
    match result {
        Ok(status) => status,
        Err(failure) => {
            stop(frame, 0);
            error::record_error(failure);
            RUNTIME_ERROR
        }
    }
}
// Called only through the checked TextViewV2 CPS adapter, with one/two slots.
// The adapter keeps the descriptor alive; only its copied method name is queued.
#[export_name = "__joky_domain_read_i32"]
pub(crate) unsafe extern "C" fn read(frame: *mut CpsFrame) -> i32 {
    poll(frame, false)
}
#[export_name = "__joky_domain_write_i32"]
pub(crate) unsafe extern "C" fn write(frame: *mut CpsFrame) -> i32 {
    poll(frame, true)
}
