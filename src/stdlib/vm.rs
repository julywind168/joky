//! Native implementation of joky/vm. Handles are thread-affine and never reused.
use crate::{runtime::string::borrow, vm::VmEngine};
use std::{
    cell::RefCell,
    collections::HashMap,
    io::Write,
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

thread_local! {
    static VMS: RefCell<HashMap<u64, Rc<RefCell<VmEngine>>>> = RefCell::new(HashMap::new());
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn operation<T>(fallback: T, action: impl FnOnce() -> Result<T, String>) -> T {
    LAST_ERROR.with(|error| *error.borrow_mut() = None);
    let result = catch_unwind(AssertUnwindSafe(action))
        .unwrap_or_else(|_| Err("VM runtime operation panicked".into()));
    match result {
        Ok(value) => value,
        Err(message) => {
            LAST_ERROR.with(|error| *error.borrow_mut() = Some(message));
            fallback
        }
    }
}

fn lookup(handle: u64) -> Result<Rc<RefCell<VmEngine>>, String> {
    VMS.with(|vms| vms.borrow().get(&handle).cloned())
        .ok_or_else(|| format!("Invalid or closed VM handle {handle} on this thread"))
}

#[export_name = "joky_vm_create"]
pub extern "C" fn create() -> u64 {
    operation(0, || {
        let handle = NEXT_HANDLE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "VM handles exhausted")?;
        VMS.with(|vms| {
            vms.borrow_mut()
                .insert(handle, Rc::new(RefCell::new(VmEngine::new())))
        });
        Ok(handle)
    })
}

#[export_name = "joky_vm_close"]
pub extern "C" fn close(handle: u64) -> i32 {
    operation(1, || {
        // Drop outside the table borrow; active calls retain their own Rc.
        let removed = VMS.with(|vms| vms.borrow_mut().remove(&handle));
        removed.ok_or_else(|| format!("Invalid or closed VM handle {handle} on this thread"))?;
        Ok(0)
    })
}

/// # Safety
/// `source` must be a live immutable Joky string for this call, on the handle's thread.
#[export_name = "joky_vm_load_source"]
pub unsafe extern "C" fn load_source(handle: u64, source: *const u8) -> i32 {
    operation(1, || {
        let source = unsafe { borrow(source)? };
        let engine = lookup(handle)?;
        let mut engine = engine
            .try_borrow_mut()
            .map_err(|_| "VM is executing another operation")?;
        engine.load_source(source)?;
        Ok(0)
    })
}

/// # Safety
/// `source` must be a live immutable Joky string for this call, on the handle's thread.
#[export_name = "joky_vm_reload_source"]
pub unsafe extern "C" fn reload_source(handle: u64, source: *const u8) -> i32 {
    unsafe { load_source(handle, source) }
}

fn call(handle: u64, name: &str, args: &[i32]) -> Result<i32, String> {
    let engine = lookup(handle)?;
    let engine = engine
        .try_borrow()
        .map_err(|_| "VM is executing another operation")?;
    match args {
        [] => engine.call_int32_0(name),
        [a] => engine.call_int32_1(name, *a),
        [a, b] => engine.call_int32_2(name, *a, *b),
        _ => unreachable!(),
    }
}

/// # Safety
/// `name` must be a live immutable Joky string for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_0"]
pub unsafe extern "C" fn call_int32_0(handle: u64, name: *const u8) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[]))
}

/// # Safety
/// `name` must be a live immutable Joky string for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_1"]
pub unsafe extern "C" fn call_int32_1(handle: u64, name: *const u8, a: i32) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[a]))
}

/// # Safety
/// `name` must be a live immutable Joky string for this call, on the handle's thread.
#[export_name = "joky_vm_call_int32_2"]
pub unsafe extern "C" fn call_int32_2(handle: u64, name: *const u8, a: i32, b: i32) -> i32 {
    operation(0, || call(handle, unsafe { borrow(name)? }, &[a, b]))
}

#[export_name = "joky_vm_error_code"]
pub extern "C" fn error_code() -> i32 {
    LAST_ERROR.with(|error| i32::from(error.borrow().is_some()))
}

#[export_name = "joky_vm_print_error"]
pub extern "C" fn print_error() {
    LAST_ERROR.with(|error| {
        if let Some(message) = error.borrow().as_ref() {
            let _ = writeln!(std::io::stderr().lock(), "joky/vm: {message}");
        }
    });
}

#[cfg(test)]
mod tests;
