//! Scalar Script calls are frame-owned children on the caller's executor.
use super::*;
use crate::runtime::{
    arena::ExternalCharge,
    cps::{self, CpsFrame, DONE, RUNTIME_ERROR},
    executor::{current_executor, current_waker, HostExecutor, TaskWaker},
    suspension::Continuation,
};
use std::{any::Any, cell::Cell, rc::Weak};

pub(super) struct HandleState {
    pub(super) engine: RefCell<VmEngine>,
    session: RefCell<Option<Session>>,
}
struct Session {
    domains: Weak<RefCell<crate::vm::state_domains::Domains>>,
    modules: Vec<Weak<dyn Any>>,
    calls: Vec<Weak<Control>>,
}
struct Control {
    closed: Cell<bool>,
    waker: TaskWaker,
}
struct Call {
    task: Continuation,
    control: Rc<Control>,
    _charge: ExternalCharge,
}
impl HandleState {
    pub(super) fn new(mode: VmMode) -> Self {
        Self {
            engine: RefCell::new(VmEngine::with_mode(mode)),
            session: RefCell::new(None),
        }
    }
    pub(super) fn remember(&self, engine: &VmEngine) {
        if let Some(session) = self.session.borrow_mut().as_mut() {
            session.modules.retain(|m| m.strong_count() != 0);
            if let Some(owner) = engine.script_code_owner() {
                if !session
                    .modules
                    .iter()
                    .any(|m| m.ptr_eq(&Rc::downgrade(&owner)))
                {
                    session.modules.push(Rc::downgrade(&owner));
                }
            }
        }
    }
    fn bind(&self, executor: &HostExecutor) -> Result<(), Error> {
        let mut context = self.session.borrow_mut();
        if let Some(session) = context.as_ref() {
            if !session
                .domains
                .upgrade()
                .is_some_and(|d| Rc::ptr_eq(&d, &executor.domains()))
            {
                return Err(runtime_error(
                    ErrorCode::InvalidState,
                    "Script handle belongs to another or closed executor",
                ));
            }
        } else {
            *context = Some(Session {
                domains: Rc::downgrade(&executor.domains()),
                modules: Vec::new(),
                calls: Vec::new(),
            });
        }
        Ok(())
    }
    pub(super) fn close(&self) {
        if let Some(session) = self.session.borrow().as_ref() {
            for call in session.calls.iter().filter_map(Weak::upgrade) {
                // A call may close its own VM while being polled. Invalidate now;
                // its frame disposes the child when control next returns to it.
                call.closed.set(true);
                call.waker.wake();
            }
        }
    }
}

#[export_name = "joky_vm_publish_domain_code"]
pub(crate) extern "C" fn publish_domain_code(handle: u64) -> i32 {
    operation(-1, || {
        let executor = current_executor().ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidState,
                "Script publication requires an active executor",
            )
        })?;
        let state = lookup(handle)?;
        state.bind(&executor)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        state.remember(&engine);
        let owners: Vec<_> = state
            .session
            .borrow()
            .as_ref()
            .unwrap()
            .modules
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let count = engine.publish_script_code(&executor, &owners)?;
        // Domain limits bound the count well below Int32::MAX.
        Ok(count as i32)
    })
}

unsafe extern "C" fn stop(frame: *mut CpsFrame, _: u32) {
    let context = std::mem::replace(&mut (*frame).host_context, std::ptr::null_mut());
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Call>()));
    }
    (*frame).host_stop = None;
    (*frame).wake = None;
}

unsafe fn poll(frame: &mut CpsFrame) -> Result<i32, Error> {
    if frame.state == 0 {
        let executor = current_executor().ok_or_else(|| {
            runtime_error(
                ErrorCode::InvalidState,
                "Script call requires an active executor",
            )
        })?;
        let state = lookup(frame.slots[0])?;
        state.bind(&executor)?;
        let engine = state.engine.try_borrow().map_err(|_| {
            runtime_error(ErrorCode::InvalidState, "VM is executing another operation")
        })?;
        let name = borrow(frame.slots[1] as *const u8)?;
        let task = engine.prepare_script_int32(name, frame.slots[2] as i32)?;
        state.remember(&engine);
        let control = Rc::new(Control {
            closed: Cell::new(false),
            waker: current_waker().ok_or_else(|| {
                runtime_error(ErrorCode::InvalidState, "Script call requires a task waker")
            })?,
        });
        let charge = (&mut *cps::arena(frame)).reserve_external(std::mem::size_of::<Call>())?;
        let mut session = state.session.borrow_mut();
        let session = session.as_mut().unwrap();
        session.calls.retain(|c| c.strong_count() != 0);
        session.calls.try_reserve(1).map_err(|_| {
            runtime_error(
                ErrorCode::ResourceLimit,
                "Script call registration allocation failed",
            )
        })?;
        session.calls.push(Rc::downgrade(&control));
        frame.host_context = Box::into_raw(Box::new(Call {
            task,
            control,
            _charge: charge,
        }))
        .cast();
        frame.host_stop = Some(stop);
        frame.state = 1;
    }
    if frame.state != 1 || frame.host_context.is_null() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Invalid Script call frame",
        ));
    }
    let call = &mut *frame.host_context.cast::<Call>();
    if call.control.closed.get() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Script VM closed during call",
        ));
    }
    call.task.resume().map_err(Error::from)?;
    if call.control.closed.get() {
        return Err(runtime_error(
            ErrorCode::InvalidState,
            "Script VM closed during call",
        ));
    }
    if let Some(value) = call.task.cps_result() {
        // Dispose child locals and callbacks before publishing its scalar result.
        crate::runtime::error::checked(|| {
            stop(frame, crate::runtime::borrowed_host::STOP_COMPLETED)
        })?;
        frame.result = value;
        frame.state = DONE;
        Ok(0)
    } else {
        frame.wake = call.task.wait_plan().wake();
        Ok(1)
    }
}

/// # Safety
/// Three live slots contain a same-thread VM handle, a JokyText name descriptor,
/// and Int32 input bits. The frame remains exclusive through completion/stop.
/// The name is read only on the first poll; no borrowed input pointer is retained.
#[export_name = "joky_vm_call_async_int32_v2"]
pub(crate) unsafe extern "C" fn call_async_int32(frame: *mut CpsFrame) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| poll(&mut *frame))).unwrap_or_else(|_| {
        Err(runtime_error(
            ErrorCode::Panic,
            "Script async call panicked",
        ))
    });
    match result {
        Ok(status) => status,
        Err(error) => {
            crate::runtime::error::record_error(error);
            stop(frame, crate::runtime::borrowed_host::STOP_FAILED);
            RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{executor::TaskGroup, string::JokyText};

    #[test]
    fn script_scalar_bridge_rejects_cross_executor_reuse() {
        for mode in [VmMode::Jit, VmMode::Interpreter] {
            let handle = create_with_mode(mode);
            unsafe {
                assert_eq!(
                    load_source(
                        handle,
                        JokyText::new("fn run(n:Int32) @suspends ->Int32 { n+1 }").as_ptr()
                    ),
                    0
                );
            }
            for attempt in 0..2 {
                let executor = HostExecutor::default();
                let group = TaskGroup::new();
                let name = Rc::new(JokyText::new("run"));
                let mut frame = CpsFrame::new(3);
                frame
                    .slots
                    .copy_from_slice(&[handle, name.as_ptr() as usize as u64, 41]);
                // The descriptor owner is retained through cancellation; bytes are static.
                let task = unsafe { Continuation::from_native(call_async_int32, frame, name) };
                executor.spawn(&group, task).unwrap();
                let report = executor.poll(1).unwrap();
                assert_eq!(report.completed.len(), 1);
                if attempt == 0 {
                    assert_eq!(*report.completed[0].result.as_ref().unwrap(), 42);
                } else {
                    assert_eq!(
                        report.completed[0].result.as_ref().unwrap_err().code(),
                        ErrorCode::InvalidState
                    );
                }
            }
            assert_eq!(close(handle), 0);
        }
    }
}
